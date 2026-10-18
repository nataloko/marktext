// `mt::cmd::exists`: whether a program is installed, looked up as Electron's
// resolveCommand does (packages/desktop/src/main/utils/resolveCommand.ts and
// main/app/envPath.ts): on PATH, then in the folders package managers install
// programs to, which a desktop launch may leave off PATH. Electron then also
// asks the user's login shell for its PATH; that is left for the image
// uploader, which runs the program it finds.

use std::path::{Path, PathBuf};

use crate::fileops::is_executable;

#[tauri::command]
pub fn cmd_exists(name: String) -> bool {
    let path: Vec<PathBuf> =
        std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default();
    find_in(&name, path.into_iter().chain(extra_dirs())).is_some()
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
