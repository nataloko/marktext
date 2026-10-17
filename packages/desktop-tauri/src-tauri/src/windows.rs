// The editor window registry, standing in for
// packages/desktop/src/main/app/windowManager.ts and the open file bookkeeping
// in main/windows/editor.ts: which files each window has open, which window
// was focused last, and where the files of a second launch go
// (main/app/index.ts `second-instance` and `_openPathList`).
//
// Not ported yet: folders (they need the sidebar) and the settings window.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::preferences::Preferences;
use crate::watch::FileWatcher;

// Asks one window's page to open files; the page listens on its own window.
const OPEN_FILES_EVENT: &str = "mt::tauri::open-files";

pub fn label(id: u32) -> String {
    format!("editor-{id}")
}

pub fn id_from_label(label: &str) -> Option<u32> {
    label.strip_prefix("editor-")?.parse().ok()
}

#[derive(Default)]
pub struct Windows(Mutex<Registry>);

#[derive(Default)]
struct Registry {
    editors: BTreeMap<u32, Editor>,
    // Window ids, least recently focused first.
    activity: Vec<u32>,
}

#[derive(Default)]
struct Editor {
    opened_files: Vec<String>,
    // Files routed here before the page listened for them.
    ready: bool,
    pending: Vec<String>,
}

impl Registry {
    fn add(&mut self, id: u32) {
        self.editors.insert(id, Editor::default());
        if self.editors.len() == 1 {
            self.focus(id);
        }
    }

    fn focus(&mut self, id: u32) {
        if self.editors.contains_key(&id) {
            self.activity.retain(|&other| other != id);
            self.activity.push(id);
        }
    }

    fn remove(&mut self, id: u32) {
        self.editors.remove(&id);
        self.activity.retain(|&other| other != id);
    }

    fn last_active(&self) -> Option<u32> {
        self.activity.last().copied()
    }

    // windowManager.ts findBestWindowToOpenIn with editor.ts getCandidateScores,
    // minus the root folder bonus. `None` means a new window. A file that is
    // already open goes to its window, which switches to the tab; Electron
    // means to skip it but only notices it in the first window.
    fn route(&self, files: Vec<String>) -> Vec<(Option<u32>, Vec<String>)> {
        if files.is_empty() {
            return Vec::new();
        }
        let last_active = self.last_active();
        if self.editors.len() <= 1 {
            return vec![(last_active, files)];
        }

        let mut groups: Vec<(Option<u32>, Vec<String>)> = Vec::new();
        for file in files {
            let owner = self.editors.iter().find(|(_, editor)| editor.opened_files.contains(&file));
            let target = match owner {
                Some((&id, _)) => Some(id),
                None => {
                    // One point per open file in the same folder or a parent of it;
                    // the earliest window wins a tie.
                    let mut best: Option<(u32, usize)> = None;
                    for (&id, editor) in &self.editors {
                        let score = editor
                            .opened_files
                            .iter()
                            .filter(|open| Path::new(open).parent().is_some_and(|dir| is_inside(dir, &file)))
                            .count();
                        if score > 0 && best.is_none_or(|(_, top)| score > top) {
                            best = Some((id, score));
                        }
                    }
                    best.map(|(id, _)| id).or(last_active)
                }
            };
            match groups.iter_mut().find(|(id, _)| *id == target) {
                Some((_, list)) => list.push(file),
                None => groups.push((target, vec![file])),
            }
        }
        groups
    }
}

// common/filesystem/paths.ts isChildOfDirectory.
fn is_inside(dir: &Path, file: &str) -> bool {
    let file = Path::new(file);
    file != dir && file.starts_with(dir)
}

impl Windows {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn add(&self, id: u32) {
        self.lock().add(id);
    }

    pub fn focus(&self, id: u32) {
        self.lock().focus(id);
    }

    pub fn remove(&self, id: u32) {
        self.lock().remove(id);
    }
}

fn bring_to_front(window: &WebviewWindow) {
    let _ = window.unminimize();
    let _ = window.set_focus();
}

// Opens files in an existing window, or in a new one when `id` is `None`.
fn deliver(app: &AppHandle, id: Option<u32>, files: Vec<String>) {
    let existing = id.and_then(|id| Some((id, app.get_webview_window(&label(id))?)));
    let Some((id, window)) = existing else {
        if let Err(err) = crate::open_editor_window(app, files) {
            eprintln!("[windows] {err}");
        }
        return;
    };
    let windows = app.state::<Windows>();
    let ready = {
        let mut registry = windows.lock();
        match registry.editors.get_mut(&id) {
            Some(editor) if !editor.ready => {
                editor.pending.extend(files.iter().cloned());
                false
            }
            _ => true,
        }
    };
    if ready {
        if let Err(err) = window.emit_to(label(id), OPEN_FILES_EVENT, files) {
            eprintln!("[windows] {err}");
        }
    }
    bring_to_front(&window);
}

// A second launch while the app runs (tauri-plugin-single-instance), as
// main/app/index.ts handles Electron's `second-instance` event.
pub fn second_launch(app: &AppHandle, argv: Vec<String>, cwd: String) {
    let args = argv.get(1..).unwrap_or_default();
    let new_window = args.iter().any(|arg| arg == "--new-window" || arg == "-n");
    let files = crate::markdown_files(Path::new(&cwd), args);

    if files.is_empty() {
        let last_active = app.state::<Windows>().lock().last_active();
        if let Some(window) = last_active.and_then(|id| app.get_webview_window(&label(id))) {
            bring_to_front(&window);
        }
        return;
    }
    if new_window {
        deliver(app, None, files);
        return;
    }
    let each_in_new_window = app
        .state::<Preferences>()
        .get("openFilesInNewWindow")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if each_in_new_window {
        for file in files {
            deliver(app, None, vec![file]);
        }
        return;
    }
    let groups = app.state::<Windows>().lock().route(files);
    for (id, files) in groups {
        deliver(app, id, files);
    }
}

fn editor_id(window: &WebviewWindow) -> Option<u32> {
    id_from_label(window.label())
}

// Messages Rust has for this window's renderer; the page passes them on.
#[derive(Clone, serde::Serialize)]
struct RendererMessage {
    channel: &'static str,
    args: Vec<serde_json::Value>,
}

// Watches a file this window opened, and tells the user once when the OS
// limit on watches is reached (watcher.ts reports ENOSPC the same way).
fn watch_file(window: &WebviewWindow, watcher: &FileWatcher, path: &str, id: u32) {
    let Some(message) = watcher.watch(path, id) else { return };
    let notification = RendererMessage {
        channel: "mt::show-notification",
        args: vec![serde_json::json!({ "title": "inotify limit reached", "type": "warning", "message": message })],
    };
    let _ = window.emit_to(window.label(), "mt::tauri::renderer-message", notification);
}

// Returns false when the window already has the file open, so the page
// switches to its tab instead (editor.ts openTabsFromPaths). Open files are
// watched for outside changes (src/watch.rs).
#[tauri::command]
pub fn window_add_file(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    watcher: State<'_, FileWatcher>,
    path: String,
) -> bool {
    let Some(id) = editor_id(&window) else { return false };
    {
        let mut registry = windows.lock();
        let Some(editor) = registry.editors.get_mut(&id) else { return false };
        if editor.opened_files.contains(&path) {
            return false;
        }
        editor.opened_files.push(path.clone());
    }
    watch_file(&window, &watcher, &path, id);
    true
}

// After Save As or a rename (editor.ts changeOpenedFilePath).
#[tauri::command]
pub fn window_change_file(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    watcher: State<'_, FileWatcher>,
    old_path: String,
    path: String,
) {
    let Some(id) = editor_id(&window) else { return };
    {
        let mut registry = windows.lock();
        let Some(editor) = registry.editors.get_mut(&id) else { return };
        editor.opened_files.retain(|open| *open != old_path && *open != path);
        editor.opened_files.push(path.clone());
    }
    watcher.unwatch(&old_path, id);
    watch_file(&window, &watcher, &path, id);
}

#[tauri::command]
pub fn window_remove_file(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    watcher: State<'_, FileWatcher>,
    path: String,
) {
    let Some(id) = editor_id(&window) else { return };
    if let Some(editor) = windows.lock().editors.get_mut(&id) {
        editor.opened_files.retain(|open| *open != path);
    }
    watcher.unwatch(&path, id);
}

// The page now listens for files to open; returns the ones that waited.
#[tauri::command]
pub fn window_ready(window: WebviewWindow, windows: State<'_, Windows>) -> Vec<String> {
    let Some(id) = editor_id(&window) else { return Vec::new() };
    let mut registry = windows.lock();
    let Some(editor) = registry.editors.get_mut(&id) else { return Vec::new() };
    editor.ready = true;
    std::mem::take(&mut editor.pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(windows: &[(u32, &[&str])]) -> Registry {
        let mut registry = Registry::default();
        for &(id, files) in windows {
            registry.add(id);
            registry.focus(id);
            registry.editors.get_mut(&id).unwrap().opened_files = files.iter().map(|f| f.to_string()).collect();
        }
        registry
    }

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn one_window_takes_everything() {
        let registry = registry(&[(1, &["/a/x.md"])]);
        assert_eq!(registry.route(files(&["/b/y.md", "/a/x.md"])), vec![(Some(1), files(&["/b/y.md", "/a/x.md"]))]);
    }

    #[test]
    fn no_window_means_a_new_one() {
        assert_eq!(Registry::default().route(files(&["/a/x.md"])), vec![(None, files(&["/a/x.md"]))]);
    }

    #[test]
    fn files_go_to_the_window_with_neighbours() {
        let registry = registry(&[(1, &["/a/x.md"]), (2, &["/b/y.md", "/b/z.md"]), (3, &[])]);
        let routed = registry.route(files(&["/b/new.md", "/b/sub/deep.md", "/a/new.md"]));
        assert_eq!(routed, vec![(Some(2), files(&["/b/new.md", "/b/sub/deep.md"])), (Some(1), files(&["/a/new.md"]))]);
    }

    #[test]
    fn unrelated_files_go_to_the_last_focused_window() {
        let mut registry = registry(&[(1, &["/a/x.md"]), (2, &["/b/y.md"])]);
        registry.focus(1);
        assert_eq!(registry.route(files(&["/c/new.md"])), vec![(Some(1), files(&["/c/new.md"]))]);
    }

    #[test]
    fn open_files_go_to_their_window() {
        let registry = registry(&[(1, &["/a/x.md"]), (2, &["/b/y.md"])]);
        assert_eq!(registry.route(files(&["/b/y.md"])), vec![(Some(2), files(&["/b/y.md"]))]);
    }

    #[test]
    fn closing_a_window_hands_focus_back() {
        let mut registry = registry(&[(1, &[]), (2, &[])]);
        registry.remove(2);
        assert_eq!(registry.last_active(), Some(1));
        registry.remove(1);
        assert_eq!(registry.last_active(), None);
    }

    #[test]
    fn labels_round_trip() {
        assert_eq!(id_from_label(&label(7)), Some(7));
        assert_eq!(id_from_label("main"), None);
    }
}
