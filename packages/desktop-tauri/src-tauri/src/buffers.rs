// Session restore, standing in for packages/desktop/src/main/editorBufferStore:
// each editor window keeps its tabs, including unsaved text, in
// `<app data>/editorStates/<buffer id>_editor_buffer_store.json`, so that the
// "Restore all" start up action can bring the windows back. The page writes
// the state its renderer builds (store/bufferedState.ts) and reads it back in
// a restored window (src/buffers.ts).
//
// Three changes from Electron, decided by the user: a store that cannot be
// read is moved aside instead of being overwritten by the window's next
// write; closing a window without saving deletes its store; and quitting
// keeps every window's store, so the windows open at quit all come back.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde_json::Value;
use tauri::{State, WebviewWindow};

use crate::display;
use crate::windows::{id_from_label, Closing, Windows};

const SUFFIX: &str = "_editor_buffer_store.json";

pub struct Buffers {
    dir: PathBuf,
}

impl Buffers {
    pub fn new(app_data: &Path) -> Self {
        Self { dir: app_data.join("editorStates") }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn store_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{SUFFIX}"))
}

// The ids of the stores in `dir`, least recently written first, so that the
// window used last opens last and ends up in front. Temp files of an
// interrupted write and damaged stores have other names and are skipped.
pub fn list(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(SystemTime, String)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_suffix(SUFFIX).filter(|id| !id.is_empty() && !id.starts_with('.'))?;
            let meta = entry.metadata().ok().filter(|meta| meta.is_file())?;
            Some((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), id.to_string()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, id)| id).collect()
}

// Left behind when the app stopped in the middle of a write (see
// crate::write_atomic). Only called at start up, when no window can be writing.
pub fn remove_temp_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().ends_with(".marktext-tmp") {
            if let Err(err) = std::fs::remove_file(entry.path()) {
                eprintln!("[buffers] {}: {err}", entry.path().display());
            }
        }
    }
}

// editorBufferStore readBufferStoreFile: an object with a `tabs` array.
fn parse(text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Err("the file is empty".into());
    }
    let state: Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
    if !state.get("tabs").is_some_and(Value::is_array) {
        return Err("the file has no list of tabs".into());
    }
    Ok(state)
}

fn read(path: &Path) -> std::io::Result<Result<Value, String>> {
    std::fs::read_to_string(path).map(|text| parse(&text))
}

// Text that is not UTF-8 is damage; a missing file is no store. Any other
// error, such as a file another program holds open on Windows, may pass, so
// the read is tried again before giving up.
fn read_with_retry(path: &Path) -> std::io::Result<Result<Value, String>> {
    let lasting = |err: &std::io::Error| matches!(err.kind(), ErrorKind::NotFound | ErrorKind::InvalidData);
    for delay in [50, 75, 50] {
        match read(path) {
            Err(err) if !lasting(&err) => std::thread::sleep(Duration::from_millis(delay)),
            result => return result,
        }
    }
    read(path)
}

// No tabs, or every tab saved and on disk: closing the window loses nothing.
fn all_saved(state: &Value) -> bool {
    let flag = |tab: &Value, key: &str| tab.get(key).and_then(Value::as_bool).unwrap_or(false);
    state["tabs"]
        .as_array()
        .is_some_and(|tabs| tabs.iter().all(|tab| flag(tab, "isSaved") && !flag(tab, "isPendingCreation")))
}

// Retried as crate::rename_with_retry does, since a store that stays after
// the user discarded it would bring the discarded text back.
fn remove(path: &Path) -> std::io::Result<()> {
    for delay in [50, 75, 50] {
        match std::fs::remove_file(path) {
            Err(err) if crate::is_sharing_error(&err) => std::thread::sleep(Duration::from_millis(delay)),
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            result => return result,
        }
    }
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

// Deleted only when it reads as all saved; anything unreadable stays.
fn remove_if_all_saved(path: &Path) {
    match read(path) {
        Ok(Ok(state)) if all_saved(&state) => {
            if let Err(err) = remove(path) {
                eprintln!("[buffers] {}: {err}", path.display());
            }
        }
        Ok(Ok(_)) => {}
        Ok(Err(err)) => eprintln!("[buffers] {}: {err}", path.display()),
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => eprintln!("[buffers] {}: {err}", path.display()),
    }
}

// main/app/index.ts with paths on the command line: those open instead of the
// session, so the windows that had nothing unsaved are forgotten.
pub fn clear_all_saved(dir: &Path) {
    for id in list(dir) {
        remove_if_all_saved(&store_path(dir, &id));
    }
}

// The state must at least look like one, so that a faulty payload never
// replaces a good session.
fn write(dir: &Path, id: &str, state: &str) -> Result<(), String> {
    parse(state)?;
    let path = store_path(dir, id);
    crate::write_atomic(&path, state.as_bytes()).map_err(|err| format!("{}: {err}", path.display()))
}

// A free name beside `path`: `<name>.damaged`, then `<name>.damaged.2` and on,
// so that a second damaged store of the same window keeps the first.
fn damaged_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    (1..)
        .map(|n| if n == 1 { format!("{name}.damaged") } else { format!("{name}.damaged.{n}") })
        .map(|candidate| path.with_file_name(candidate))
        .find(|candidate| std::fs::symlink_metadata(candidate).is_err())
        .expect("an unused name exists")
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Loaded {
    State { state: Value },
    // Nothing was stored, as for a window that closed before its first write.
    Missing,
    // The file is not a session; it was moved to `kept`.
    Damaged { kept: String, reason: String },
    // The file could not be read, or not moved aside, and stays where it is.
    // The window then writes under a new id, so the file is kept for recovery.
    Unreadable { reason: String },
}

fn load(dir: &Path, id: &str) -> Loaded {
    let path = store_path(dir, id);
    let reason = match read_with_retry(&path) {
        Ok(Ok(state)) => return Loaded::State { state },
        Err(err) if err.kind() == ErrorKind::NotFound => return Loaded::Missing,
        Err(err) if err.kind() == ErrorKind::InvalidData => err.to_string(),
        Err(err) => return Loaded::Unreadable { reason: err.to_string() },
        Ok(Err(reason)) => reason,
    };
    let target = damaged_path(&path);
    match crate::rename_with_retry(&path, &target) {
        Ok(()) => Loaded::Damaged { kept: display(&target), reason },
        Err(err) => Loaded::Unreadable { reason: format!("{reason}; it could not be moved aside ({err})") },
    }
}

// editorBufferStore handleClose, with the user's choices: the store goes when
// the window closes without saving, or, outside a quit, when other editor
// windows stay open and this one has nothing unsaved. The last window keeps
// its store even when all is saved, so that its tabs come back.
fn close(dir: &Path, closing: &Closing, discard: bool) -> Result<(), String> {
    let path = store_path(dir, &closing.buffer_id);
    if discard {
        return remove(&path).map_err(|err| format!("{}: {err}", path.display()));
    }
    if closing.other_windows > 0 && !closing.quitting {
        remove_if_all_saved(&path);
    }
    Ok(())
}

async fn blocking<T: Send + 'static>(task: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(task).await.map_err(|err| err.to_string())
}

// `update-buffer-state`. `state` is the renderer's state as JSON text, written
// as given. False, without writing, for a window without a store (the settings
// window) or one that is closing.
#[tauri::command]
pub async fn update_buffer_state(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    buffers: State<'_, Buffers>,
    state: String,
) -> Result<bool, String> {
    let Some(id) = id_from_label(window.label()).and_then(|id| windows.writable_buffer_id(id)) else {
        return Ok(false);
    };
    let dir = buffers.dir.clone();
    blocking(move || write(&dir, &id, &state)).await??;
    Ok(true)
}

#[tauri::command]
pub async fn load_buffer_state(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    buffers: State<'_, Buffers>,
) -> Result<Loaded, String> {
    let window_id = id_from_label(window.label()).ok_or("not an editor window")?;
    let id = windows.buffer_id(window_id).ok_or("the window has no session store")?;
    let dir = buffers.dir.clone();
    let loaded = blocking(move || load(&dir, &id)).await?;
    if let Loaded::Unreadable { .. } = loaded {
        windows.set_buffer_id(window_id, new_id());
    }
    Ok(loaded)
}

// The page could not restore the session it loaded. The window goes on under
// a new id, so the store stays as it was for the next start.
#[tauri::command]
pub fn abandon_buffer(window: WebviewWindow, windows: State<'_, Windows>) -> Result<(), String> {
    let window_id = id_from_label(window.label()).ok_or("not an editor window")?;
    windows.buffer_id(window_id).ok_or("the window has no session store")?;
    windows.set_buffer_id(window_id, new_id());
    Ok(())
}

// Called by the page just before the window closes, after its last write.
#[tauri::command]
pub async fn close_buffer(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    buffers: State<'_, Buffers>,
    discard: bool,
) -> Result<(), String> {
    let Some(closing) = id_from_label(window.label()).and_then(|id| windows.begin_close(id)) else {
        return Ok(());
    };
    let dir = buffers.dir.clone();
    blocking(move || close(&dir, &closing, discard)).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-buffers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn tab(is_saved: bool, is_pending_creation: bool) -> Value {
        json!({ "pathname": "/a.md", "markdown": "text", "isSaved": is_saved, "isPendingCreation": is_pending_creation })
    }

    fn state(tabs: Vec<Value>) -> String {
        json!({ "version": 1, "tabs": tabs, "currentFileId": null, "project": { "rootDirectory": "" } }).to_string()
    }

    fn store(dir: &Path, id: &str, tabs: Vec<Value>) {
        write(dir, id, &state(tabs)).unwrap();
    }

    #[test]
    fn a_written_state_loads_back_and_leaves_no_temp_file() {
        let dir = temp_dir("write");
        store(&dir, "one", vec![tab(true, false)]);
        let text = state(vec![tab(false, false)]);
        write(&dir, "one", &text).unwrap();
        assert_eq!(names(&dir), vec!["one_editor_buffer_store.json"]);
        let expected: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(load(&dir, "one"), Loaded::State { state: expected });
    }

    #[test]
    fn a_faulty_state_is_not_written() {
        let dir = temp_dir("faulty");
        store(&dir, "one", vec![tab(false, false)]);
        assert!(write(&dir, "one", "{\"tabs\": 3}").is_err());
        assert!(write(&dir, "one", "not json").is_err());
        assert!(matches!(load(&dir, "one"), Loaded::State { .. }));
    }

    #[test]
    fn a_window_without_a_store_loads_nothing() {
        let dir = temp_dir("missing");
        assert_eq!(load(&dir, "none"), Loaded::Missing);
        assert_eq!(list(&dir.join("absent")), Vec::<String>::new());
    }

    #[test]
    fn listing_skips_temp_and_damaged_files_and_puts_the_newest_last() {
        let dir = temp_dir("list");
        store(&dir, "older", vec![]);
        let earlier = SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(store_path(&dir, "older"))
            .unwrap()
            .set_modified(earlier)
            .unwrap();
        store(&dir, "newer", vec![]);
        std::fs::write(dir.join("gone_editor_buffer_store.json.damaged"), "x").unwrap();
        std::fs::write(dir.join(".x_editor_buffer_store.json.1-0.marktext-tmp"), "x").unwrap();
        std::fs::write(dir.join("_editor_buffer_store.json"), "x").unwrap();
        std::fs::create_dir(dir.join("folder_editor_buffer_store.json")).unwrap();
        assert_eq!(list(&dir), vec!["older", "newer"]);

        remove_temp_files(&dir);
        assert!(!names(&dir).iter().any(|name| name.ends_with(".marktext-tmp")));
    }

    #[test]
    fn the_all_saved_rule() {
        let saved = |tabs: Vec<Value>| all_saved(&serde_json::from_str(&state(tabs)).unwrap());
        assert!(saved(vec![]));
        assert!(saved(vec![tab(true, false), tab(true, false)]));
        assert!(!saved(vec![tab(true, false), tab(false, false)]));
        assert!(!saved(vec![tab(true, true)]));
        assert!(!saved(vec![json!({ "pathname": "" })]));
    }

    #[test]
    fn a_command_line_start_clears_only_all_saved_stores() {
        let dir = temp_dir("clear");
        store(&dir, "saved", vec![tab(true, false)]);
        store(&dir, "empty", vec![]);
        store(&dir, "dirty", vec![tab(true, false), tab(false, false)]);
        store(&dir, "pending", vec![tab(true, true)]);
        std::fs::write(store_path(&dir, "broken"), "{").unwrap();
        clear_all_saved(&dir);
        assert_eq!(
            names(&dir),
            vec!["broken_editor_buffer_store.json", "dirty_editor_buffer_store.json", "pending_editor_buffer_store.json"]
        );
    }

    #[test]
    fn a_damaged_store_is_moved_aside() {
        let dir = temp_dir("damaged");
        for (id, text) in [("bad", "{\"tabs\":"), ("empty", "  \n"), ("notabs", "{\"tabs\": {}}")] {
            std::fs::write(store_path(&dir, id), text).unwrap();
            let kept = dir.join(format!("{id}_editor_buffer_store.json.damaged"));
            match load(&dir, id) {
                Loaded::Damaged { kept: at, .. } => assert_eq!(at, display(&kept)),
                other => panic!("{other:?}"),
            }
            assert_eq!(std::fs::read_to_string(&kept).unwrap(), text);
            assert!(!store_path(&dir, id).exists());
        }

        // The window writes again under its id; damage a second time keeps both.
        std::fs::write(store_path(&dir, "bad"), "second").unwrap();
        assert!(matches!(load(&dir, "bad"), Loaded::Damaged { .. }));
        assert_eq!(std::fs::read_to_string(dir.join("bad_editor_buffer_store.json.damaged")).unwrap(), "{\"tabs\":");
        assert_eq!(std::fs::read_to_string(dir.join("bad_editor_buffer_store.json.damaged.2")).unwrap(), "second");
        assert!(list(&dir).is_empty());

        std::fs::write(store_path(&dir, "latin1"), b"{\"tabs\": [], \"x\": \"\xe9\"}").unwrap();
        assert!(matches!(load(&dir, "latin1"), Loaded::Damaged { .. }));
    }

    #[test]
    fn a_store_that_cannot_be_read_stays_in_place() {
        let dir = temp_dir("unreadable");
        // Reading a folder fails with an error that is not damage.
        std::fs::create_dir(store_path(&dir, "held")).unwrap();
        assert!(matches!(load(&dir, "held"), Loaded::Unreadable { .. }));
        assert!(store_path(&dir, "held").is_dir());
        assert_eq!(names(&dir), vec!["held_editor_buffer_store.json"]);
    }

    fn closing(id: &str, other_windows: usize, quitting: bool) -> Closing {
        Closing { buffer_id: id.into(), other_windows, quitting }
    }

    #[test]
    fn the_close_rule() {
        let dir = temp_dir("close");
        let exists = |id: &str| store_path(&dir, id).exists();

        // The last window keeps its store, saved or not.
        store(&dir, "last", vec![tab(true, false)]);
        close(&dir, &closing("last", 0, false), false).unwrap();
        assert!(exists("last"));

        // With other windows open, only a window with nothing unsaved forgets.
        store(&dir, "saved", vec![tab(true, false)]);
        close(&dir, &closing("saved", 1, false), false).unwrap();
        assert!(!exists("saved"));
        store(&dir, "empty", vec![]);
        close(&dir, &closing("empty", 2, false), false).unwrap();
        assert!(!exists("empty"));
        store(&dir, "dirty", vec![tab(false, false)]);
        close(&dir, &closing("dirty", 1, false), false).unwrap();
        assert!(exists("dirty"));
        store(&dir, "pending", vec![tab(true, true)]);
        close(&dir, &closing("pending", 1, false), false).unwrap();
        assert!(exists("pending"));
        std::fs::write(store_path(&dir, "broken"), "{").unwrap();
        close(&dir, &closing("broken", 1, false), false).unwrap();
        assert!(exists("broken"));

        // Closing without saving forgets the store, even for the last window.
        close(&dir, &closing("dirty", 0, false), true).unwrap();
        assert!(!exists("dirty"));
        close(&dir, &closing("never-written", 0, false), true).unwrap();
        close(&dir, &closing("never-written", 1, false), false).unwrap();
    }

    #[test]
    fn quitting_keeps_every_store_unless_discarded() {
        let dir = temp_dir("quit");
        store(&dir, "saved", vec![tab(true, false)]);
        store(&dir, "empty", vec![]);
        store(&dir, "discarded", vec![tab(false, false)]);
        close(&dir, &closing("saved", 2, true), false).unwrap();
        close(&dir, &closing("empty", 1, true), false).unwrap();
        close(&dir, &closing("discarded", 1, true), true).unwrap();
        assert_eq!(names(&dir), vec!["empty_editor_buffer_store.json", "saved_editor_buffer_store.json"]);
    }
}
