// Watching open files for changes made outside MarkText, standing in for
// packages/desktop/src/main/filesystem/watcher.ts (files only; folders wait
// for the sidebar). Each file's folder is watched rather than the file, so
// editors that save by replacing the file are seen as well. A file is checked
// once it has been quiet for a second (chokidar's awaitWriteFinish in
// Electron) and reported only if its content differs from what MarkText last
// read or wrote, which also keeps MarkText's own saves quiet.
//
// Not ported yet: polling for network drives and the watcherUsePolling
// preference.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::windows::label;

const SETTLE: Duration = Duration::from_millis(1000);
const TICK: Duration = Duration::from_millis(150);

// Sent to a window that has the file open; the page loads it and tells the
// renderer (`mt::update-file`).
const FILE_EVENT: &str = "mt::tauri::file-changed";

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Add,
    Change,
    Unlink,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileEvent {
    pathname: String,
    kind: Kind,
    mtime_ms: f64,
}

pub struct FileWatcher(Mutex<State>);

#[derive(Default)]
struct State {
    watcher: Option<RecommendedWatcher>,
    // Watched file and the windows that have it open.
    files: HashMap<PathBuf, BTreeSet<u32>>,
    // Watched folder and how many watched files it holds.
    folders: HashMap<PathBuf, usize>,
    // What MarkText last read or wrote: a hash of the bytes, None if missing.
    known: HashMap<PathBuf, Option<u64>>,
    // Files with events, waiting to be quiet.
    pending: HashMap<PathBuf, Instant>,
    limit_reported: bool,
}

fn hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

// What a quiet file amounts to, given what MarkText knew about it.
fn compare(known: Option<Option<u64>>, now: Option<u64>) -> Option<Kind> {
    match (known.flatten(), now) {
        (before, now) if before == now => None,
        (Some(_), None) => Some(Kind::Unlink),
        (None, Some(_)) if known.is_some() => Some(Kind::Add),
        (_, Some(_)) => Some(Kind::Change),
        (None, None) => None,
    }
}

impl FileWatcher {
    // Starts the OS watcher and the thread that turns its events into reports.
    pub fn start(app: &AppHandle) -> Self {
        let (sender, events) = std::sync::mpsc::channel();
        let watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .inspect_err(|err| eprintln!("[watch] cannot watch files: {err}"))
        .ok();
        let handle = app.clone();
        std::thread::spawn(move || run(handle, events));
        Self(Mutex::new(State { watcher, ..State::default() }))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Called with the bytes MarkText just read from or wrote to `path`.
    pub fn remember(&self, path: &str, bytes: &[u8]) {
        self.lock().known.insert(PathBuf::from(path), Some(hash(bytes)));
    }

    // Returns a message for the user when the OS refuses another watch.
    pub fn watch(&self, path: &str, window: u32) -> Option<String> {
        let path = PathBuf::from(path);
        let mut state = self.lock();
        let windows = state.files.entry(path.clone()).or_default();
        let first = windows.is_empty();
        windows.insert(window);
        let folder = path.parent().filter(|_| first)?.to_path_buf();
        let count = state.folders.entry(folder.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            return None;
        }
        let result = state.watcher.as_mut()?.watch(&folder, RecursiveMode::NonRecursive);
        match result {
            Err(err) if matches!(err.kind, notify::ErrorKind::MaxFilesWatch) && !state.limit_reported => {
                state.limit_reported = true;
                Some(err.to_string())
            }
            Err(err) => {
                eprintln!("[watch] {}: {err}", folder.display());
                None
            }
            Ok(()) => None,
        }
    }

    pub fn unwatch(&self, path: &str, window: u32) {
        let mut state = self.lock();
        state.drop_window(Path::new(path), window);
    }

    pub fn unwatch_window(&self, window: u32) {
        let mut state = self.lock();
        let paths: Vec<PathBuf> = state.files.keys().cloned().collect();
        for path in paths {
            state.drop_window(&path, window);
        }
    }
}

impl State {
    fn drop_window(&mut self, path: &Path, window: u32) {
        let Some(windows) = self.files.get_mut(path) else { return };
        windows.remove(&window);
        if !windows.is_empty() {
            return;
        }
        self.files.remove(path);
        self.known.remove(path);
        self.pending.remove(path);
        let Some(folder) = path.parent() else { return };
        let Some(count) = self.folders.get_mut(folder) else { return };
        *count -= 1;
        if *count == 0 {
            self.folders.remove(folder);
            if let Some(watcher) = self.watcher.as_mut() {
                let _ = watcher.unwatch(folder);
            }
        }
    }

    fn note(&mut self, event: notify::Event) {
        // Opening and reading a file, MarkText's own loads included, is no change.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let now = Instant::now();
        for path in event.paths {
            if self.files.contains_key(&path) {
                self.pending.insert(path, now);
            }
        }
    }

    fn take_quiet(&mut self) -> Vec<PathBuf> {
        let quiet: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, last)| last.elapsed() >= SETTLE)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &quiet {
            self.pending.remove(path);
        }
        quiet
    }
}

fn run(app: AppHandle, events: Receiver<notify::Result<notify::Event>>) {
    loop {
        let event = match events.recv_timeout(TICK) {
            Ok(Ok(event)) => Some(event),
            Ok(Err(err)) => {
                eprintln!("[watch] {err}");
                None
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // The state is registered just after this thread starts.
        let Some(watcher) = app.try_state::<FileWatcher>() else { continue };
        let quiet = {
            let mut state = watcher.lock();
            if let Some(event) = event {
                state.note(event);
            }
            state.take_quiet()
        };
        for path in quiet {
            check(&app, path);
        }
    }
}

fn check(app: &AppHandle, path: PathBuf) {
    // Read outside the lock; a file that cannot be read counts as missing.
    let bytes = std::fs::read(&path).ok();
    let mtime_ms = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |since| since.as_millis() as f64);

    let watcher = app.state::<FileWatcher>();
    let (change, windows) = {
        let mut state = watcher.lock();
        let Some(windows) = state.files.get(&path).cloned() else { return };
        let now = bytes.as_deref().map(hash);
        let change = compare(state.known.get(&path).copied(), now);
        if change.is_some() {
            state.known.insert(path.clone(), now);
        }
        (change, windows)
    };
    let Some(kind) = change else { return };
    let event = FileEvent { pathname: path.to_string_lossy().into_owned(), kind, mtime_ms };
    for window in windows {
        if let Err(err) = app.emit_to(label(window), FILE_EVENT, event.clone()) {
            eprintln!("[watch] {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_changes_are_reported() {
        assert_eq!(compare(Some(Some(1)), Some(1)), None);
        assert_eq!(compare(Some(Some(1)), Some(2)), Some(Kind::Change));
        assert_eq!(compare(Some(Some(1)), None), Some(Kind::Unlink));
        assert_eq!(compare(Some(None), Some(1)), Some(Kind::Add));
        assert_eq!(compare(Some(None), None), None);
        assert_eq!(compare(None, Some(1)), Some(Kind::Change));
        assert_eq!(compare(None, None), None);
    }
}
