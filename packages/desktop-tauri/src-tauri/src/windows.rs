// The editor window registry, standing in for
// packages/desktop/src/main/app/windowManager.ts and the open file bookkeeping
// in main/windows/editor.ts: which files each window has open, which window
// was focused last, and where the files of a second launch go
// (main/app/index.ts `second-instance` and `_openPathList`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::folders::Folders;
use crate::preferences::Preferences;
use crate::recent::RecentFiles;
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
    // A path appears twice while two tabs have it, which happens for a moment
    // when a document is renamed over another open one.
    opened_files: Vec<String>,
    // The folder in the sidebar, or the one it is about to show.
    root: Option<String>,
    // Files routed here before the page listened for them.
    ready: bool,
    pending: Vec<String>,
}

// What `Registry::plan` does with paths from a command line.
#[derive(Debug, PartialEq)]
enum Step {
    // Brings a window that shows the folder already to the front.
    Front(u32),
    // Opens files in an existing window, or in a new one for `None`.
    Files(Option<u32>, Vec<String>),
    // Opens a folder and files in a new window.
    Folder(String, Vec<String>),
}

impl Registry {
    fn add(&mut self, id: u32, root: Option<String>) {
        self.editors.insert(id, Editor { root, ..Editor::default() });
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

    // windowManager.ts findBestWindowToOpenIn with editor.ts getCandidateScores.
    // `None` means a new window. A file that is already open goes to its
    // window, which switches to the tab; Electron means to skip it but only
    // notices it in the first window.
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
                    // Five points for a window whose sidebar folder holds the
                    // file, one per open file in the same folder or a parent of
                    // it; the earliest window wins a tie.
                    let mut best: Option<(u32, usize)> = None;
                    for (&id, editor) in &self.editors {
                        let in_root = editor.root.as_ref().is_some_and(|root| is_inside(Path::new(root), &file));
                        let score = usize::from(in_root) * 5
                            + editor
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

    // main/app/index.ts _openPathList: each folder opens in a new window,
    // unless a window shows it already, and takes the files inside it; the
    // other files go where `route` sends them. `same_window` puts the first
    // folder and every file in one new window.
    fn plan(&self, mut files: Vec<String>, folders: Vec<String>, same_window: bool, each_in_new_window: bool) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut new_folders: Vec<(String, Vec<String>)> = Vec::new();
        for folder in folders {
            match self.editors.iter().find(|(_, editor)| editor.root.as_ref() == Some(&folder)) {
                Some((&id, _)) => steps.push(Step::Front(id)),
                None => new_folders.push((folder, Vec::new())),
            }
        }

        if same_window {
            match new_folders.into_iter().next() {
                Some((folder, _)) => steps.push(Step::Folder(folder, files)),
                None if !files.is_empty() => steps.push(Step::Files(None, files)),
                None => {}
            }
            return steps;
        }
        if each_in_new_window {
            steps.extend(files.into_iter().map(|file| Step::Files(None, vec![file])));
            steps.extend(new_folders.into_iter().map(|(folder, _)| Step::Folder(folder, Vec::new())));
            return steps;
        }

        // At start up there is no window to route files to, so one folder's
        // window takes them all: the first that holds one of them, or else the
        // first folder.
        let first_window = self.editors.is_empty();
        for (folder, list) in &mut new_folders {
            let dir = Path::new(folder.as_str());
            if first_window && files.iter().any(|file| is_inside(dir, file)) {
                list.append(&mut files);
                break;
            }
            let (inside, rest) = files.into_iter().partition(|file| is_inside(dir, file));
            *list = inside;
            files = rest;
        }
        if first_window {
            if let Some((_, list)) = new_folders.first_mut() {
                list.append(&mut files);
            }
        }
        steps.extend(self.route(files).into_iter().map(|(id, files)| Step::Files(id, files)));
        steps.extend(new_folders.into_iter().map(|(folder, files)| Step::Folder(folder, files)));
        steps
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

    pub fn add(&self, id: u32, root: Option<String>) {
        self.lock().add(id, root);
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

fn open_window(app: &AppHandle, files: Vec<String>, folder: Option<String>) {
    if let Err(err) = crate::open_editor_window(app, files, folder) {
        eprintln!("[windows] {err}");
    }
}

// Opens files in an existing window, or in a new one when `id` is `None`.
fn deliver(app: &AppHandle, id: Option<u32>, files: Vec<String>) {
    let existing = id.and_then(|id| Some((id, app.get_webview_window(&label(id))?)));
    let Some((id, window)) = existing else {
        open_window(app, files, None);
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

// Files and folders from a command line, at start up or from a second launch.
pub fn open_paths(app: &AppHandle, files: Vec<String>, folders: Vec<String>, same_window: bool) {
    let each_in_new_window = app
        .state::<Preferences>()
        .get("openFilesInNewWindow")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let steps = app.state::<Windows>().lock().plan(files, folders, same_window, each_in_new_window);
    for step in steps {
        match step {
            Step::Front(id) => {
                if let Some(window) = app.get_webview_window(&label(id)) {
                    bring_to_front(&window);
                }
            }
            Step::Files(id, files) => deliver(app, id, files),
            Step::Folder(folder, files) => open_window(app, files, Some(folder)),
        }
    }
}

// A second launch while the app runs (tauri-plugin-single-instance), as
// main/app/index.ts handles Electron's `second-instance` event.
pub fn second_launch(app: &AppHandle, argv: Vec<String>, cwd: String) {
    let args = argv.get(1..).unwrap_or_default();
    let new_window = args.iter().any(|arg| arg == "--new-window" || arg == "-n");
    let (files, folders) = crate::cli_paths(Path::new(&cwd), args);

    if files.is_empty() && folders.is_empty() {
        let last_active = app.state::<Windows>().lock().last_active();
        if let Some(window) = last_active.and_then(|id| app.get_webview_window(&label(id))) {
            bring_to_front(&window);
        }
        return;
    }
    open_paths(app, files, folders, new_window);
}

fn editor_id(window: &WebviewWindow) -> Option<u32> {
    id_from_label(window.label())
}

// Messages Rust has for a window's renderer; its page passes them on.
#[derive(Clone, serde::Serialize)]
struct RendererMessage {
    channel: &'static str,
    args: Vec<serde_json::Value>,
}

pub fn send_to_renderer(window: &WebviewWindow, channel: &'static str, args: Vec<serde_json::Value>) {
    let message = RendererMessage { channel, args };
    if let Err(err) = window.emit_to(window.label(), "mt::tauri::renderer-message", message) {
        eprintln!("[windows] {err}");
    }
}

// watcher.ts reports ENOSPC the same way.
pub fn report_watch_limit(window: &WebviewWindow, message: String) {
    let notification = serde_json::json!({ "title": "inotify limit reached", "type": "warning", "message": message });
    send_to_renderer(window, "mt::show-notification", vec![notification]);
}

// Watches a file this window opened, and tells the user once when the OS
// limit on watches is reached.
fn watch_file(window: &WebviewWindow, watcher: &FileWatcher, path: &str, id: u32) {
    if let Some(message) = watcher.watch(path, id) {
        report_watch_limit(window, message);
    }
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

fn replace_file(window: &WebviewWindow, windows: &Windows, watcher: &FileWatcher, id: u32, old_path: &str, path: &str) {
    let still_open = {
        let mut registry = windows.lock();
        let Some(editor) = registry.editors.get_mut(&id) else { return };
        match editor.opened_files.iter().position(|open| open == old_path) {
            Some(index) => editor.opened_files[index] = path.to_string(),
            None => editor.opened_files.push(path.to_string()),
        }
        editor.opened_files.iter().any(|open| open == old_path)
    };
    if !still_open {
        watcher.unwatch(old_path, id);
    }
    watch_file(window, watcher, path, id);
}

// After Save As (editor.ts changeOpenedFilePath).
#[tauri::command]
pub fn window_change_file(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    watcher: State<'_, FileWatcher>,
    old_path: String,
    path: String,
) {
    if let Some(id) = editor_id(&window) {
        replace_file(&window, &windows, &watcher, id, &old_path, &path);
    }
}

// A document renamed on disk (src/fileops.rs move_file). The content the
// window knew is the same under the new name, so the move is no outside
// change; the watcher's record is taken first because on Windows a change of
// case alone gives both names one record, which unwatching drops.
pub fn file_moved(window: &WebviewWindow, windows: &Windows, watcher: &FileWatcher, from: &str, to: &str) {
    let Some(id) = editor_id(window) else { return };
    let open = windows.lock().editors.get(&id).is_some_and(|editor| editor.opened_files.iter().any(|open| open == from));
    if open {
        let known = watcher.known(from, id);
        replace_file(window, windows, watcher, id, from, to);
        watcher.restore(to, id, known);
    }
}

#[tauri::command]
pub fn window_remove_file(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    watcher: State<'_, FileWatcher>,
    path: String,
) {
    let Some(id) = editor_id(&window) else { return };
    let still_open = {
        let mut registry = windows.lock();
        let Some(editor) = registry.editors.get_mut(&id) else { return };
        if let Some(index) = editor.opened_files.iter().position(|open| *open == path) {
            editor.opened_files.remove(index);
        }
        editor.opened_files.contains(&path)
    };
    if !still_open {
        watcher.unwatch(&path, id);
    }
}

// editor.ts openFolder: the window's sidebar shows the folder from now on,
// which becomes the last opened folder and joins the recent documents.
#[tauri::command]
pub fn window_open_folder(
    app: AppHandle,
    window: WebviewWindow,
    windows: State<'_, Windows>,
    folders: State<'_, Folders>,
    preferences: State<'_, Preferences>,
    recent: State<'_, RecentFiles>,
    path: String,
) -> Result<(), String> {
    let id = editor_id(&window).ok_or("not an editor window")?;
    let root = crate::resolve_cli_path(&std::env::current_dir().unwrap_or_default(), &path);
    if !root.is_dir() {
        return Err(format!("{path} is not a folder"));
    }
    if !folders.open(&app, id, root.clone()) {
        return Ok(());
    }
    let root = crate::display(&root);
    if let Some(editor) = windows.lock().editors.get_mut(&id) {
        editor.root = Some(root.clone());
    }
    recent.add(&root);
    let changes = serde_json::Map::from_iter([("lastOpenedFolder".to_string(), root.into())]);
    preferences.update(&app, changes)
}

// The page now listens for files to open; returns the ones that waited. A
// page that boots, after a reload too, starts without a folder: it would
// ignore the old folder's changes, and asking for that folder again would
// change nothing.
#[tauri::command]
pub fn window_ready(window: WebviewWindow, windows: State<'_, Windows>, folders: State<'_, Folders>) -> Vec<String> {
    let Some(id) = editor_id(&window) else { return Vec::new() };
    folders.close(id);
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
            registry.add(id, None);
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
    fn files_prefer_the_window_whose_folder_holds_them() {
        let mut registry = registry(&[(1, &["/a/x.md"]), (2, &[])]);
        registry.editors.get_mut(&2).unwrap().root = Some("/notes".into());
        let routed = registry.route(files(&["/notes/deep/new.md", "/a/new.md"]));
        assert_eq!(routed, vec![(Some(2), files(&["/notes/deep/new.md"])), (Some(1), files(&["/a/new.md"]))]);
    }

    #[test]
    fn at_start_up_one_folder_takes_every_file() {
        let plan = Registry::default().plan(files(&["/elsewhere/x.md", "/b/y.md"]), files(&["/a", "/b"]), false, false);
        // Electron gives them to the first folder that holds one of them.
        assert_eq!(
            plan,
            vec![Step::Folder("/a".into(), vec![]), Step::Folder("/b".into(), files(&["/elsewhere/x.md", "/b/y.md"]))]
        );
        let plan = Registry::default().plan(files(&["/x.md"]), files(&["/a"]), false, false);
        assert_eq!(plan, vec![Step::Folder("/a".into(), files(&["/x.md"]))]);
    }

    #[test]
    fn later_folders_open_with_their_files_and_others_are_routed() {
        let mut registry = registry(&[(1, &["/c/z.md"]), (2, &[])]);
        registry.editors.get_mut(&2).unwrap().root = Some("/shown".into());
        let plan = registry.plan(files(&["/a/in.md", "/c/new.md"]), files(&["/a", "/shown"]), false, false);
        assert_eq!(
            plan,
            vec![
                Step::Front(2),
                Step::Files(Some(1), files(&["/c/new.md"])),
                Step::Folder("/a".into(), files(&["/a/in.md"])),
            ]
        );
    }

    #[test]
    fn new_window_and_each_in_new_window() {
        let registry = registry(&[(1, &[])]);
        let plan = registry.plan(files(&["/x.md"]), files(&["/a", "/b"]), true, false);
        assert_eq!(plan, vec![Step::Folder("/a".into(), files(&["/x.md"]))]);
        let plan = registry.plan(files(&["/x.md", "/y.md"]), files(&["/a"]), false, true);
        assert_eq!(
            plan,
            vec![
                Step::Files(None, files(&["/x.md"])),
                Step::Files(None, files(&["/y.md"])),
                Step::Folder("/a".into(), vec![]),
            ]
        );
    }

    #[test]
    fn labels_round_trip() {
        assert_eq!(id_from_label(&label(7)), Some(7));
        assert_eq!(id_from_label("main"), None);
    }
}
