// The folder in an editor window's sidebar, standing in for the folder
// watches of packages/desktop/src/main/filesystem/watcher.ts. A thread per
// window lists the folder once and then follows its changes, sending them in
// batches that the page passes on to the renderer as `mt::update-object-tree`
// (src/folders.ts). What an OS event says happened differs between platforms,
// so the thread mostly does not rely on it: it lists each folder that had
// events again and compares the result with what it sent before.
//
// Not ported yet: polling for network drives and the watcherUsePolling
// preference.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use globset::{GlobBuilder, GlobMatcher};
use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::display;
use crate::preferences::Preferences;
use crate::windows::{label, report_watch_limit};

const FOLDER_EVENT: &str = "mt::tauri::folder";
// Events are handled once the folder has been quiet this long, or after
// LONGEST while they keep coming.
const QUIET: Duration = Duration::from_millis(100);
const LONGEST: Duration = Duration::from_millis(1000);
// Changes per message, so that a large folder starts to show while it is listed.
const BATCH: usize = 500;

// Linux watches each listed folder on its own, so that skipped folders such
// as node_modules cost no inotify watches. Elsewhere one watch on the root
// covers the whole tree.
const PER_FOLDER: bool = cfg!(target_os = "linux");

// Windows drops events without a word when they come faster than notify
// reads them (inotify says so, and notify asks for a rescan), so a round with
// this many events lists everything again there.
const BUSY: usize = 500;

static GENERATION: AtomicU64 = AtomicU64::new(1);

struct Pattern {
    matcher: GlobMatcher,
    // minimatch's matchBase: a pattern without a slash matches the name alone.
    name_only: bool,
}

// watcher.ts `ignored`: node_modules and Electron archives, the
// treePathExcludePatterns preference, and files that are not markdown.
// Unlike minimatch, globset lets `*` match a leading dot.
struct Filter {
    source: Vec<String>,
    patterns: Vec<Pattern>,
}

impl Filter {
    fn new(source: Vec<String>) -> Self {
        let patterns = source
            .iter()
            .filter_map(|pattern| {
                let glob = GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .build()
                    .inspect_err(|err| eprintln!("[folders] {pattern}: {err}"))
                    .ok()?;
                Some(Pattern { matcher: glob.compile_matcher(), name_only: !pattern.contains('/') })
            })
            .collect();
        Self { source, patterns }
    }

    fn skips(&self, path: &Path, name: &OsStr) -> bool {
        let text = name.to_string_lossy();
        text.starts_with("node_modules")
            || text.find(".asar").is_some_and(|at| at > 0)
            || self.patterns.iter().any(|pattern| {
                if pattern.name_only {
                    pattern.matcher.is_match(name)
                } else {
                    pattern.matcher.is_match(path)
                }
            })
    }
}

// Read when the folder opens and again for each round of events, as
// watcher.ts reads it for each new entry.
fn exclude_patterns(app: &AppHandle) -> Vec<String> {
    app.try_state::<Preferences>()
        .and_then(|preferences| preferences.get("treePathExcludePatterns"))
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Found {
    Folder { link: bool },
    File,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Known {
    Folder,
    File { mtime_ms: f64 },
}

#[derive(Clone, Copy)]
struct Times {
    mtime_ms: f64,
    birth_ms: f64,
}

fn millis(time: std::io::Result<SystemTime>) -> Option<f64> {
    Some(time.ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs_f64() * 1000.0)
}

// Node's birthtime and mtimeMs, which the renderer sorts by. File systems
// without a creation time give the modification time.
fn times(path: &Path) -> Option<Times> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = millis(meta.modified())?;
    Some(Times { mtime_ms, birth_ms: millis(meta.created()).unwrap_or(mtime_ms) })
}

fn folder_change(path: &Path, name: &OsStr) -> Value {
    json!({
        "pathname": display(path),
        "name": name.to_string_lossy(),
        "isCollapsed": true,
        "isDirectory": true,
        "isFile": false,
        "isMarkdown": false,
        "folders": [],
        "files": []
    })
}

// Without the content watcher.ts loads for every file: the renderer only
// uses it for a file the user just created in the sidebar.
fn file_change(path: &Path, name: &OsStr, times: Times) -> Value {
    json!({
        "pathname": display(path),
        "name": name.to_string_lossy(),
        "isFile": true,
        "isDirectory": false,
        "birthTime": times.birth_ms,
        "mtimeMs": times.mtime_ms,
        "isMarkdown": true
    })
}

// Where the tree's changes go: to the page, or to a list in the tests.
trait Out {
    fn push(&mut self, kind: &'static str, change: Value);
    // True once the window has closed or opened another folder.
    fn stopped(&self) -> bool;
}

struct Watches {
    watcher: Option<RecommendedWatcher>,
    // The OS refused a watch for want of resources; reported once.
    limit: Option<String>,
    limit_reported: bool,
}

impl Watches {
    fn watch(&mut self, dir: &Path, mode: RecursiveMode) {
        let Some(watcher) = self.watcher.as_mut() else { return };
        match watcher.watch(dir, mode) {
            Ok(()) => {}
            Err(err) if matches!(err.kind, notify::ErrorKind::MaxFilesWatch) => {
                if !self.limit_reported {
                    self.limit_reported = true;
                    self.limit = Some(err.to_string());
                }
            }
            Err(err) => eprintln!("[folders] {}: {err}", dir.display()),
        }
    }

    fn start(&mut self, root: &Path) {
        if !PER_FOLDER {
            self.watch(root, RecursiveMode::Recursive);
        }
    }

    fn add(&mut self, dir: &Path) {
        if PER_FOLDER {
            self.watch(dir, RecursiveMode::NonRecursive);
        }
    }

    fn remove(&mut self, dir: &Path) {
        if let (true, Some(watcher)) = (PER_FOLDER, self.watcher.as_mut()) {
            let _ = watcher.unwatch(dir);
        }
    }
}

// What a round of events asks for.
#[derive(Default)]
struct Round {
    // Folders to list again.
    dirs: HashSet<PathBuf>,
    // Paths the events named; the files among them are checked for changes.
    touched: HashSet<PathBuf>,
    // Folders removed or renamed away, even if there is one with the same name
    // now: notify stops watching such a folder, and what it held is unknown.
    gone: HashSet<PathBuf>,
    // The OS dropped events: every folder is listed again and every file checked.
    everything: bool,
    events: usize,
}

struct Tree {
    real_root: PathBuf,
    filter: Filter,
    watches: Watches,
    // Each listed folder, the root included, with the entries in it that the
    // page knows about.
    folders: HashMap<PathBuf, HashMap<OsString, Known>>,
    // Linked folders shown, with their real paths.
    links: HashMap<PathBuf, PathBuf>,
}

impl Tree {
    fn new(root: PathBuf, filter: Filter, watcher: Option<RecommendedWatcher>) -> Self {
        let watches = Watches { watcher, limit: None, limit_reported: false };
        let real_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        Self { real_root, filter, watches, folders: HashMap::new(), links: HashMap::new() }
    }

    fn list(&self, dir: &Path) -> std::io::Result<Vec<(OsString, Found)>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else { continue };
            let (name, path) = (entry.file_name(), entry.path());
            if self.filter.skips(&path, &name) {
                continue;
            }
            // Links are followed, as chokidar does; broken ones are left out.
            let link = file_type.is_symlink();
            let is_dir = if link {
                match std::fs::metadata(&path) {
                    Ok(meta) => meta.is_dir(),
                    Err(_) => continue,
                }
            } else {
                file_type.is_dir()
            };
            if is_dir {
                found.push((name, Found::Folder { link }));
            } else if crate::is_markdown(&path) {
                found.push((name, Found::File));
            }
        }
        Ok(found)
    }

    // A linked folder is shown when its real path lies outside the root and
    // outside every other linked folder shown, and holds none of them. A
    // folder reached twice would be watched twice, which inotify cannot do
    // (both paths share one watch, reported under one of them), and a link
    // back to a folder above would be listed forever. Unlike chokidar, links
    // to folders inside the root are therefore left out.
    fn follow(&mut self, link: &Path) -> bool {
        let Ok(target) = std::fs::canonicalize(link) else { return false };
        let overlaps = |shown: &PathBuf| shown.starts_with(&target) || target.starts_with(shown);
        if overlaps(&self.real_root) || self.links.values().any(overlaps) {
            return false;
        }
        self.links.insert(link.to_path_buf(), target);
        true
    }

    // Tells the page about a new entry and returns what it now knows of it.
    fn announce(&mut self, path: &Path, name: &OsStr, found: Found, out: &mut impl Out) -> Option<Known> {
        match found {
            Found::Folder { link } => {
                if link && !self.follow(path) {
                    return None;
                }
                out.push("addDir", folder_change(path, name));
                Some(Known::Folder)
            }
            Found::File => {
                let times = times(path)?;
                out.push("add", file_change(path, name, times));
                Some(Known::File { mtime_ms: times.mtime_ms })
            }
        }
    }

    // Lists a folder the page has not seen yet, and everything below it.
    fn add_folder(&mut self, dir: PathBuf, out: &mut impl Out) {
        let mut pending = vec![dir];
        while let Some(dir) = pending.pop() {
            if out.stopped() {
                return;
            }
            // Watched before it is listed, so that nothing created meanwhile is missed.
            self.watches.add(&dir);
            let mut entries = HashMap::new();
            for (name, found) in self.list(&dir).unwrap_or_default() {
                let path = dir.join(&name);
                if let Some(known) = self.announce(&path, &name, found, out) {
                    if known == Known::Folder {
                        pending.push(path);
                    }
                    entries.insert(name, known);
                }
            }
            self.folders.insert(dir, entries);
        }
    }

    // Forgets an entry and, for a folder, everything below it, children first.
    fn remove(&mut self, path: &Path, known: Known, out: &mut impl Out) {
        match known {
            Known::File { .. } => out.push("unlink", json!({ "pathname": display(path) })),
            Known::Folder => {
                for (name, known) in self.folders.remove(path).unwrap_or_default() {
                    self.remove(&path.join(name), known, out);
                }
                self.watches.remove(path);
                self.links.remove(path);
                out.push("unlinkDir", json!({ "pathname": display(path) }));
            }
        }
    }

    fn note(&self, event: &notify::Event, round: &mut Round) {
        round.events += 1;
        round.everything |= event.need_rescan();
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let gone = matches!(event.kind, EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)));
        for path in &event.paths {
            if self.folders.contains_key(path) {
                // Windows reports a change inside a folder on the folder too.
                round.dirs.insert(path.clone());
                if gone {
                    round.gone.insert(path.clone());
                }
            }
            if let Some(parent) = path.parent().filter(|parent| self.folders.contains_key(*parent)) {
                round.dirs.insert(parent.to_path_buf());
            }
            round.touched.insert(path.clone());
        }
    }

    // Brings the page up to date after a round of events. Removals go first,
    // so that a folder replaced under the same name is unwatched before its
    // successor is watched.
    fn refresh(&mut self, round: &Round, out: &mut impl Out) {
        let everything = round.everything || (!PER_FOLDER && round.events >= BUSY);
        let all_folders: HashSet<PathBuf>;
        let dirs = if everything {
            all_folders = self.folders.keys().cloned().collect();
            &all_folders
        } else {
            &round.dirs
        };
        let mut removed = Vec::new();
        let mut added = Vec::new();
        let mut checked = Vec::new();
        for dir in dirs {
            let Some(entries) = self.folders.get(dir) else { continue };
            let listing: HashMap<OsString, Found> = match self.list(dir) {
                Ok(listing) => listing.into_iter().collect(),
                Err(err) if err.kind() == ErrorKind::NotFound => HashMap::new(),
                Err(_) => continue,
            };
            for (name, &known) in entries {
                let path = dir.join(name);
                match (known, listing.get(name)) {
                    (Known::Folder, Some(Found::Folder { .. })) if !round.gone.contains(&path) => {}
                    (Known::File { .. }, Some(Found::File)) => {
                        if everything || round.touched.contains(&path) {
                            checked.push(path);
                        }
                    }
                    (_, found) => {
                        removed.push(path);
                        if let Some(&found) = found {
                            added.push((dir.clone(), name.clone(), found));
                        }
                    }
                }
            }
            for (name, &found) in &listing {
                if !entries.contains_key(name) {
                    added.push((dir.clone(), name.clone(), found));
                }
            }
        }

        for path in removed {
            let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { continue };
            // Not found when it went with a folder removed earlier in this round.
            let Some(known) = self.folders.get_mut(dir).and_then(|entries| entries.remove(name)) else { continue };
            self.remove(&path, known, out);
        }
        // A folder that went in this round, even if it is back, is listed
        // whole when it is added again.
        added.retain(|(dir, _, _)| self.folders.contains_key(dir));
        for (dir, name, found) in added {
            let path = dir.join(&name);
            let Some(known) = self.announce(&path, &name, found, out) else { continue };
            if let Some(entries) = self.folders.get_mut(&dir) {
                entries.insert(name, known);
            }
            if known == Known::Folder {
                self.add_folder(path, out);
            }
        }
        for path in checked {
            let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { continue };
            let Some(times) = times(&path) else { continue };
            let Some(known) = self.folders.get_mut(dir).and_then(|entries| entries.get_mut(name)) else { continue };
            if *known != (Known::File { mtime_ms: times.mtime_ms }) {
                *known = Known::File { mtime_ms: times.mtime_ms };
                out.push("change", json!({ "pathname": display(&path), "mtimeMs": times.mtime_ms }));
            }
        }
    }
}

#[derive(Clone, Serialize)]
struct Batch<'a> {
    generation: u64,
    // On the first batch only: the page opens the folder in the renderer.
    root: Option<&'a str>,
    changes: &'a [Value],
}

// Sends a window the changes of its folder. Generations grow in the order
// folders are opened, so the page can drop what is left over from a folder it
// has since replaced.
struct Page {
    app: AppHandle,
    window: u32,
    generation: u64,
    stop: Arc<AtomicBool>,
    changes: Vec<Value>,
}

impl Page {
    fn send(&mut self, root: Option<&str>) {
        let changes = std::mem::take(&mut self.changes);
        if self.stopped() || (root.is_none() && changes.is_empty()) {
            return;
        }
        let batch = Batch { generation: self.generation, root, changes: &changes };
        if let Err(err) = self.app.emit_to(label(self.window), FOLDER_EVENT, batch) {
            eprintln!("[folders] {err}");
        }
    }

    fn report_limit(&self, watches: &mut Watches) {
        let Some(message) = watches.limit.take() else { return };
        if let Some(window) = self.app.get_webview_window(&label(self.window)) {
            report_watch_limit(&window, message);
        }
    }
}

impl Out for Page {
    fn push(&mut self, kind: &'static str, change: Value) {
        self.changes.push(json!({ "type": kind, "change": change }));
        if self.changes.len() >= BATCH {
            self.send(None);
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

fn run(app: AppHandle, window: u32, generation: u64, root: PathBuf, stop: Arc<AtomicBool>) {
    let (sender, events) = std::sync::mpsc::channel();
    let watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .inspect_err(|err| eprintln!("[folders] cannot watch folders: {err}"))
    .ok();
    let filter = Filter::new(exclude_patterns(&app));
    let mut page = Page { app, window, generation, stop, changes: Vec::new() };
    page.send(Some(&display(&root)));

    let mut tree = Tree::new(root.clone(), filter, watcher);
    tree.watches.start(&root);
    tree.add_folder(root, &mut page);
    page.send(None);
    page.report_limit(&mut tree.watches);

    let mut round = Round::default();
    let mut since: Option<Instant> = None;
    loop {
        let received = events.recv_timeout(QUIET);
        if page.stopped() {
            return;
        }
        let quiet = match received {
            Ok(Ok(event)) => {
                tree.note(&event, &mut round);
                since.get_or_insert_with(Instant::now);
                false
            }
            Ok(Err(err)) => {
                eprintln!("[folders] {err}");
                false
            }
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let Some(first) = since else { continue };
        if !quiet && first.elapsed() < LONGEST {
            continue;
        }
        since = None;
        let patterns = exclude_patterns(&page.app);
        if patterns != tree.filter.source {
            tree.filter = Filter::new(patterns);
        }
        tree.refresh(&std::mem::take(&mut round), &mut page);
        page.send(None);
        page.report_limit(&mut tree.watches);
    }
}

struct Running {
    root: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

// Each editor window's folder and the thread that follows it.
#[derive(Default)]
pub struct Folders(Mutex<HashMap<u32, Running>>);

// editor.ts openFolder compares with isSamePathSync.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

impl Folders {
    fn lock(&self) -> MutexGuard<'_, HashMap<u32, Running>> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Shows `root` in the window's sidebar in place of its folder, if any.
    // Returns false when the window shows `root` already.
    pub fn open(&self, app: &AppHandle, window: u32, root: PathBuf) -> bool {
        let mut running = self.lock();
        if running.get(&window).is_some_and(|running| same_path(&running.root, &root)) {
            return false;
        }
        let stop = Arc::new(AtomicBool::new(false));
        running.insert(window, Running { root: root.clone(), stop: stop.clone() });
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        let app = app.clone();
        std::thread::spawn(move || run(app, window, generation, root, stop));
        true
    }

    pub fn close(&self, window: u32) {
        self.lock().remove(&window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::RemoveKind;

    #[derive(Default)]
    struct Log(Vec<(String, String)>);

    impl Out for Log {
        fn push(&mut self, kind: &'static str, change: Value) {
            self.0.push((kind.to_string(), change["pathname"].as_str().unwrap().to_string()));
        }

        fn stopped(&self) -> bool {
            false
        }
    }

    impl Log {
        fn take(&mut self, root: &Path) -> Vec<(String, String)> {
            let prefix = format!("{}{}", root.display(), std::path::MAIN_SEPARATOR);
            let mut log: Vec<_> = std::mem::take(&mut self.0)
                .into_iter()
                .map(|(kind, path)| (kind, path.strip_prefix(&prefix).unwrap().replace('\\', "/")))
                .collect();
            log.sort();
            log
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-folders-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, path: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    fn entries(list: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut list: Vec<_> = list.iter().map(|(kind, path)| (kind.to_string(), path.to_string())).collect();
        list.sort();
        list
    }

    fn listed(root: &Path, exclude: &[&str]) -> (Tree, Log) {
        let filter = Filter::new(exclude.iter().map(|pattern| pattern.to_string()).collect());
        let mut tree = Tree::new(root.to_path_buf(), filter, None);
        let mut log = Log::default();
        tree.add_folder(root.to_path_buf(), &mut log);
        (tree, log)
    }

    fn touch_later(path: &Path) {
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(later).unwrap();
    }

    #[test]
    fn the_filter_follows_electron() {
        let filter = Filter::new(vec!["drafts".into(), "*.tmp.md".into(), "**/private/*.md".into()]);
        let skips = |path: &str| filter.skips(Path::new(path), Path::new(path).file_name().unwrap());
        assert!(skips("/r/node_modules"));
        assert!(skips("/r/app.asar"));
        assert!(!skips("/r/.asar"));
        assert!(skips("/r/drafts"));
        assert!(skips("/r/x/note.tmp.md"));
        assert!(skips("/r/private/a.md"));
        assert!(!skips("/r/private"));
        assert!(!skips("/r/notes/a.md"));
    }

    #[test]
    fn listing_shows_folders_and_markdown_files() {
        let root = temp_dir("list");
        for file in ["a.md", "b.txt", "image.png", "node_modules/x.md", "app.asar", "sub/c.md", "sub/deeper/d.markdown", "drafts/e.md"] {
            write(&root, file);
        }
        std::fs::create_dir_all(root.join("empty")).unwrap();

        let (_, mut log) = listed(&root, &["drafts"]);
        assert_eq!(
            log.take(&root),
            entries(&[
                ("add", "a.md"),
                ("add", "b.txt"),
                ("add", "sub/c.md"),
                ("add", "sub/deeper/d.markdown"),
                ("addDir", "empty"),
                ("addDir", "sub"),
                ("addDir", "sub/deeper"),
            ])
        );
    }

    #[test]
    fn listing_again_reports_the_differences() {
        let root = temp_dir("refresh");
        for file in ["a.md", "b.md", "sub/c.md", "sub/deeper/d.md"] {
            write(&root, file);
        }
        let (mut tree, mut log) = listed(&root, &[]);
        log.take(&root);

        std::fs::remove_file(root.join("a.md")).unwrap();
        write(&root, "new.md");
        std::fs::rename(root.join("sub"), root.join("moved")).unwrap();
        touch_later(&root.join("b.md"));

        tree.refresh(&Round { everything: true, ..Round::default() }, &mut log);
        let position = |path: &str| {
            let path = display(&root.join(path.replace('/', std::path::MAIN_SEPARATOR_STR)));
            log.0.iter().position(|(_, logged)| *logged == path).unwrap()
        };
        assert!(position("sub/c.md") < position("sub"), "children go first: {:?}", log.0);
        assert_eq!(
            log.take(&root),
            entries(&[
                ("add", "moved/c.md"),
                ("add", "moved/deeper/d.md"),
                ("add", "new.md"),
                ("addDir", "moved"),
                ("addDir", "moved/deeper"),
                ("change", "b.md"),
                ("unlink", "a.md"),
                ("unlink", "sub/c.md"),
                ("unlink", "sub/deeper/d.md"),
                ("unlinkDir", "sub"),
                ("unlinkDir", "sub/deeper"),
            ])
        );

        tree.refresh(&Round { everything: true, ..Round::default() }, &mut log);
        assert_eq!(log.take(&root), vec![]);
    }

    #[test]
    fn only_touched_files_are_checked_for_changes() {
        let root = temp_dir("touched");
        write(&root, "a.md");
        write(&root, "b.md");
        let (mut tree, mut log) = listed(&root, &[]);
        log.take(&root);

        touch_later(&root.join("a.md"));
        touch_later(&root.join("b.md"));
        let mut round = Round::default();
        let event = notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join("a.md"));
        tree.note(&event, &mut round);
        tree.refresh(&round, &mut log);
        assert_eq!(log.take(&root), entries(&[("change", "a.md")]));
    }

    #[test]
    fn a_folder_replaced_under_the_same_name_is_listed_again() {
        let root = temp_dir("replaced");
        write(&root, "docs/old.md");
        write(&root, "docs/inner/kept.md");
        let (mut tree, mut log) = listed(&root, &[]);
        log.take(&root);

        std::fs::remove_dir_all(root.join("docs")).unwrap();
        write(&root, "docs/new.md");
        write(&root, "docs/inner/kept.md");
        let mut round = Round::default();
        let event = notify::Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(root.join("docs"));
        tree.note(&event, &mut round);
        tree.refresh(&round, &mut log);
        assert_eq!(
            log.take(&root),
            entries(&[
                ("add", "docs/inner/kept.md"),
                ("add", "docs/new.md"),
                ("addDir", "docs"),
                ("addDir", "docs/inner"),
                ("unlink", "docs/inner/kept.md"),
                ("unlink", "docs/old.md"),
                ("unlinkDir", "docs"),
                ("unlinkDir", "docs/inner"),
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_links_to_folders_outside_the_tree_are_followed() {
        let root = temp_dir("links");
        let outside = temp_dir("links-outside");
        write(&root, "sub/a.md");
        write(&root, "b/x.md");
        write(&outside, "y.md");
        write(&outside, "inner/z.md");
        std::os::unix::fs::symlink(".", root.join("self")).unwrap();
        std::os::unix::fs::symlink("..", root.join("sub/up")).unwrap();
        std::os::unix::fs::symlink("../b", root.join("sub/to-b")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("ext")).unwrap();
        std::os::unix::fs::symlink(&root, outside.join("back")).unwrap();

        let (mut tree, mut log) = listed(&root, &[]);
        assert_eq!(
            log.take(&root),
            entries(&[
                ("add", "b/x.md"),
                ("add", "ext/inner/z.md"),
                ("add", "ext/y.md"),
                ("add", "sub/a.md"),
                ("addDir", "b"),
                ("addDir", "ext"),
                ("addDir", "ext/inner"),
                ("addDir", "sub"),
            ])
        );

        // A link to a folder inside ext is shown only once ext goes.
        std::os::unix::fs::symlink(outside.join("inner"), root.join("ext-inner")).unwrap();
        tree.refresh(&Round { everything: true, ..Round::default() }, &mut log);
        assert_eq!(log.take(&root), vec![]);
        std::fs::remove_file(root.join("ext")).unwrap();
        tree.refresh(&Round { everything: true, ..Round::default() }, &mut log);
        assert_eq!(
            log.take(&root),
            entries(&[
                ("add", "ext-inner/z.md"),
                ("addDir", "ext-inner"),
                ("unlink", "ext/inner/z.md"),
                ("unlink", "ext/y.md"),
                ("unlinkDir", "ext"),
                ("unlinkDir", "ext/inner"),
            ])
        );
    }
}
