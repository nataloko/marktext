#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Manager};

mod buffers;
mod clipboard;
mod drop;
mod encoding;
mod fileops;
mod folders;
mod preferences;
mod recent;
mod search;
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
// window's id and the files and folder to open, which Electron's main process
// would send to the renderer instead.
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
    // "editor" or "settings" (packages/desktop/src/main/windows/base.ts type).
    window_type: &'static str,
    // The settings page to show first, if any.
    category: Option<String>,
    #[serde(flatten)]
    contents: Contents,
}

// What an editor window opens with: files and a folder, or the session saved
// in its store (src/buffers.rs).
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Contents {
    open_files: Vec<String>,
    open_folder: Option<String>,
    restored: bool,
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

pub(crate) fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn is_markdown(path: &Path) -> bool {
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
pub(crate) fn resolve_cli_path(cwd: &Path, arg: &str) -> PathBuf {
    let path = resolve_lexically(cwd, Path::new(arg));
    match std::fs::read_link(&path) {
        Ok(target) => resolve_lexically(path.parent().unwrap_or(&path), &target),
        Err(_) => path,
    }
}

// The markdown files and the folders named on a command line, resolved
// against `cwd` (packages/desktop/src/main/filesystem/markdown.ts
// normalizeCliMarkdownPath). Files that do not exist yet are passed on when
// their folder exists.
fn cli_paths(cwd: &Path, args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut folders = Vec::new();
    for path in args.iter().filter(|arg| !arg.starts_with('-')).map(|arg| resolve_cli_path(cwd, arg)) {
        if path.is_dir() {
            folders.push(display(&path));
        } else if is_markdown(&path) && (path.is_file() || (!path.exists() && path.parent().is_some_and(Path::is_dir))) {
            files.push(display(&path));
        }
    }
    (files, folders)
}

// main/app/index.ts ready: with nothing on the command line, the start up
// action may name a folder to open.
fn start_up_folder(preferences: &preferences::Preferences) -> Option<String> {
    let key = match preferences.get("startUpAction")?.as_str()? {
        "folder" => "defaultDirectoryToOpen",
        "openLastFolder" => "lastOpenedFolder",
        _ => return None,
    };
    let folder = preferences.get(key)?.as_str()?.to_string();
    (!folder.is_empty() && Path::new(&folder).is_dir()).then_some(folder)
}

// main/app/index.ts ready and createWindow: the sessions to restore, when the
// start up action restores them and nothing else is to open. Otherwise the
// start up folder joins the paths, and when paths open the stores with
// nothing unsaved are forgotten.
fn start_up(store_dir: &Path, preferences: &preferences::Preferences, files: &[String], folders: &mut Vec<String>) -> Vec<String> {
    if files.is_empty() && folders.is_empty() {
        if preferences.get("startUpAction").as_ref().and_then(|value| value.as_str()) == Some("restoreAll") {
            return buffers::list(store_dir);
        }
        folders.extend(start_up_folder(preferences));
    }
    if !files.is_empty() || !folders.is_empty() {
        buffers::clear_all_saved(store_dir);
    }
    Vec::new()
}

fn boot_info(
    app: &AppHandle,
    user_data: &Path,
    window_id: u32,
    window_type: &'static str,
    category: Option<String>,
    contents: Contents,
) -> BootInfo {
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
        window_type,
        category,
        contents,
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
// newline (src/markdown.ts). Async, like the write below, so that a slow
// drive holds up only this document instead of every window's main thread.
#[tauri::command]
async fn read_markdown_file(
    window: tauri::WebviewWindow,
    watcher: tauri::State<'_, watch::FileWatcher>,
    path: String,
    auto_guess_encoding: bool,
) -> Result<LoadedText, String> {
    let bytes = std::fs::read(&path).map_err(|err| format!("{path}: {err}"))?;
    if let Some(id) = windows::id_from_label(window.label()) {
        watcher.remember(&path, id, &bytes);
    }
    let (encoding, is_bom) = encoding::guess(&bytes, auto_guess_encoding);
    let text = encoding::decode(&bytes, &encoding, is_bom).map_err(|err| format!("{path}: {err}"))?;
    Ok(LoadedText { text, encoding, is_bom })
}

// markdown.ts writeMarkdownFile after the line endings are applied.
#[tauri::command]
async fn write_markdown_file(
    window: tauri::WebviewWindow,
    watcher: tauri::State<'_, watch::FileWatcher>,
    path: String,
    text: String,
    encoding: String,
    is_bom: bool,
) -> Result<(), String> {
    let bytes = encoding::encode(&text, &encoding, is_bom).map_err(|err| format!("{path}: {err}"))?;
    write_atomic(Path::new(&path), &bytes).map_err(|err| format!("{path}: {err}"))?;
    if let Some(id) = windows::id_from_label(window.label()) {
        watcher.remember(&path, id, &bytes);
    }
    Ok(())
}

// The path a document opens under: Electron opens a link as its target
// (packages/desktop/src/main/filesystem/index.ts normalizeAndResolvePath),
// which is also the folder that has to be watched.
#[tauri::command]
fn resolve_path(path: String) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    display(&resolve_cli_path(&cwd, &path))
}

static TEMP_FILES: AtomicU32 = AtomicU32::new(0);

// Writes beside the target and renames over it, so a failed write never
// leaves a truncated file behind. As write-file-atomic does for Electron, a
// link is written through to its target and the target keeps its permissions.
fn write_atomic(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let target = match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(target)
            .map_err(|err| std::io::Error::new(err.kind(), format!("the link points to a missing file ({err})")))?,
        _ => target.to_path_buf(),
    };
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "not a file path"))?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = target.with_file_name(format!(
        ".{}.{}-{}.marktext-tmp",
        name.to_string_lossy(),
        std::process::id(),
        TEMP_FILES.fetch_add(1, Ordering::Relaxed)
    ));
    #[cfg(unix)]
    let permissions = std::fs::metadata(&target).ok().map(|meta| meta.permissions());
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // The temp file has the target's mode from the start, so a private
        // note is never readable by others while it is written.
        #[cfg(unix)]
        if let Some(permissions) = &permissions {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(permissions.mode());
        }
        let mut file = options.open(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        // The umask may have cleared bits that the target had.
        #[cfg(unix)]
        if let Some(permissions) = permissions {
            std::fs::set_permissions(&temp, permissions)?;
        }
        rename_with_retry(&temp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

fn is_sharing_error(err: &std::io::Error) -> bool {
    matches!(err.kind(), ErrorKind::PermissionDenied | ErrorKind::ResourceBusy)
        // Windows ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION, libuv's EBUSY.
        || (cfg!(windows) && matches!(err.raw_os_error(), Some(32 | 33)))
}

// Windows can hold a file for a moment after a watcher, a virus scanner or a
// sync client looked at it, notably on network drives
// (packages/desktop/src/main/filesystem/index.ts writeFile, #5322).
pub(crate) fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    for delay in [50, 75, 50] {
        match std::fs::rename(from, to) {
            Err(err) if is_sharing_error(&err) => std::thread::sleep(Duration::from_millis(delay)),
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
fn is_directory(path: String) -> bool {
    Path::new(&path).is_dir()
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

// The parts every window shares: its boot info, available before any page
// script runs, and no system title bar, since the renderer draws its own
// (titleBarStyle "custom"). The native drag and drop handler would swallow
// HTML drop events that the editor relies on. Only the app's own pages may
// load in a window.
fn window_builder<'a>(
    app: &'a AppHandle,
    label: String,
    window_id: u32,
    window_type: &'static str,
    category: Option<String>,
    contents: Contents,
) -> tauri::Result<tauri::WebviewWindowBuilder<'a, tauri::Wry, AppHandle>> {
    let user_data = app.path().app_data_dir()?;
    std::fs::create_dir_all(&user_data)?;
    let boot = boot_info(app, &user_data, window_id, window_type, category, contents);
    let script = format!("window.__MT_BOOT__ = {};", serde_json::to_string(&boot)?);
    let dev_url = if cfg!(debug_assertions) { app.config().build.dev_url.clone() } else { None };
    Ok(tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::default())
        .title("MarkText")
        .decorations(false)
        .disable_drag_drop_handler()
        .on_navigation(move |url| drop::is_app_page(url, dev_url.as_ref()))
        .initialization_script(script))
}

// Debug builds print the page console, including script load errors, to the terminal.
fn print_console_messages(window: &tauri::WebviewWindow) -> tauri::Result<()> {
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

// One editor window (packages/desktop/src/main/windows/editor.ts).
pub(crate) fn open_editor_window(
    app: &AppHandle,
    open_files: Vec<String>,
    open_folder: Option<String>,
) -> tauri::Result<()> {
    let contents = Contents { open_files, open_folder, restored: false };
    build_editor_window(app, contents, buffers::new_id())
}

// A window for the session kept in the store `buffer_id`.
fn open_restored_window(app: &AppHandle, buffer_id: String) -> tauri::Result<()> {
    build_editor_window(app, Contents { restored: true, ..Contents::default() }, buffer_id)
}

fn build_editor_window(app: &AppHandle, contents: Contents, buffer_id: String) -> tauri::Result<()> {
    let window_id = NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed);
    let label = windows::label(window_id);
    let open_folder = contents.open_folder.clone();
    let builder = window_builder(app, label, window_id, "editor", None, contents)?;
    let registry = app.state::<windows::Windows>();
    registry.add(window_id, open_folder, buffer_id);
    let window = builder
        .inner_size(1200.0, 800.0)
        .min_inner_size(400.0, 300.0)
        .build()
        .inspect_err(|_| registry.remove(window_id))?;
    drop::attach(&window);
    print_console_messages(&window)
}

const SETTINGS_LABEL: &str = "settings";

// packages/desktop/src/main/app/index.ts _openSettingsWindow: there is one
// settings window; opening it again brings it forward on the given page.
// Sizes follow packages/desktop/src/main/config.ts preferencesWinOptions.
#[tauri::command]
async fn open_settings_window(app: AppHandle, category: Option<String>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(SETTINGS_LABEL) {
        let page = category.map_or(serde_json::Value::Null, serde_json::Value::String);
        windows::send_to_renderer(&window, "settings::change-tab", vec![page]);
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let open = || -> tauri::Result<()> {
        let window_id = NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed);
        let window = window_builder(&app, SETTINGS_LABEL.into(), window_id, "settings", category, Contents::default())?
            .inner_size(950.0, 650.0)
            .min_inner_size(450.0, 350.0)
            .minimizable(false)
            .center()
            .build()?;
        print_console_messages(&window)
    };
    open().map_err(|err| err.to_string())
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
    run_editing_command(&window, command).map_err(|err| err.to_string())
}

#[cfg(target_os = "linux")]
fn run_editing_command(window: &tauri::WebviewWindow, command: &'static str) -> tauri::Result<()> {
    window.with_webview(move |webview| {
        use webkit2gtk::WebViewExt;
        webview.inner().execute_editing_command(command);
    })
}

// WebView2 refuses `document.execCommand('paste')` from script. Sent through
// the DevTools protocol, the command counts as the user's own, as Electron's
// webContents.paste() does inside Chromium.
#[cfg(windows)]
fn run_editing_command(window: &tauri::WebviewWindow, command: &'static str) -> tauri::Result<()> {
    window.with_webview(move |webview| unsafe {
        use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
        use windows_core::HSTRING;
        let Ok(core) = webview.controller().CoreWebView2() else { return };
        let params = format!(r#"{{"type":"rawKeyDown","commands":["{}"]}}"#, command.to_lowercase());
        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
        let sent = core.CallDevToolsProtocolMethod(
            &HSTRING::from("Input.dispatchKeyEvent"),
            &HSTRING::from(params),
            &handler,
        );
        if let Err(err) = sent {
            eprintln!("[editing] {command}: {err}");
        }
    })
}

// Electron's app.quit: every window gets a close request, so each one asks
// about its own unsaved documents. Every window keeps its session store
// (src/buffers.rs), until the user keeps one of them open.
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.state::<windows::Windows>().set_quitting(true);
    for window in app.webview_windows().values() {
        let _ = window.close();
    }
}

// Async because building a window inside a synchronous command deadlocks on Windows.
#[tauri::command]
async fn new_editor_window(app: AppHandle, files: Vec<String>, folder: Option<String>) -> Result<(), String> {
    open_editor_window(&app, files, folder).map_err(|err| err.to_string())
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
            resolve_path,
            path_exists,
            is_directory,
            drop::is_markdown_file,
            clipboard::clipboard_read_text,
            clipboard::clipboard_write_text,
            fileops::fs_is_file,
            fileops::fs_is_directory,
            fileops::fs_is_executable,
            fileops::fs_ensure_dir,
            fileops::fs_write_file,
            fileops::fs_output_file,
            fileops::fs_read_file,
            fileops::fs_readdir,
            fileops::fs_copy,
            fileops::move_file,
            fileops::trash_item,
            toggle_devtools,
            new_editor_window,
            open_settings_window,
            editing_command,
            quit_app,
            preferences::get_preferences,
            preferences::set_preferences,
            windows::window_add_file,
            windows::window_change_file,
            windows::window_remove_file,
            windows::window_open_folder,
            windows::window_ready,
            recent::recent_files,
            recent::add_recent_file,
            recent::clear_recent_files,
            buffers::update_buffer_state,
            buffers::load_buffer_state,
            buffers::close_buffer,
            buffers::abandon_buffer,
            windows::close_cancelled,
            search::search_start,
            search::search_cancel
        ])
        .manage(windows::Windows::default())
        .manage(folders::Folders::default())
        .manage(search::Searches::default())
        .on_window_event(|window, event| {
            let Some(id) = windows::id_from_label(window.label()) else { return };
            match event {
                tauri::WindowEvent::Focused(true) => window.state::<windows::Windows>().focus(id),
                tauri::WindowEvent::Destroyed => {
                    window.state::<windows::Windows>().remove(id);
                    window.state::<watch::FileWatcher>().unwatch_window(id);
                    window.state::<folders::Folders>().close(id);
                    window.state::<search::Searches>().close(id);
                }
                _ => {}
            }
        })
        .setup(|app| {
            let user_data = app.path().app_data_dir()?;
            let preferences = preferences::Preferences::load(user_data.join("preferences.json"));
            let cwd = std::env::current_dir().unwrap_or_default();
            let args: Vec<String> = std::env::args().skip(1).collect();
            let (files, mut folders) = cli_paths(&cwd, &args);
            let buffers = buffers::Buffers::new(&user_data);
            buffers::remove_temp_files(buffers.dir());
            let sessions = start_up(buffers.dir(), &preferences, &files, &mut folders);
            app.manage(preferences);
            app.manage(buffers);
            app.manage(recent::RecentFiles::load(user_data.join("recently-used-documents.json")));
            app.manage(watch::FileWatcher::start(app.handle()));
            if !files.is_empty() || !folders.is_empty() {
                windows::open_paths(app.handle(), files, folders, false);
                return Ok(());
            }
            // A session whose window fails to open stays on disk for next time.
            let mut restored = 0;
            for id in sessions {
                match open_restored_window(app.handle(), id) {
                    Ok(()) => restored += 1,
                    Err(err) => eprintln!("[buffers] {err}"),
                }
            }
            if restored == 0 {
                open_editor_window(app.handle(), Vec::new(), None)?;
            }
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
    fn cli_paths_keeps_markdown_new_files_and_folders() {
        let dir = temp_dir("filter");
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        std::fs::write(dir.join("notes.MD"), "").unwrap();
        std::fs::write(dir.join("image.png"), "").unwrap();

        let given = args(&["--new-window", "notes.MD", "image.png", "folder", "new.md", "gone/new.md", "folder/../x.md"]);
        let (files, folders) = cli_paths(&dir, &given);
        assert_eq!(files, vec![display(&dir.join("notes.MD")), display(&dir.join("new.md")), display(&dir.join("x.md"))]);
        assert_eq!(folders, vec![display(&dir.join("folder"))]);
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn saved_store(dir: &Path, id: &str) -> PathBuf {
        let path = dir.join(format!("{id}_editor_buffer_store.json"));
        std::fs::write(&path, r#"{"tabs":[{"isSaved":true}]}"#).unwrap();
        path
    }

    fn preferences_with(dir: &Path, values: serde_json::Value) -> preferences::Preferences {
        let path = dir.join("preferences.json");
        std::fs::write(&path, values.to_string()).unwrap();
        preferences::Preferences::load(path)
    }

    #[test]
    fn start_up_restores_or_clears_saved_sessions() {
        let dir = temp_dir("start-up");
        let stores = dir.join("editorStates");
        std::fs::create_dir_all(&stores).unwrap();
        let folder = display(&dir);

        // Restore all, with nothing else to open: the stores come back.
        let store = saved_store(&stores, "one");
        let restore = preferences_with(&dir, serde_json::json!({ "startUpAction": "restoreAll" }));
        let mut folders = Vec::new();
        assert_eq!(start_up(&stores, &restore, &[], &mut folders), vec!["one"]);
        assert!(store.exists());

        // Blank opens nothing, so nothing is forgotten.
        let blank = preferences_with(&dir, serde_json::json!({ "startUpAction": "blank" }));
        assert!(start_up(&stores, &blank, &[], &mut folders).is_empty());
        assert!(store.exists());

        // A start up folder opens like a path from the command line.
        let open_folder =
            preferences_with(&dir, serde_json::json!({ "startUpAction": "folder", "defaultDirectoryToOpen": folder }));
        assert!(start_up(&stores, &open_folder, &[], &mut folders).is_empty());
        assert_eq!(folders, vec![folder.clone()]);
        assert!(!store.exists());

        // So does a file named on the command line, whatever the action.
        let store = saved_store(&stores, "two");
        let mut folders = Vec::new();
        assert!(start_up(&stores, &restore, &[display(&dir.join("x.md"))], &mut folders).is_empty());
        assert!(!store.exists());
    }

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_nothing_behind() {
        let dir = temp_dir("write");
        let file = dir.join("note.md");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        assert_eq!(files_in(&dir), vec!["note.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_keeps_permissions_and_links() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("write-unix");
        let file = dir.join("private.md");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&file, b"new").unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);

        std::os::unix::fs::symlink("private.md", dir.join("link.md")).unwrap();
        write_atomic(&dir.join("link.md"), b"through the link").unwrap();
        assert!(std::fs::symlink_metadata(dir.join("link.md")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&file).unwrap(), b"through the link");

        std::os::unix::fs::symlink("gone.md", dir.join("broken.md")).unwrap();
        let err = write_atomic(&dir.join("broken.md"), b"x").unwrap_err();
        assert!(err.to_string().contains("missing file"), "{err}");
        assert_eq!(files_in(&dir), vec!["broken.md", "link.md", "private.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_resolves_to_its_target() {
        let dir = temp_dir("link");
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        std::fs::write(dir.join("elsewhere/target.md"), "").unwrap();
        std::os::unix::fs::symlink("elsewhere/target.md", dir.join("link.md")).unwrap();
        assert_eq!(cli_paths(&dir, &args(&["link.md"])).0, vec![display(&dir.join("elsewhere/target.md"))]);
    }
}
