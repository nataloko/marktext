// Finding the programs the Image settings run (src/uploader.rs), as Electron's
// resolveCommand does (packages/desktop/src/main/utils/resolveCommand.ts and
// main/app/envPath.ts): on PATH, then in the folders package managers install
// programs to, which a desktop launch may leave off PATH, then on the PATH the
// user's login shell sets up, where version managers such as nvm put theirs.
// `mt::cmd::exists` answers the Image page's PicGo check the same way.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::fileops::is_executable;

// Asking the login shell can take seconds, so not on the main thread.
#[tauri::command]
pub async fn cmd_exists(name: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || resolve(&name).is_some()).await.unwrap_or(false)
}

// The login shell is asked only when the other places miss.
pub fn resolve(name: &str) -> Option<PathBuf> {
    find_in(name, process_path().into_iter().chain(extra_dirs())).or_else(|| find_in(name, login_shell_path()?))
}

// The PATH a program found here runs with: the login shell's folders first,
// as Electron puts them first on its own PATH, then this process's, then the
// package managers' folders.
pub fn child_path() -> Option<OsString> {
    let mut dirs = login_shell_path().unwrap_or_default();
    for dir in process_path().into_iter().chain(extra_dirs()) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).ok()
}

fn process_path() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default()
}

// The name comes from the page. An empty name or one with a folder in it would
// find files outside the folders searched.
fn is_command_name(name: &str) -> bool {
    !name.trim().is_empty() && !name.contains('/') && !name.contains(std::path::MAIN_SEPARATOR)
}

fn find_in(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    if !is_command_name(name) {
        return None;
    }
    dirs.into_iter()
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| candidates(&dir, name))
        .find(|candidate| is_executable(candidate))
}

// Windows runs a name with each of PATHEXT's extensions added, and a name that
// already ends in one of them as it is.
#[cfg(windows)]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let extensions: Vec<&str> = extensions.split(';').filter(|ext| !ext.is_empty()).collect();
    let typed = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| extensions.iter().any(|known| known.trim_start_matches('.').eq_ignore_ascii_case(ext)));
    let mut list: Vec<PathBuf> = extensions.iter().map(|ext| dir.join(format!("{name}{ext}"))).collect();
    if typed {
        list.insert(0, dir.join(name));
    }
    list
}

#[cfg(not(windows))]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![dir.join(name)]
}

// envPath.ts: Windows starts a desktop app with the user's own PATH.
#[cfg(windows)]
fn login_shell_path() -> Option<Vec<PathBuf>> {
    None
}

// envPath.ts ensureShellEnvPath: the shell is asked once per run; a read that
// failed is tried again by the next caller, up to three times, since a shell
// that timed out on a busy machine may answer later. Callers wait for one
// another rather than start a shell each.
#[cfg(unix)]
fn login_shell_path() -> Option<Vec<PathBuf>> {
    use std::sync::Mutex;

    static READ: Mutex<(Option<Vec<PathBuf>>, u32)> = Mutex::new((None, 0));
    let mut read = READ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(dirs) = &read.0 {
        return Some(dirs.clone());
    }
    if read.1 >= 3 {
        return None;
    }
    read.1 += 1;
    let dirs = shell::read_path()?;
    read.0 = Some(dirs.clone());
    Some(dirs)
}

// envPath.ts readLoginShellPath and parseShellPath.
#[cfg(unix)]
mod shell {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    // `printenv` rather than `echo $PATH`, which fish prints with spaces. The
    // markers fence the value off from whatever start up files print, and the
    // end marker shows the output was not cut short.
    pub(super) const BEGIN: &str = "__MARKTEXT_PATH_BEGIN__";
    pub(super) const END: &str = "__MARKTEXT_PATH_END__";
    const TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_OUTPUT: usize = 8 * 1024 * 1024;

    fn login_shell() -> Option<PathBuf> {
        let shell = PathBuf::from(std::env::var_os("SHELL")?);
        let name = shell.file_name()?.to_string_lossy().into_owned();
        (shell.is_absolute() && name != "nologin" && name != "false").then_some(shell)
    }

    // An interactive login shell, in a process group of its own so that the
    // timeout also ends what its start up files left running; stdin is closed
    // so that it does not wait for input.
    pub(super) fn read_path() -> Option<Vec<PathBuf>> {
        let shell = login_shell()?;
        let mut child = Command::new(&shell)
            .args(["-ilc", &format!("echo {BEGIN}; printenv PATH; echo {END}")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .inspect_err(|err| eprintln!("[cmd] {}: {err}", shell.display()))
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let (sender, received) = std::sync::mpsc::channel();
        // A job the start up files put in the background keeps the pipe open,
        // so reading stops at the end marker rather than at the end.
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut chunk = [0u8; 4096];
            while let Ok(read @ 1..) = stdout.read(&mut chunk) {
                if output.len() < MAX_OUTPUT {
                    output.extend_from_slice(&chunk[..read]);
                }
                if String::from_utf8_lossy(&output).contains(END) {
                    break;
                }
            }
            let _ = sender.send(output);
        });
        let output = received.recv_timeout(TIMEOUT).ok();
        if let Ok(group) = i32::try_from(child.id()) {
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let _ = child.wait();
        parse(&String::from_utf8_lossy(&output?))
    }

    // None when the output was cut short; an empty list when the shell set no
    // folders.
    pub(super) fn parse(output: &str) -> Option<Vec<PathBuf>> {
        let begin = output.rfind(BEGIN)?;
        let end = output[begin..].find(END)? + begin;
        let fenced = &output[begin + BEGIN.len()..end];
        for line in fenced.lines() {
            let dirs: Vec<PathBuf> =
                line.trim().split(':').map(PathBuf::from).filter(|dir| Path::is_absolute(dir)).collect();
            if !dirs.is_empty() {
                return Some(dirs);
            }
        }
        Some(Vec::new())
    }
}

// envPath.ts extraPathDirs, without macOS.
#[cfg(windows)]
fn extra_dirs() -> Vec<PathBuf> {
    let under = |var: &str, rest: &[&str]| {
        std::env::var_os(var).map(|base| rest.iter().fold(PathBuf::from(base), |path, part| path.join(part)))
    };
    [
        under("ProgramData", &["chocolatey", "bin"]),
        under("USERPROFILE", &["scoop", "shims"]),
        under("LOCALAPPDATA", &["Microsoft", "WinGet", "Links"]),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(not(windows))]
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        ["/usr/local/bin", "/usr/local/lib/node_modules/.bin", "/usr/bin", "/bin"].iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for user_dir in [".local/share/pnpm", ".local/bin", ".volta/bin", ".bun/bin", ".yarn/bin", ".npm-global/bin", ".npm/bin"] {
            dirs.push(home.join(user_dir));
        }
    }
    if let Some(prefix) = std::env::var_os("npm_config_prefix") {
        dirs.push(PathBuf::from(prefix).join("bin"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-cmd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A program file named as the system would run it.
    fn install(dir: &Path, name: &str) {
        let file = if cfg!(windows) { dir.join(format!("{name}.exe")) } else { dir.join(name) };
        std::fs::write(&file, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn finds_a_program_in_the_folders_given() {
        let dir = temp_dir("found");
        install(&dir, "tool");
        assert!(find_in("tool", [PathBuf::from("relative"), dir.clone()]).is_some());
        assert!(find_in("missing", [dir.clone()]).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn skips_files_that_cannot_run() {
        let dir = temp_dir("plain");
        std::fs::write(dir.join("notes"), b"").unwrap();
        assert!(find_in("notes", [dir.clone()]).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_login_shell_path_is_read_between_the_markers() {
        let output = format!("motd\n{}\n/usr/bin:relative:/home/me/.nvm/bin\n{}\nbye", shell::BEGIN, shell::END);
        assert_eq!(shell::parse(&output), Some(vec![PathBuf::from("/usr/bin"), PathBuf::from("/home/me/.nvm/bin")]));
        assert_eq!(shell::parse(&format!("{}\n/usr/bin", shell::BEGIN)), None);
        assert_eq!(shell::parse("no markers"), None);
        assert_eq!(shell::parse(&format!("{}\n\n{}", shell::BEGIN, shell::END)), Some(Vec::new()));
    }

    #[test]
    fn names_with_folders_are_refused() {
        let dir = temp_dir("names");
        let inner = dir.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        install(&inner, "tool");
        assert!(find_in("inner/tool", [dir.clone()]).is_none());
        assert!(find_in(&format!("inner{}tool", std::path::MAIN_SEPARATOR), [dir.clone()]).is_none());
        assert!(find_in("", [dir.clone()]).is_none());
        assert!(find_in("  ", [dir.clone()]).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
