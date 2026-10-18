// Files dropped onto an editor window from outside, for the page's
// `mt::window::drop` handler (src/drop.ts). The webviews keep their own drag
// and drop handling, which the editor relies on, so the paths are read here:
// web pages only see the file names.
//
// Linux: WebKitGTK gives the page neither the files nor their URIs, and its
// default action would load the dropped file in place of the editor. The
// paths are sent while the files are dragged over the window, so the page can
// take the drop before that happens.
//
// Windows: the page sees the files but not their paths. It posts them to the
// webview host, which knows each file's path.
//
// Also the navigation guard every window gets.

use std::path::Path;

use tauri::{Url, WebviewWindow};

use crate::is_markdown;

#[cfg(target_os = "linux")]
const DRAG_FILES_EVENT: &str = "mt::tauri::drag-files";
#[cfg(windows)]
const DROPPED_FILES_EVENT: &str = "mt::tauri::dropped-files";
#[cfg(windows)]
const DROP_MESSAGE: &str = "mt-file-drop:";

pub(crate) fn attach(window: &WebviewWindow) {
    #[cfg(any(target_os = "linux", windows))]
    if let Err(err) = watch_drops(window) {
        eprintln!("[drop] {}: {err}", window.label());
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = window;
}

// WebKit asks for the dragged data as the pointer moves over the view, so
// this runs during the hover, possibly several times and once per data
// format. Listening here leaves WebKit's own handling as it is.
#[cfg(target_os = "linux")]
fn watch_drops(window: &WebviewWindow) -> tauri::Result<()> {
    use std::cell::RefCell;

    use gtk::gdk::DragContext;
    use gtk::prelude::WidgetExt;
    use tauri::Emitter;

    let window = window.clone();
    window.clone().with_webview(move |webview| {
        // The drag last reported and its paths. Holding the context keeps a
        // later drag from getting the same address.
        let last: RefCell<Option<(DragContext, Vec<String>)>> = RefCell::new(None);
        webview.inner().connect_drag_data_received(move |_, context, _, _, data, _, _| {
            let paths: Vec<String> = data
                .uris()
                .iter()
                .filter_map(|uri| gtk::glib::filename_from_uri(uri).ok())
                .map(|(path, _)| crate::display(&path))
                .collect();
            let mut last = last.borrow_mut();
            let report = match last.as_ref() {
                Some((known, known_paths)) if known == context => !paths.is_empty() && paths != *known_paths,
                // An empty list tells the page that a new drag carries no files.
                _ => true,
            };
            if !report {
                return;
            }
            if let Err(err) = window.emit_to(window.label(), DRAG_FILES_EVENT, &paths) {
                eprintln!("[drop] {}: {err}", window.label());
            }
            *last = Some((context.clone(), paths));
        });
    })
}

// The page posts the string `mt-file-drop:<id>` with the dropped File objects.
// It has to be a string: WebView2 calls wry's handler for Tauri's own messages
// first, that handler fails on anything else, and a failing handler stops
// WebView2 calling the ones after it. wry then hands the string to Tauri,
// which cannot read it and logs an error in the page's console.
#[cfg(windows)]
fn watch_drops(window: &WebviewWindow) -> tauri::Result<()> {
    use tauri::Emitter;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2File, ICoreWebView2WebMessageReceivedEventArgs, ICoreWebView2WebMessageReceivedEventArgs2,
    };
    use webview2_com::{take_pwstr, WebMessageReceivedEventHandler};
    use windows_core::{Interface, PWSTR};

    unsafe fn dropped_paths(args: &ICoreWebView2WebMessageReceivedEventArgs) -> windows_core::Result<Vec<String>> {
        let objects = args.cast::<ICoreWebView2WebMessageReceivedEventArgs2>()?.AdditionalObjects()?;
        let mut count = 0;
        objects.Count(&mut count)?;
        let mut paths = Vec::new();
        for index in 0..count {
            // Anything other than a file is skipped.
            let Ok(file) = objects.GetValueAtIndex(index)?.cast::<ICoreWebView2File>() else { continue };
            let mut path = PWSTR::null();
            file.Path(&mut path)?;
            paths.push(take_pwstr(path));
        }
        Ok(paths)
    }

    let window = window.clone();
    window.clone().with_webview(move |webview| unsafe {
        let core = match webview.controller().CoreWebView2() {
            Ok(core) => core,
            Err(err) => return eprintln!("[drop] {}: {err}", window.label()),
        };
        let target = window.clone();
        let handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut message = PWSTR::null();
            if args.TryGetWebMessageAsString(&mut message).is_err() {
                return Ok(());
            }
            let message = take_pwstr(message);
            let Some(id) = message.strip_prefix(DROP_MESSAGE).and_then(|id| id.parse::<u64>().ok()) else {
                return Ok(());
            };
            let paths = dropped_paths(&args).unwrap_or_else(|err| {
                eprintln!("[drop] {}: {err}", target.label());
                Vec::new()
            });
            let payload = serde_json::json!({ "id": id, "paths": paths });
            if let Err(err) = target.emit_to(target.label(), DROPPED_FILES_EVENT, payload) {
                eprintln!("[drop] {}: {err}", target.label());
            }
            Ok(())
        }));
        let mut token = 0;
        if let Err(err) = core.add_WebMessageReceived(&handler, &mut token) {
            eprintln!("[drop] {}: {err}", window.label());
        }
    })
}

// packages/desktop/src/common/filesystem/paths.ts isMarkdownFile: an
// existing markdown file, or a link to one, judged by the link's target.
#[tauri::command]
pub(crate) fn is_markdown_file(path: String) -> bool {
    let path = Path::new(&path);
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::read_link(path).is_ok_and(|target| {
            let target = path.parent().unwrap_or(path).join(target);
            target.is_file() && is_markdown(&target)
        }),
        Ok(meta) => meta.is_file() && is_markdown(path),
        Err(_) => false,
    }
}

// The app's own pages: tauri://localhost on Linux and macOS,
// http(s)://tauri.localhost on Windows, and the dev server in debug builds.
// Electron refuses every navigation the page starts
// (packages/desktop/src/main/app/index.ts will-navigate); here the first
// load of the page has to pass too.
pub(crate) fn is_app_page(url: &Url, dev_url: Option<&Url>) -> bool {
    let host = url.host_str();
    match url.scheme() {
        "tauri" if host == Some("localhost") => true,
        "http" | "https" if host == Some("tauri.localhost") => true,
        _ => dev_url.is_some_and(|dev| dev.origin() == url.origin()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn only_app_pages_may_load() {
        for page in ["tauri://localhost", "tauri://localhost/index.html?wid=1", "http://tauri.localhost/", "https://tauri.localhost/x"] {
            assert!(is_app_page(&url(page), None), "{page}");
        }
        for page in [
            "file:///home/me/notes.md",
            "https://example.com/",
            "http://localhost:1420/",
            "tauri://elsewhere/",
            "http://tauri.localhost.example.com/",
            "about:blank",
        ] {
            assert!(!is_app_page(&url(page), None), "{page}");
        }
    }

    #[test]
    fn the_dev_server_may_load_when_given() {
        let dev = url("http://localhost:1420");
        assert!(is_app_page(&url("http://localhost:1420/index.html"), Some(&dev)));
        assert!(!is_app_page(&url("http://localhost:1421/"), Some(&dev)));
        assert!(!is_app_page(&url("https://localhost:1420/"), Some(&dev)));
        assert!(!is_app_page(&url("file:///tmp/a.md"), Some(&dev)));
    }

    #[test]
    fn markdown_files_and_links_to_them() {
        let dir = std::env::temp_dir().join(format!("mt-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("folder.md")).unwrap();
        std::fs::write(dir.join("notes.md"), "").unwrap();
        std::fs::write(dir.join("image.png"), "").unwrap();
        let check = |name: &str| is_markdown_file(crate::display(&dir.join(name)));
        assert!(check("notes.md"));
        assert!(!check("image.png"));
        assert!(!check("folder.md"));
        assert!(!check("missing.md"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("notes.md", dir.join("link")).unwrap();
            std::os::unix::fs::symlink("image.png", dir.join("image.md")).unwrap();
            std::os::unix::fs::symlink("gone.md", dir.join("broken.md")).unwrap();
            assert!(check("link"));
            assert!(!check("image.md"));
            assert!(!check("broken.md"));
        }
    }
}
