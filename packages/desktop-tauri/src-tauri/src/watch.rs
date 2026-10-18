// Watching open files for changes made outside MarkText, standing in for
// packages/desktop/src/main/filesystem/watcher.ts (files only; folders wait
// for the sidebar). Each file's folder is watched rather than the file, so
// editors that save by replacing the file are seen as well. A file is checked
// once it has been quiet for a second (chokidar's awaitWriteFinish in
// Electron) and reported to each window whose last read or write of it had
// different content. A window's own saves therefore stay quiet for that
// window, while other windows with the file open still hear about them, as
// Electron's per window ignore list does. A folder on a network share, or
// any folder with the watcherUsePolling preference, is polled instead.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};

use notify::{EventHandler, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::preferences::Preferences;
use crate::windows::label;

const SETTLE: Duration = Duration::from_millis(1000);
const TICK: Duration = Duration::from_millis(150);
// chokidar polls every 100 ms; a network share is spared that.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

// watcher.ts: the OS may report no changes on a network share, so a UNC path
// is polled, as every path is with the watcherUsePolling preference. Electron
// polls on macOS too.
pub fn uses_polling(app: &AppHandle, path: &Path) -> bool {
    let preferred = app
        .try_state::<Preferences>()
        .and_then(|preferences| preferences.get("watcherUsePolling"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    preferred || is_unc(path)
}

// watcher.ts isUncPath: /^(?:\\\\|\/\/)[^\\/]+[\\/][^\\/]+/, a server and a share.
fn is_unc(path: &Path) -> bool {
    let path = path.to_string_lossy();
    let Some(rest) = path.strip_prefix(r"\\").or_else(|| path.strip_prefix("//")) else { return false };
    let mut parts = rest.splitn(2, ['\\', '/']);
    let server = parts.next().unwrap_or("");
    let share = parts.next().unwrap_or("");
    !server.is_empty() && !share.is_empty() && !share.starts_with(['\\', '/'])
}

// Changes are found by comparing modification times.
pub fn poll_watcher<F: EventHandler>(handler: F) -> notify::Result<PollWatcher> {
    PollWatcher::new(handler, notify::Config::default().with_poll_interval(POLL_INTERVAL))
}

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

// What each window last read or wrote: a hash of the bytes, None if missing.
type Known = HashMap<u32, Option<u64>>;

pub struct FileWatcher(Mutex<State>);

struct Watched {
    // As the windows know it.
    path: PathBuf,
    windows: BTreeSet<u32>,
}

struct Folder {
    path: PathBuf,
    files: usize,
    // False until the OS accepted the watch; the next file retries.
    watching: bool,
    // Watched by the poller rather than the OS.
    polled: bool,
}

// Maps are keyed by `key(path)`.
#[derive(Default)]
struct State {
    watcher: Option<RecommendedWatcher>,
    // Started when the first folder needs it, with this sender.
    poller: Option<PollWatcher>,
    events: Option<Sender<notify::Result<notify::Event>>>,
    files: HashMap<PathBuf, Watched>,
    folders: HashMap<PathBuf, Folder>,
    known: HashMap<PathBuf, Known>,
    // Files with events, waiting to be quiet.
    pending: HashMap<PathBuf, Instant>,
    limit_reported: bool,
}

// Windows file systems ignore case, and notify reports names as stored.
fn key(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path.to_path_buf()
    }
}

fn hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

// What a quiet file amounts to for a window, given what that window knew.
fn compare(known: Option<Option<u64>>, now: Option<u64>) -> Option<Kind> {
    match (known.flatten(), now) {
        (before, now) if before == now => None,
        (Some(_), None) => Some(Kind::Unlink),
        (None, Some(_)) if known.is_some() => Some(Kind::Add),
        (_, Some(_)) => Some(Kind::Change),
        (None, None) => None,
    }
}

impl State {
    fn remember(&mut self, path: &Path, window: u32, now: Option<u64>) {
        self.known.entry(key(path)).or_default().insert(window, now);
    }

    // Returns the folder to start watching, if the OS is not watching it yet.
    fn add(&mut self, path: &Path, window: u32) -> Option<PathBuf> {
        let watched = self
            .files
            .entry(key(path))
            .or_insert_with(|| Watched { path: path.to_path_buf(), windows: BTreeSet::new() });
        if !watched.windows.insert(window) || watched.windows.len() > 1 {
            return None;
        }
        let parent = path.parent()?;
        let folder = self
            .folders
            .entry(key(parent))
            .or_insert_with(|| Folder { path: parent.to_path_buf(), files: 0, watching: false, polled: false });
        folder.files += 1;
        (!folder.watching).then(|| folder.path.clone())
    }

    fn folder_watched(&mut self, folder: &Path, polled: bool) {
        if let Some(folder) = self.folders.get_mut(&key(folder)) {
            folder.watching = true;
            folder.polled = polled;
        }
    }

    fn watcher(&mut self, polled: bool) -> Option<&mut dyn Watcher> {
        if !polled {
            return self.watcher.as_mut().map(|watcher| watcher as &mut dyn Watcher);
        }
        if self.poller.is_none() {
            let sender = self.events.clone()?;
            self.poller = poll_watcher(move |event| {
                let _ = sender.send(event);
            })
            .inspect_err(|err| eprintln!("[watch] cannot poll files: {err}"))
            .ok();
        }
        self.poller.as_mut().map(|poller| poller as &mut dyn Watcher)
    }

    fn unwatch(&mut self, (folder, polled): (PathBuf, bool)) {
        if let Some(watcher) = self.watcher(polled) {
            let _ = watcher.unwatch(&folder);
        }
    }

    // Returns the folder to stop watching, and whether it is polled, if it
    // holds no watched file any more.
    fn drop_window(&mut self, path: &Path, window: u32) -> Option<(PathBuf, bool)> {
        let file_key = key(path);
        if let Some(known) = self.known.get_mut(&file_key) {
            known.remove(&window);
        }
        let watched = self.files.get_mut(&file_key)?;
        if !watched.windows.remove(&window) || !watched.windows.is_empty() {
            return None;
        }
        self.files.remove(&file_key);
        self.known.remove(&file_key);
        self.pending.remove(&file_key);
        let folder_key = key(path.parent()?);
        let folder = self.folders.get_mut(&folder_key)?;
        folder.files -= 1;
        if folder.files > 0 {
            return None;
        }
        let folder = self.folders.remove(&folder_key)?;
        folder.watching.then_some((folder.path, folder.polled))
    }

    fn note(&mut self, event: notify::Event) {
        // Opening and reading a file, MarkText's own loads included, is no change.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let now = Instant::now();
        for path in event.paths {
            let file_key = key(&path);
            if self.files.contains_key(&file_key) {
                self.pending.insert(file_key, now);
            }
        }
    }

    // Files that have been quiet long enough, with what each window knew
    // before the file is read again.
    fn take_quiet(&mut self) -> Vec<(PathBuf, PathBuf, Known)> {
        let quiet: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, last)| last.elapsed() >= SETTLE)
            .map(|(file_key, _)| file_key.clone())
            .collect();
        quiet
            .into_iter()
            .filter_map(|file_key| {
                self.pending.remove(&file_key);
                let path = self.files.get(&file_key)?.path.clone();
                let before = self.known.get(&file_key).cloned().unwrap_or_default();
                Some((file_key, path, before))
            })
            .collect()
    }

    // The windows to tell about the file's new content, recording it as known.
    fn settle(&mut self, file_key: &Path, now: Option<u64>, before: &Known) -> Vec<(u32, Kind)> {
        let Some(watched) = self.files.get(file_key) else { return Vec::new() };
        let windows: Vec<u32> = watched.windows.iter().copied().collect();
        let known = self.known.entry(file_key.to_path_buf()).or_default();
        windows
            .into_iter()
            .filter_map(|window| {
                let current = known.get(&window).copied();
                // The window read or wrote the file while it was being checked.
                if current != before.get(&window).copied() {
                    return None;
                }
                let kind = compare(current, now)?;
                known.insert(window, now);
                Some((window, kind))
            })
            .collect()
    }
}

impl FileWatcher {
    // Starts the OS watcher and the thread that turns its events into reports.
    pub fn start(app: &AppHandle) -> Self {
        let (sender, events) = std::sync::mpsc::channel();
        let os_sender = sender.clone();
        let watcher = notify::recommended_watcher(move |event| {
            let _ = os_sender.send(event);
        })
        .inspect_err(|err| eprintln!("[watch] cannot watch files: {err}"))
        .ok();
        let handle = app.clone();
        std::thread::spawn(move || run(handle, events));
        Self(Mutex::new(State { watcher, events: Some(sender), ..State::default() }))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Called with the bytes a window just read from or wrote to `path`.
    pub fn remember(&self, path: &str, window: u32, bytes: &[u8]) {
        self.lock().remember(Path::new(path), window, Some(hash(bytes)));
    }

    // What a window last read or wrote at `path`, for `restore` after a rename.
    pub fn known(&self, path: &str, window: u32) -> Option<Option<u64>> {
        self.lock().known.get(&key(Path::new(path))).and_then(|known| known.get(&window)).copied()
    }

    pub fn restore(&self, path: &str, window: u32, known: Option<Option<u64>>) {
        if let Some(known) = known {
            self.lock().remember(Path::new(path), window, known);
        }
    }

    // Returns a message for the user when the OS refuses another watch.
    pub fn watch(&self, path: &str, window: u32, polled: bool) -> Option<String> {
        let mut state = self.lock();
        let folder = state.add(Path::new(path), window)?;
        match state.watcher(polled)?.watch(&folder, RecursiveMode::NonRecursive) {
            Ok(()) => {
                state.folder_watched(&folder, polled);
                None
            }
            Err(err) if matches!(err.kind, notify::ErrorKind::MaxFilesWatch) && !state.limit_reported => {
                state.limit_reported = true;
                Some(err.to_string())
            }
            Err(err) => {
                eprintln!("[watch] {}: {err}", folder.display());
                None
            }
        }
    }

    pub fn unwatch(&self, path: &str, window: u32) {
        let mut state = self.lock();
        if let Some(folder) = state.drop_window(Path::new(path), window) {
            state.unwatch(folder);
        }
    }

    pub fn unwatch_window(&self, window: u32) {
        let mut state = self.lock();
        let paths: Vec<PathBuf> = state.files.values().map(|watched| watched.path.clone()).collect();
        for path in paths {
            if let Some(folder) = state.drop_window(&path, window) {
                state.unwatch(folder);
            }
        }
        for known in state.known.values_mut() {
            known.remove(&window);
        }
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
        for (file_key, path, before) in quiet {
            check(&app, &file_key, &path, &before);
        }
    }
}

fn check(app: &AppHandle, file_key: &Path, path: &Path, before: &Known) {
    // Read outside the lock; a file that cannot be read counts as missing.
    let now = std::fs::read(path).ok().as_deref().map(hash);
    let mtime_ms = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |since| since.as_millis() as f64);
    let reports = app.state::<FileWatcher>().lock().settle(file_key, now, before);
    let pathname = path.to_string_lossy().into_owned();
    for (window, kind) in reports {
        let event = FileEvent { pathname: pathname.clone(), kind, mtime_ms };
        if let Err(err) = app.emit_to(label(window), FILE_EVENT, event) {
            eprintln!("[watch] {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unc_paths_name_a_server_and_a_share() {
        assert!(is_unc(Path::new(r"\\server\share\notes\a.md")));
        assert!(is_unc(Path::new("//server/share")));
        assert!(!is_unc(Path::new(r"\\server")));
        assert!(!is_unc(Path::new(r"\\server\")));
        assert!(!is_unc(Path::new(r"C:\notes\a.md")));
        assert!(!is_unc(Path::new("/home/me/a.md")));
        assert!(!is_unc(Path::new("///x/y")));
    }

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

    #[test]
    fn folders_are_watched_while_they_hold_open_files() {
        let mut state = State::default();
        let folder = PathBuf::from("/notes");
        assert_eq!(state.add(Path::new("/notes/a.md"), 1), Some(folder.clone()));
        state.folder_watched(&folder, false);
        assert_eq!(state.add(Path::new("/notes/b.md"), 1), None);
        assert_eq!(state.add(Path::new("/notes/a.md"), 2), None);
        assert_eq!(state.drop_window(Path::new("/notes/a.md"), 1), None);
        assert_eq!(state.drop_window(Path::new("/notes/a.md"), 2), None);
        assert_eq!(state.drop_window(Path::new("/notes/b.md"), 1), Some((folder, false)));
        assert!(state.files.is_empty() && state.folders.is_empty());

        // A polled folder is unwatched from the poller.
        let share = PathBuf::from("//server/share");
        assert_eq!(state.add(Path::new("//server/share/a.md"), 1), Some(share.clone()));
        state.folder_watched(&share, true);
        assert_eq!(state.drop_window(Path::new("//server/share/a.md"), 1), Some((share, true)));
    }

    #[test]
    fn a_refused_folder_is_retried_by_the_next_file() {
        let mut state = State::default();
        let folder = PathBuf::from("/notes");
        assert_eq!(state.add(Path::new("/notes/a.md"), 1), Some(folder.clone()));
        assert_eq!(state.add(Path::new("/notes/b.md"), 1), Some(folder.clone()));
        state.folder_watched(&folder, false);
        assert_eq!(state.add(Path::new("/notes/c.md"), 1), None);
    }

    #[test]
    fn a_save_is_quiet_for_its_window_but_reported_to_others() {
        let mut state = State::default();
        let file = Path::new("/notes/a.md");
        state.add(file, 1);
        state.add(file, 2);
        state.remember(file, 1, Some(10));
        state.remember(file, 2, Some(10));
        // Window 1 saves new content.
        state.remember(file, 1, Some(20));
        let before = state.known[&key(file)].clone();
        assert_eq!(state.settle(&key(file), Some(20), &before), vec![(2, Kind::Change)]);
        let before = state.known[&key(file)].clone();
        assert_eq!(state.settle(&key(file), Some(20), &before), vec![]);
    }

    #[test]
    fn a_read_during_the_check_wins() {
        let mut state = State::default();
        let file = Path::new("/notes/a.md");
        state.add(file, 1);
        state.remember(file, 1, Some(10));
        let before = state.known[&key(file)].clone();
        // The window loads the file again while it is being checked.
        state.remember(file, 1, Some(30));
        assert_eq!(state.settle(&key(file), Some(20), &before), vec![]);
        assert_eq!(state.known[&key(file)][&1], Some(30));
    }
}
