#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Manager};

mod encoding;
mod preferences;
mod recent;
mod watch;
mod windows;

// packages/desktop/src/common/filesystem/paths.ts MARKDOWN_EXTENSIONS.
const MARKDOWN_EXTENSIONS: [&str; 11] = [
    "markdown", "mdown", "mkdn", "md", "mkd", "mdwn", "mdtxt", "mdtext", "mdx", "text", "txt",
];

// Same allow list as packages/desktop/src/main/ipc/bootInfo.ts.
const ENV_ALLOWLIST: [&str; 4] = ["APPIMAGE", "MARKTEXT_RIPGREP_PATH", "PATH", "HOME"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootPaths {
    resources: String,
    user_data: String,
    cwd: String,
    ripgrep_binary: String,
}

// The `BootInfo` type in packages/desktop/src/shared/types/ipc.ts, plus the
// window's id and the files to open, which Electron's main process would
// send as tabs instead.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootInfo {
    platform: &'static str,
    arch: &'static str,
    versions: HashMap<&'static str, String>,
    env: HashMap<String, String>,
    paths: BootPaths,
    is_updatable: bool,
    #[serde(rename = "MARKDOWN_INCLUSIONS")]
    markdown_inclusions: Vec<String>,
    window_id: u32,
    open_files: Vec<String>,
}

// Electron's BrowserWindow ids start at 1 and are never reused.
static NEXT_WINDOW_ID: AtomicU32 = AtomicU32::new(1);

// The renderer compares against Node's `process.platform` and `process.arch` names.
fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| MARKDOWN_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

// Node's path.resolve: absolute, without "." and ".." parts, links kept.
fn resolve_lexically(base: &Path, path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

// packages/desktop/src/main/filesystem/index.ts normalizeAndResolvePath: a
// link is replaced by its target; other paths keep the folders they were
// given through, so they match what the open dialog returns.
fn resolve_cli_path(cwd: &Path, arg: &str) -> PathBuf {
    let path = resolve_lexically(cwd, Path::new(arg));
    match std::fs::read_link(&path) {
        Ok(target) => resolve_lexically(path.parent().unwrap_or(&path), &target),
        Err(_) => path,
    }
}

// Markdown files named on a command line, resolved against `cwd`
// (packages/desktop/src/main/filesystem/markdown.ts normalizeCliMarkdownPath).
// Files that do not exist yet are passed on when their folder exists; folders
// are skipped until the sidebar is ported.
fn markdown_files(cwd: &Path, args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .map(|arg| resolve_cli_path(cwd, arg))
        .filter(|path| {
            is_markdown(path)
                && (path.is_file() || (!path.exists() && path.parent().is_some_and(Path::is_dir)))
        })
        .map(|path| display(&path))
        .collect()
}

fn boot_info(app: &AppHandle, user_data: &Path, window_id: u32, open_files: Vec<String>) -> BootInfo {
    let cwd = std::env::current_dir().unwrap_or_default();
    let version = env!("CARGO_PKG_VERSION").to_string();

    let mut env: HashMap<String, String> = ENV_ALLOWLIST
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_string(), value)))
        .collect();
    if !env.contains_key("HOME") {
        if let Ok(home) = app.path().home_dir() {
            env.insert("HOME".into(), display(&home));
        }
    }
    let node_env = if cfg!(debug_assertions) { "development" } else { "production" };
    env.insert("NODE_ENV".into(), node_env.into());
    env.insert("MARKTEXT_VERSION".into(), version.clone());
    env.insert("MARKTEXT_VERSION_STRING".into(), format!("v{version} (Tauri trial)"));

    BootInfo {
        platform: node_platform(),
        arch: node_arch(),
        versions: HashMap::from([("tauri", tauri::VERSION.to_string())]),
        env,
        paths: BootPaths {
            resources: app.path().resource_dir().map(|p| display(&p)).unwrap_or_default(),
            user_data: display(user_data),
            cwd: display(&cwd),
            ripgrep_binary: String::new(),
        },
        is_updatable: false,
        markdown_inclusions: MARKDOWN_EXTENSIONS.iter().map(|ext| format!("*.{ext}")).collect(),
        window_id,
        open_files,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadedText {
    text: String,
    encoding: String,
    is_bom: bool,
}

// packages/desktop/src/main/filesystem/markdown.ts loadMarkdownFile: read,
// detect the encoding, decode. The page handles line endings and the trailing
// newline (src/markdown.ts).
#[tauri::command]
fn read_markdown_file(
    watcher: tauri::State<'_, watch::FileWatcher>,
    path: String,
    auto_guess_encoding: bool,
) -> Result<LoadedText, String> {
    let bytes = std::fs::read(&path).map_err(|err| format!("{path}: {err}"))?;
    watcher.remember(&path, &bytes);
    let (encoding, is_bom) = encoding::guess(&bytes, auto_guess_encoding);
    let text = encoding::decode(&bytes, &encoding, is_bom).map_err(|err| format!("{path}: {err}"))?;
    Ok(LoadedText { text, encoding, is_bom })
}

// markdown.ts writeMarkdownFile after the line endings are applied.
#[tauri::command]
fn write_markdown_file(
    watcher: tauri::State<'_, watch::FileWatcher>,
    path: String,
    text: String,
    encoding: String,
    is_bom: bool,
) -> Result<(), String> {
    let bytes = encoding::encode(&text, &encoding, is_bom).map_err(|err| format!("{path}: {err}"))?;
    write_atomic(Path::new(&path), &bytes).map_err(|err| format!("{path}: {err}"))?;
    watcher.remember(&path, &bytes);
    Ok(())
}

// Writes beside the target and renames over it, so a failed write never
// leaves a truncated file behind. As write-file-atomic does for Electron, a
// link is written through to its target and the target keeps its permissions.
fn write_atomic(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let target = match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(target)?,
        _ => target.to_path_buf(),
    };
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "not a file path"))?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = target.with_file_name(format!(".{}.marktext-tmp", name.to_string_lossy()));
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        if let Ok(meta) = std::fs::metadata(&target) {
            std::fs::set_permissions(&temp, meta.permissions())?;
        }
        rename_with_retry(&temp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

// Windows can hold a file for a moment after a watcher looked at it, notably
// on network drives (packages/desktop/src/main/filesystem/index.ts writeFile, #5322).
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    for delay in [50, 75, 50] {
        match std::fs::rename(from, to) {
            Err(err) if matches!(err.kind(), ErrorKind::PermissionDenied | ErrorKind::ResourceBusy) => {
                std::thread::sleep(Duration::from_millis(delay))
            }
            result => return result,
        }
    }
    std::fs::rename(from, to)
}

#[tauri::command]
fn path_exists(path: String) -> bool {
    Path::new(&path).exists()
}

#[tauri::command]
fn toggle_devtools(window: tauri::WebviewWindow) {
    #[cfg(debug_assertions)]
    if window.is_devtools_open() {
        window.close_devtools();
    } else {
        window.open_devtools();
    }
    #[cfg(not(debug_assertions))]
    let _ = window;
}

// One editor window with its own boot info (packages/desktop/src/main/windows/editor.ts).
pub(crate) fn open_editor_window(app: &AppHandle, open_files: Vec<String>) -> tauri::Result<()> {
    let user_data = app.path().app_data_dir()?;
    std::fs::create_dir_all(&user_data)?;
    let window_id = NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed);
    let script = format!(
        "window.__MT_BOOT__ = {};",
        serde_json::to_string(&boot_info(app, &user_data, window_id, open_files))?
    );

    // No system title bar: the renderer draws its own (titleBarStyle "custom").
    // The native drag and drop handler would swallow HTML drop events
    // that the editor relies on.
    let registry = app.state::<windows::Windows>();
    registry.add(window_id);
    let window = tauri::WebviewWindowBuilder::new(app, windows::label(window_id), tauri::WebviewUrl::default())
        .title("MarkText")
        .inner_size(1200.0, 800.0)
        .min_inner_size(400.0, 300.0)
        .decorations(false)
        .disable_drag_drop_handler()
        .initialization_script(script)
        .build()
        .inspect_err(|_| registry.remove(window_id))?;

    // Debug builds print the page console, including script load
    // errors, to the terminal.
    #[cfg(all(debug_assertions, target_os = "linux"))]
    window.with_webview(|webview| {
        use webkit2gtk::{SettingsExt, WebViewExt};
        if let Some(settings) = webview.inner().settings() {
            settings.set_enable_write_console_messages_to_stdout(true);
        }
    })?;
    #[cfg(not(all(debug_assertions, target_os = "linux")))]
    let _ = window;
    Ok(())
}

// Electron's webContents.cut/copy/paste for the Edit menu: the page gets the
// same clipboard events as from the keyboard shortcuts.
#[tauri::command]
fn editing_command(window: tauri::WebviewWindow, command: String) -> Result<(), String> {
    let command = match command.as_str() {
        "cut" => "Cut",
        "copy" => "Copy",
        "paste" => "Paste",
        other => return Err(format!("unknown editing command {other}")),
    };
    #[cfg(target_os = "linux")]
    return window
        .with_webview(move |webview| {
            use webkit2gtk::WebViewExt;
            webview.inner().execute_editing_command(command);
        })
        .map_err(|err| err.to_string());
    // Untested: WebView2 may refuse a paste that no key press started.
    #[cfg(not(target_os = "linux"))]
    return window
        .eval(format!("document.execCommand('{}')", command.to_lowercase()))
        .map_err(|err| err.to_string());
}

// Electron's app.quit: every window gets a close request, so each one asks
// about its own unsaved documents.
#[tauri::command]
fn quit_app(app: AppHandle) {
    for window in app.webview_windows().values() {
        let _ = window.close();
    }
}

// Async because building a window inside a synchronous command deadlocks on Windows.
#[tauri::command]
async fn new_editor_window(app: AppHandle, files: Vec<String>) -> Result<(), String> {
    open_editor_window(&app, files).map_err(|err| err.to_string())
}

fn main() {
    tauri::Builder::default()
        // Must come first, so a second launch exits before doing anything else.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            windows::second_launch(app, argv, cwd)
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            read_markdown_file,
            write_markdown_file,
            path_exists,
            toggle_devtools,
            new_editor_window,
            editing_command,
            quit_app,
            preferences::get_preferences,
            preferences::set_preferences,
            windows::window_add_file,
            windows::window_change_file,
            windows::window_remove_file,
            windows::window_ready,
            recent::recent_files,
            recent::add_recent_file,
            recent::clear_recent_files
        ])
        .manage(windows::Windows::default())
        .on_window_event(|window, event| {
            let Some(id) = windows::id_from_label(window.label()) else { return };
            match event {
                tauri::WindowEvent::Focused(true) => window.state::<windows::Windows>().focus(id),
                tauri::WindowEvent::Destroyed => {
                    window.state::<windows::Windows>().remove(id);
                    window.state::<watch::FileWatcher>().unwatch_window(id);
                }
                _ => {}
            }
        })
        .setup(|app| {
            let user_data = app.path().app_data_dir()?;
            app.manage(preferences::Preferences::load(user_data.join("preferences.json")));
            app.manage(recent::RecentFiles::load(user_data.join("recently-used-documents.json")));
            app.manage(watch::FileWatcher::start(app.handle()));
            let cwd = std::env::current_dir().unwrap_or_default();
            let args: Vec<String> = std::env::args().skip(1).collect();
            open_editor_window(app.handle(), markdown_files(&cwd, &args))?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running MarkText");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn markdown_files_keeps_markdown_and_new_files() {
        let dir = temp_dir("filter");
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        std::fs::write(dir.join("notes.MD"), "").unwrap();
        std::fs::write(dir.join("image.png"), "").unwrap();

        let given = args(&["--new-window", "notes.MD", "image.png", "folder", "new.md", "gone/new.md", "folder/../x.md"]);
        assert_eq!(
            markdown_files(&dir, &given),
            vec![display(&dir.join("notes.MD")), display(&dir.join("new.md")), display(&dir.join("x.md"))]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_resolves_to_its_target() {
        let dir = temp_dir("link");
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        std::fs::write(dir.join("elsewhere/target.md"), "").unwrap();
        std::os::unix::fs::symlink("elsewhere/target.md", dir.join("link.md")).unwrap();
        assert_eq!(markdown_files(&dir, &args(&["link.md"])), vec![display(&dir.join("elsewhere/target.md"))]);
    }
}
