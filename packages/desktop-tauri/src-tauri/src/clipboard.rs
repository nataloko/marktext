// The clipboard's text for the page (`mt::clipboard::read-text` and
// `write-text`, Electron's clipboard.readText and writeText). The webviews
// refuse the page's own clipboard calls (WebKitGTK answers both with
// NotAllowedError), which left Paste as Plain Text with nothing to paste and
// Copy Path copying nothing.

#[tauri::command]
pub async fn clipboard_read_text(window: tauri::WebviewWindow) -> Result<String, String> {
    read_text(window).await
}

#[tauri::command]
pub async fn clipboard_write_text(window: tauri::WebviewWindow, text: String) -> Result<(), String> {
    write_text(window, text).await
}

#[cfg(target_os = "linux")]
async fn write_text(window: tauri::WebviewWindow, text: String) -> Result<(), String> {
    window
        .run_on_main_thread(move || gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&text))
        .map_err(|err| err.to_string())
}

#[cfg(windows)]
async fn write_text(_window: tauri::WebviewWindow, text: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || clipboard_win::set_clipboard_string(&text).map_err(|err| err.to_string()))
        .await
        .map_err(|err| err.to_string())?
}

#[cfg(not(any(target_os = "linux", windows)))]
async fn write_text(_window: tauri::WebviewWindow, _text: String) -> Result<(), String> {
    Err("writing the clipboard is not supported here".into())
}

// GTK's clipboard, the one the webview's own Paste reads. It belongs to the
// main thread and answers through a callback.
#[cfg(target_os = "linux")]
async fn read_text(window: tauri::WebviewWindow) -> Result<String, String> {
    use std::time::Duration;

    let (sender, receiver) = std::sync::mpsc::channel();
    window
        .run_on_main_thread(move || {
            gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).request_text(move |_, text| {
                let _ = sender.send(text.unwrap_or_default().to_string());
            });
        })
        .map_err(|err| err.to_string())?;
    tauri::async_runtime::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(5)))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|_| "the clipboard did not answer".to_string())
}

// Opening the clipboard is retried while another program holds it.
#[cfg(windows)]
async fn read_text(_window: tauri::WebviewWindow) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        if !clipboard_win::is_format_avail(clipboard_win::formats::CF_UNICODETEXT) {
            return Ok(String::new());
        }
        clipboard_win::get_clipboard_string().map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| err.to_string())?
}

#[cfg(not(any(target_os = "linux", windows)))]
async fn read_text(_window: tauri::WebviewWindow) -> Result<String, String> {
    Err("reading the clipboard is not supported here".into())
}
