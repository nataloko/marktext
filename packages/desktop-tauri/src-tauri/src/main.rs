#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Manager};

mod preferences;

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

fn files_from_command_line(cwd: &Path) -> Vec<String> {
    std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .map(|arg| display(&cwd.join(arg)))
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

#[tauri::command]
fn read_text_file(path: String) -> Result<String, String> {
    let bytes = std::fs::read(&path).map_err(|err| format!("{path}: {err}"))?;
    String::from_utf8(bytes)
        .map_err(|_| format!("{path} is not UTF-8. Other encodings are not supported in the Tauri trial."))
}

// Writes beside the target and renames over it, so a failed write never
// leaves a truncated file behind.
fn write_atomic(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file path"))?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = target.with_file_name(format!(".{}.marktext-tmp", name.to_string_lossy()));
    std::fs::write(&temp, contents)?;
    std::fs::rename(&temp, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

#[tauri::command]
fn write_text_file(path: String, contents: String) -> Result<(), String> {
    write_atomic(Path::new(&path), contents.as_bytes()).map_err(|err| format!("{path}: {err}"))
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
fn open_editor_window(app: &AppHandle, open_files: Vec<String>) -> tauri::Result<()> {
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
    let window = tauri::WebviewWindowBuilder::new(
        app,
        format!("editor-{window_id}"),
        tauri::WebviewUrl::default(),
    )
    .title("MarkText")
    .inner_size(1200.0, 800.0)
    .min_inner_size(400.0, 300.0)
    .decorations(false)
    .disable_drag_drop_handler()
    .initialization_script(script)
    .build()?;

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

// Async because building a window inside a synchronous command deadlocks on Windows.
#[tauri::command]
async fn new_editor_window(app: AppHandle, files: Vec<String>) -> Result<(), String> {
    open_editor_window(&app, files).map_err(|err| err.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            read_text_file,
            write_text_file,
            path_exists,
            toggle_devtools,
            new_editor_window,
            preferences::get_preferences,
            preferences::set_preferences
        ])
        .setup(|app| {
            let user_data = app.path().app_data_dir()?;
            app.manage(preferences::Preferences::load(user_data.join("preferences.json")));
            let cwd = std::env::current_dir().unwrap_or_default();
            open_editor_window(app.handle(), files_from_command_line(&cwd))?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running MarkText");
}
