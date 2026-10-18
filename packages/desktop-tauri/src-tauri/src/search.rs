// Find in folder (the sidebar search) and quick open, standing in for
// packages/desktop/src/main/ipc/ripgrep.ts. Electron runs the rg binary and
// reads its JSON output; here the crates rg is built from search inside the
// app, with the settings ripgrep.ts passes on rg's command line, and the
// window gets the same `mt::rg::*` events and payloads. Each search runs on a
// thread of its own, one folder after another, where Electron runs one rg per
// folder at once.

use std::collections::HashMap;
use std::io;
use std::path::MAIN_SEPARATOR;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::overrides::{Override, OverrideBuilder};
use ignore::{DirEntry, Walk, WalkBuilder};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{State, WebviewWindow};

use crate::display;
use crate::windows;

const PROGRESS: &str = "mt::rg::progress";
const MATCH: &str = "mt::rg::match";
const DONE: &str = "mt::rg::done";
const ERROR: &str = "mt::rg::error";
const CANCELLED: &str = "mt::rg::cancelled";

// ripgrepSearcher.ts startSearch. `mode` is "files" for quick open; anything
// else searches text.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    search_id: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    directories: Vec<String>,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    options: Value,
}

// ripgrep.ts SearchOptions. The context line counts are left out: ripgrep.ts
// asks rg for context lines but drops them, so every match has empty ones.
#[derive(Default)]
struct Options {
    is_regexp: bool,
    is_case_sensitive: bool,
    is_whole_word: bool,
    follow_symlinks: bool,
    // As written on rg's command line, e.g. "10M".
    max_file_size: Option<String>,
    include_hidden: bool,
    no_ignore: bool,
    inclusions: Vec<String>,
    exclusions: Vec<String>,
}

// ripgrep.ts tests each option as JavaScript would.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

impl Options {
    fn from_value(options: &Value) -> Self {
        let flag = |name: &str| truthy(options.get(name));
        let globs = |name: &str| -> Vec<String> {
            let list = options.get(name).and_then(Value::as_array);
            list.map(|list| list.iter().filter_map(|glob| glob.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let max_file_size = options.get("maxFileSize").filter(|value| truthy(Some(value))).map(|value| match value {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.as_u64().map_or_else(|| number.to_string(), |n| n.to_string()),
            other => other.to_string(),
        });
        Self {
            is_regexp: flag("isRegexp"),
            is_case_sensitive: flag("isCaseSensitive"),
            is_whole_word: flag("isWholeWord"),
            follow_symlinks: flag("followSymlinks"),
            max_file_size,
            include_hidden: flag("includeHidden"),
            no_ignore: flag("noIgnore"),
            inclusions: globs("inclusions"),
            exclusions: globs("exclusions"),
        }
    }
}

// rg's --max-filesize: a whole number of bytes with an optional K, M or G
// suffix (powers of 1024).
fn parse_size(text: &str) -> Result<u64, String> {
    let invalid = || format!("invalid max file size {text:?}");
    let (digits, multiplier) = match text.as_bytes().last() {
        Some(b'K') => (&text[..text.len() - 1], 1 << 10),
        Some(b'M') => (&text[..text.len() - 1], 1 << 20),
        Some(b'G') => (&text[..text.len() - 1], 1 << 30),
        _ => (text, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let number: u64 = digits.parse().map_err(|_| invalid())?;
    number.checked_mul(multiplier).ok_or_else(invalid)
}

// Node's path.basename, splitting on `/` and `sep`.
fn basename(path: &str, sep: char) -> &str {
    let is_sep = |c: char| c == '/' || c == sep;
    let trimmed = path.trim_end_matches(is_sep);
    trimmed.rsplit(is_sep).next().unwrap_or("")
}

// ripgrep.ts prepareGlobs, quirks included: a glob naming the folder itself
// matches everything, a leading folder name is dropped, and every glob also
// covers whatever lies below a folder it matches.
fn prepare_globs(globs: &[String], root: &str, sep: char) -> Vec<String> {
    let project = basename(root, sep);
    let mut output = Vec::new();
    for glob in globs {
        let mut pattern = glob.replace(sep, "/");
        if pattern.is_empty() {
            continue;
        }
        if pattern == project {
            output.push("**/*".to_string());
            continue;
        }
        if let Some(rest) = pattern.strip_prefix(&format!("{project}/")) {
            pattern = rest.to_string();
        }
        if let Some(rest) = pattern.strip_suffix('/') {
            pattern = rest.to_string();
        }
        if !pattern.starts_with("**/") {
            pattern = format!("**/{pattern}");
        }
        let below = if pattern.ends_with("/**") { pattern.clone() } else { format!("{pattern}/**") };
        output.push(pattern);
        output.push(below);
    }
    output
}

// ripgrep.ts prepareRegexp.
fn prepare_regexp(pattern: &str) -> String {
    if pattern == "--" {
        return "\\-\\-".to_string();
    }
    pattern.replace("\\/", "/")
}

// rg's --iglob, in the order ripgrep.ts passes them: later globs win, and
// with any inclusion a file has to match one.
fn overrides(root: &str, inclusions: &[String], exclusions: &[String]) -> Result<Override, ignore::Error> {
    let mut builder = OverrideBuilder::new(root);
    builder.case_insensitive(true)?;
    for glob in inclusions {
        builder.add(glob)?;
    }
    for glob in exclusions {
        builder.add(&format!("!{glob}"))?;
    }
    builder.build()
}

// The walk rg does by default, with --follow, --hidden and --no-ignore as asked.
fn walk(root: &str, options: &Options, overrides: Override, max_file_size: Option<u64>) -> Walk {
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(options.follow_symlinks)
        .hidden(!options.include_hidden)
        .max_filesize(max_file_size)
        .overrides(overrides);
    if options.no_ignore {
        builder.ignore(false).git_ignore(false).git_global(false).git_exclude(false).parents(false);
    } else {
        builder.add_custom_ignore_filename(".rgignore");
    }
    builder.build()
}

// rg searches files, and links to files when it follows links.
fn is_file(entry: &DirEntry) -> bool {
    entry.file_type().is_some_and(|file_type| file_type.is_file())
}

// rg's settings for a pattern from the command line ripgrep.ts builds.
fn build_matcher(pattern: &str, options: &Options, multiline: bool) -> Result<RegexMatcher, grep_regex::Error> {
    let mut builder = RegexMatcherBuilder::new();
    builder
        .multi_line(true)
        .unicode(true)
        .octal(false)
        .fixed_strings(!options.is_regexp)
        .case_insensitive(!options.is_case_sensitive)
        .word(options.is_whole_word);
    if !multiline {
        builder.line_terminator(Some(b'\n'));
    }
    builder.build(pattern)
}

// ripgrep.ts processUnicodeMatch: byte offsets into `bytes`, in ascending
// order, as UTF-16 offsets into its text, which is what the renderer counts
// columns in. Each piece is decoded on its own, as Node's Buffer does there.
fn utf16_offsets(bytes: &[u8], offsets: &[usize]) -> Vec<usize> {
    let (mut previous, mut length) = (0, 0);
    offsets
        .iter()
        .map(|&offset| {
            let offset = offset.clamp(previous, bytes.len());
            length += String::from_utf8_lossy(&bytes[previous..offset]).encode_utf16().count();
            previous = offset;
            length
        })
        .collect()
}

// ripgrep.ts getPositionFromColumn: the row and column of a UTF-16 offset,
// given each line's length in UTF-16 units.
fn position(lines: &[usize], column: usize) -> (usize, usize) {
    let (mut current, mut previous, mut line) = (0, 0, 0);
    while column >= current {
        previous = current;
        current += lines.get(line).copied().unwrap_or(0) + 1;
        line += 1;
    }
    (line - 1, column - previous)
}

type Range = [[usize; 2]; 2];

// ripgrep.ts processSubmatch: where a submatch lies, with `first_row` the
// 0-based row of the text's first line, and the lines it shows. ripgrep.ts
// drops the lines above the submatch but then trims those below against the
// end row counted before the drop, so a submatch that starts below the first
// line keeps extra lines after it. Only multiline searches give more than one
// line.
fn locate(start: usize, end: usize, text: &str, first_row: usize) -> (Range, String) {
    let parts: Vec<&str> = text.split('\n').collect();
    let lengths: Vec<usize> = parts.iter().map(|part| part.encode_utf16().count()).collect();
    let (start_row, start_col) = position(&lengths, start);
    let (end_row, end_col) = position(&lengths, end);
    let mut kept = parts[start_row.min(parts.len())..].to_vec();
    while end_row + 1 < kept.len() {
        kept.pop();
    }
    let mut line_text = kept.join("\n");
    if line_text.ends_with('\n') {
        line_text.pop();
    }
    ([[start_row + first_row, start_col], [end_row + first_row, end_col]], line_text)
}

// Where a search's events go: to its window, or to a list in the tests.
trait Out {
    fn send(&mut self, channel: &'static str, payload: Value);
    // True once the search is cancelled.
    fn stopped(&self) -> bool;
}

// One file's matches, in the shape ripgrep.ts builds from rg's `match` messages.
struct Collect<'a, O: Out> {
    matcher: &'a RegexMatcher,
    // Whether the searcher reports matches that span lines.
    multi_line: bool,
    out: &'a O,
    matches: Vec<Value>,
}

impl<O: Out> Sink for Collect<'_, O> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, found: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.out.stopped() {
            return Ok(false);
        }
        // Submatches as rg's JSON printer finds them: within the matched
        // lines, with the text before them as context for look-behind, and
        // without the line ending unless the search spans lines.
        let lines = found.bytes();
        let buffer = found.buffer();
        let range = found.bytes_range_in_buffer();
        let mut end = range.end;
        if !self.multi_line && buffer[..end].ends_with(b"\n") {
            end -= 1;
        }
        let mut submatches = Vec::new();
        let _ = self.matcher.find_iter_at(&buffer[..end], range.start, |m| {
            submatches.push((m.start() - range.start, m.end() - range.start));
            true
        });
        // rg leaves out an empty match at the very end.
        if submatches.last().is_some_and(|&(start, end)| start == end && start >= lines.len()) {
            submatches.pop();
        }

        let first_row = found.line_number().unwrap_or(1).saturating_sub(1) as usize;
        let text = String::from_utf8_lossy(lines);
        let offsets: Vec<usize> = submatches.iter().flat_map(|&(start, end)| [start, end]).collect();
        let columns = utf16_offsets(lines, &offsets);
        // A multiline match's lines grow with each submatch, so a big one can
        // take a while; check for a cancel as it goes.
        for (index, &(start, end)) in submatches.iter().enumerate() {
            if self.out.stopped() {
                return Ok(false);
            }
            let (range, line_text) = locate(columns[2 * index], columns[2 * index + 1], &text, first_row);
            self.matches.push(json!({
                "matchText": String::from_utf8_lossy(lines.get(start..end).unwrap_or_default()),
                "lineText": line_text,
                "range": range,
                "leadingContextLines": [],
                "trailingContextLines": []
            }));
        }
        Ok(true)
    }
}

// ripgrep.ts startTextSearch: each file with matches gets a progress event
// with the count of such files so far, then its matches. As there, a pattern
// or setting rg would refuse ends the search with no results rather than an
// error.
fn search_text(id: &str, directories: &[String], pattern: &str, options: &Options, out: &mut impl Out) {
    let max_file_size = match options.max_file_size.as_deref().map(parse_size).transpose() {
        Ok(size) => size,
        Err(err) => return eprintln!("[search] {err}"),
    };
    // Patterns span lines only when a regex asks for a line break.
    let (pattern, multiline) = if options.is_regexp {
        let regexp = prepare_regexp(pattern);
        let multiline = regexp.contains("\\n");
        (regexp, multiline)
    } else {
        (pattern.to_string(), false)
    };
    // ripgrep.ts would leave an empty fixed string off rg's command line.
    if !options.is_regexp && pattern.is_empty() {
        return;
    }
    let matcher = match build_matcher(&pattern, options, multiline) {
        Ok(matcher) => matcher,
        Err(err) => return eprintln!("[search] {err}"),
    };
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .multi_line(multiline)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();
    let multi_line = searcher.multi_line_with_matcher(&matcher);

    let mut found = 0;
    for root in directories {
        let inclusions = prepare_globs(&options.inclusions, root, MAIN_SEPARATOR);
        let exclusions = prepare_globs(&options.exclusions, root, MAIN_SEPARATOR);
        let overrides = match overrides(root, &inclusions, &exclusions) {
            Ok(overrides) => overrides,
            Err(err) => {
                eprintln!("[search] {err}");
                continue;
            }
        };
        for entry in walk(root, options, overrides, max_file_size) {
            if out.stopped() {
                return;
            }
            let Ok(entry) = entry else { continue };
            if !is_file(&entry) {
                continue;
            }
            let mut sink = Collect { matcher: &matcher, multi_line, out: &*out, matches: Vec::new() };
            if let Err(err) = searcher.search_path(&matcher, entry.path(), &mut sink) {
                eprintln!("[search] {}: {err}", entry.path().display());
            }
            let matches = sink.matches;
            if matches.is_empty() || out.stopped() {
                continue;
            }
            found += 1;
            out.send(PROGRESS, json!({ "searchId": id, "num": found }));
            let payload = json!({ "filePath": display(entry.path()), "matches": matches });
            out.send(MATCH, json!({ "searchId": id, "payload": payload }));
        }
    }
}

// ripgrep.ts startFileSearch (`rg --files`): each file the inclusions match is
// a progress event and a match whose payload is the path. As rg exits with an
// error then, a folder that could not be read fails the search once its walk
// is over.
fn search_files(id: &str, directories: &[String], options: &Options, out: &mut impl Out) -> Result<(), String> {
    let mut found = 0;
    for root in directories {
        let inclusions = prepare_globs(&options.inclusions, root, MAIN_SEPARATOR);
        let overrides = overrides(root, &inclusions, &[]).map_err(|err| err.to_string())?;
        let mut errors = Vec::new();
        for entry in walk(root, options, overrides, None) {
            if out.stopped() {
                return Ok(());
            }
            match entry {
                Ok(entry) if is_file(&entry) => {
                    found += 1;
                    out.send(PROGRESS, json!({ "searchId": id, "num": found }));
                    out.send(MATCH, json!({ "searchId": id, "payload": display(entry.path()) }));
                }
                Ok(_) => {}
                Err(err) => errors.push(err.to_string()),
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
    }
    Ok(())
}

type Sender = Box<dyn Fn(&'static str, Value) + Send + Sync>;

struct Search {
    id: String,
    stop: AtomicBool,
    // Set once done, error or cancelled has gone out. Events are sent under
    // this lock, so cancelled is the last one the renderer gets.
    finished: Mutex<bool>,
    send: Sender,
}

impl Search {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Sends done or error, unless the search was cancelled.
    fn finish(&self, channel: &'static str, payload: Value) {
        let mut finished = self.lock();
        if !*finished && !self.stopped() {
            *finished = true;
            (self.send)(channel, payload);
        }
    }

    // Stops the search at once, as killing rg does; the thread notices at the
    // next file or match.
    fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut finished = self.lock();
        if !*finished {
            *finished = true;
            (self.send)(CANCELLED, json!({ "searchId": self.id }));
        }
    }

    fn run(&self, request: &Request) {
        let options = Options::from_value(&request.options);
        let mut out = self;
        let result = if request.mode == "files" {
            search_files(&self.id, &request.directories, &options, &mut out)
        } else {
            search_text(&self.id, &request.directories, &request.pattern, &options, &mut out);
            Ok(())
        };
        match result {
            Ok(()) => self.finish(DONE, json!({ "searchId": self.id })),
            Err(error) => self.finish(ERROR, json!({ "searchId": self.id, "error": error })),
        }
    }
}

impl Out for &Search {
    fn send(&mut self, channel: &'static str, payload: Value) {
        let finished = self.lock();
        if !*finished && !self.stopped() {
            (self.send)(channel, payload);
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

type Running = HashMap<(u32, String), Arc<Search>>;

// The searches running in each editor window, by window and search id.
#[derive(Default)]
pub struct Searches(Arc<Mutex<Running>>);

fn lock(running: &Mutex<Running>) -> MutexGuard<'_, Running> {
    running.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Searches {
    fn start(&self, window: u32, request: Request, send: Sender) -> bool {
        let id = request.search_id.clone();
        let search = Arc::new(Search { id: id.clone(), stop: AtomicBool::new(false), finished: Mutex::new(false), send });
        let key = (window, id);
        // A search under the same id could no longer be cancelled.
        if let Some(previous) = lock(&self.0).insert(key.clone(), search.clone()) {
            previous.stop.store(true, Ordering::Relaxed);
        }
        let running = self.0.clone();
        std::thread::spawn(move || {
            search.run(&request);
            let mut running = lock(&running);
            if running.get(&key).is_some_and(|current| Arc::ptr_eq(current, &search)) {
                running.remove(&key);
            }
        });
        true
    }

    fn cancel(&self, window: u32, id: &str) {
        let search = lock(&self.0).remove(&(window, id.to_string()));
        if let Some(search) = search {
            search.cancel();
        }
    }

    // Stops a closed window's searches, without events (ripgrep.ts
    // cleanupAtSenderDestroy).
    pub fn close(&self, window: u32) {
        lock(&self.0).retain(|(owner, _), search| {
            if *owner == window {
                search.stop.store(true, Ordering::Relaxed);
            }
            *owner != window
        });
    }
}

// ripgrep.ts mt::rg::start. False for a window that is not an editor.
#[tauri::command]
pub fn search_start(window: WebviewWindow, searches: State<'_, Searches>, request: Request) -> bool {
    let Some(id) = windows::id_from_label(window.label()) else { return false };
    let send = move |channel, payload| windows::send_to_renderer(&window, channel, vec![payload]);
    searches.start(id, request, Box::new(send))
}

// ripgrep.ts mt::rg::cancel. Kept off the main thread: cancelling waits for an
// event the search thread may be sending, which must never need the main
// thread to finish.
#[tauri::command(async)]
pub fn search_cancel(window: WebviewWindow, searches: State<'_, Searches>, search_id: String) {
    if let Some(id) = windows::id_from_label(window.label()) {
        searches.cancel(id, &search_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Duration;

    // Events in order; stops the search after `limit` matches, if set.
    #[derive(Default)]
    struct Log {
        events: Vec<(&'static str, Value)>,
        limit: Option<usize>,
    }

    impl Out for Log {
        fn send(&mut self, channel: &'static str, payload: Value) {
            self.events.push((channel, payload));
        }

        fn stopped(&self) -> bool {
            self.limit.is_some_and(|limit| self.events.iter().filter(|(channel, _)| *channel == MATCH).count() >= limit)
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-search-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, path: &str, contents: impl AsRef<[u8]>) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn relative(root: &Path, path: &str) -> String {
        let prefix = format!("{}{}", root.display(), MAIN_SEPARATOR);
        path.strip_prefix(&prefix).unwrap().replace('\\', "/")
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    // Each file with matches, by path relative to `root`, with its matches.
    fn text(root: &Path, pattern: &str, options: Value) -> Vec<(String, Vec<Value>)> {
        let mut log = Log::default();
        search_text("s1", &[display(root)], pattern, &Options::from_value(&options), &mut log);
        let mut files = Vec::new();
        for (index, (channel, payload)) in log.events.iter().enumerate() {
            assert_eq!(payload["searchId"], "s1");
            if *channel == PROGRESS {
                assert_eq!(payload["num"], files.len() + 1);
                assert_eq!(log.events[index + 1].0, MATCH);
            } else {
                let payload = &payload["payload"];
                let matches = payload["matches"].as_array().unwrap().clone();
                files.push((relative(root, payload["filePath"].as_str().unwrap()), matches));
            }
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    }

    fn names(files: &[(String, Vec<Value>)]) -> Vec<&str> {
        files.iter().map(|(name, _)| name.as_str()).collect()
    }

    fn ranges(matches: &[Value]) -> Vec<String> {
        matches.iter().map(|m| m["range"].to_string()).collect()
    }

    #[test]
    fn utf16_offsets_count_code_units() {
        let text = "😀a中b";
        assert_eq!(utf16_offsets(text.as_bytes(), &[4, 5, 8, 9]), vec![2, 3, 4, 5]);
        assert_eq!(utf16_offsets(b"plain", &[1, 3]), vec![1, 3]);
        let line = "中文 café 😀😀 x";
        let at = line.find('x').unwrap();
        assert_eq!(utf16_offsets(line.as_bytes(), &[0, at]), vec![0, 13]);
    }

    #[test]
    fn locate_follows_ripgrep_ts() {
        assert_eq!(locate(2, 4, "abcdef\n", 9), ([[9, 2], [9, 4]], "abcdef".to_string()));
        // A submatch across two lines shows both.
        assert_eq!(locate(1, 6, "abc\ndef\n", 0), ([[0, 1], [1, 2]], "abc\ndef".to_string()));
        // One on the second of three lines keeps the line after it too.
        assert_eq!(locate(5, 6, "abc\ndef\nghi\n", 0), ([[1, 1], [1, 2]], "def\nghi".to_string()));
    }

    #[test]
    fn prepare_globs_follows_ripgrep_ts() {
        let root = "/home/me/notes";
        let globs = |list: &[&str]| prepare_globs(&strings(list), root, '/');
        assert_eq!(globs(&["*.md"]), strings(&["**/*.md", "**/*.md/**"]));
        assert_eq!(globs(&["notes"]), strings(&["**/*"]));
        assert_eq!(globs(&["notes/drafts/"]), strings(&["**/drafts", "**/drafts/**"]));
        assert_eq!(globs(&["**/build/**"]), strings(&["**/build/**", "**/build/**"]));
        assert_eq!(globs(&["", "a/b"]), strings(&["**/a/b", "**/a/b/**"]));
        assert_eq!(prepare_globs(&strings(&["notes"]), "/home/me/notes/", '/'), strings(&["**/*"]));
        // The root folder has no name, so a leading slash goes.
        assert_eq!(prepare_globs(&strings(&["/abs"]), "/", '/'), strings(&["**/abs", "**/abs/**"]));
        // Windows separators.
        let windows = prepare_globs(&strings(&["notes\\drafts\\", "a\\b"]), "C:\\Users\\me\\notes", '\\');
        assert_eq!(windows, strings(&["**/drafts", "**/drafts/**", "**/a/b", "**/a/b/**"]));
    }

    #[test]
    fn max_file_size_parses_like_rg() {
        assert_eq!(parse_size("100"), Ok(100));
        assert_eq!(parse_size("2K"), Ok(2048));
        assert_eq!(parse_size("3M"), Ok(3 << 20));
        assert_eq!(parse_size("1G"), Ok(1 << 30));
        for invalid in ["", "K", "1k", "1.5M", " 1M", "1MB", "-1", "99999999999999999999G"] {
            assert!(parse_size(invalid).is_err(), "{invalid}");
        }
        let options = |value: Value| Options::from_value(&json!({ "maxFileSize": value })).max_file_size;
        assert_eq!(options(json!("10M")), Some("10M".to_string()));
        assert_eq!(options(json!(2048)), Some("2048".to_string()));
        assert_eq!(options(json!("")), None);
        assert_eq!(options(Value::Null), None);
    }

    // Each test needs a folder of its own: tests run side by side.
    fn text_folder(name: &str) -> PathBuf {
        let root = temp_dir(name);
        write(&root, "case.md", "Hello world\nsay hello\n");
        write(&root, "word.md", "foobar foo\n");
        write(&root, "regex.md", "a.c abc\n");
        write(&root, "multi.md", "first line\nsecond line\n");
        write(&root, "unicode.md", "😀 café hello 中文 hello\n");
        write(&root, "binary.md", b"hello\x00world\n");
        write(&root, "sub/deep.md", "hello from below\n");
        write(&root, ".hidden.md", "secret\n");
        write(&root, "ignored.md", "secret\n");
        write(&root, ".ignore", "ignored.md\n");
        write(&root, "big.md", format!("hidden giant\n{}", "x".repeat(2000)));
        root
    }

    #[test]
    fn text_search_matches_ripgrep_ts() {
        let root = text_folder("text-matches");

        // Case-insensitive by default; binary files are skipped.
        let found = text(&root, "hello", json!({}));
        assert_eq!(names(&found), vec!["case.md", "sub/deep.md", "unicode.md"]);
        assert_eq!(
            found[0].1,
            vec![
                json!({ "matchText": "Hello", "lineText": "Hello world", "range": [[0, 0], [0, 5]], "leadingContextLines": [], "trailingContextLines": [] }),
                json!({ "matchText": "hello", "lineText": "say hello", "range": [[1, 4], [1, 9]], "leadingContextLines": [], "trailingContextLines": [] }),
            ]
        );
        // Columns count UTF-16 code units: the emoji is two.
        assert_eq!(ranges(&found[2].1), vec!["[[0,8],[0,13]]", "[[0,17],[0,22]]"]);
        assert_eq!(found[2].1[0]["lineText"], "😀 café hello 中文 hello");

        let found = text(&root, "hello", json!({ "isCaseSensitive": true }));
        assert_eq!(ranges(&found[0].1), vec!["[[1,4],[1,9]]"]);

        let found = text(&root, "foo", json!({}));
        assert_eq!(ranges(&found[0].1), vec!["[[0,0],[0,3]]", "[[0,7],[0,10]]"]);
        let found = text(&root, "foo", json!({ "isWholeWord": true }));
        assert_eq!(ranges(&found[0].1), vec!["[[0,7],[0,10]]"]);

        let found = text(&root, "a.c", json!({}));
        assert_eq!(ranges(&found[0].1), vec!["[[0,0],[0,3]]"]);
        let found = text(&root, "a.c", json!({ "isRegexp": true }));
        assert_eq!(ranges(&found[0].1), vec!["[[0,0],[0,3]]", "[[0,4],[0,7]]"]);

        // A line break in a regex searches across lines.
        let found = text(&root, "line\\nsecond", json!({ "isRegexp": true }));
        assert_eq!(names(&found), vec!["multi.md"]);
        assert_eq!(found[0].1[0]["matchText"], "line\nsecond");
        assert_eq!(found[0].1[0]["lineText"], "first line\nsecond line");
        assert_eq!(ranges(&found[0].1), vec!["[[0,6],[1,6]]"]);
        // An invalid regex finds nothing, as rg refuses it.
        assert!(text(&root, "(", json!({ "isRegexp": true })).is_empty());
    }

    #[test]
    fn text_search_honours_hidden_ignore_size_and_globs() {
        let root = text_folder("text-options");
        assert!(text(&root, "secret", json!({})).is_empty());
        assert_eq!(names(&text(&root, "secret", json!({ "includeHidden": true }))), vec![".hidden.md"]);
        assert_eq!(names(&text(&root, "secret", json!({ "noIgnore": true }))), vec!["ignored.md"]);
        let both = json!({ "includeHidden": true, "noIgnore": true });
        assert_eq!(names(&text(&root, "secret", both)), vec![".hidden.md", "ignored.md"]);

        assert_eq!(names(&text(&root, "giant", json!({}))), vec!["big.md"]);
        assert!(text(&root, "giant", json!({ "maxFileSize": "1K" })).is_empty());
        assert_eq!(names(&text(&root, "giant", json!({ "maxFileSize": "3K" }))), vec!["big.md"]);
        assert!(text(&root, "giant", json!({ "maxFileSize": "lots" })).is_empty());

        let found = text(&root, "hello", json!({ "exclusions": ["sub"], "inclusions": ["*.MD"] }));
        assert_eq!(names(&found), vec!["case.md", "unicode.md"]);
    }

    fn files(root: &Path, options: Value) -> Vec<String> {
        let mut log = Log::default();
        search_files("q", &[display(root)], &Options::from_value(&options), &mut log).unwrap();
        let mut found = Vec::new();
        for (index, (channel, payload)) in log.events.iter().enumerate() {
            if index % 2 == 0 {
                assert_eq!((*channel, &payload["num"]), (PROGRESS, &json!(index / 2 + 1)));
            } else {
                assert_eq!(*channel, MATCH);
                found.push(relative(root, payload["payload"].as_str().unwrap()));
            }
        }
        found.sort();
        found
    }

    #[test]
    fn files_mode_lists_what_the_inclusions_match() {
        let root = temp_dir("files");
        for file in ["notes.md", "sub/My-Note.MD", "other.md", "note.txt", ".hidden-note.md", "sub/x.md/inner.txt"] {
            write(&root, file, "");
        }
        // As with rg's --iglob, a file an inclusion names is listed even when hidden.
        let inclusions = json!({ "inclusions": ["*note*.md", "*x.md"] });
        let expected = strings(&[".hidden-note.md", "notes.md", "sub/My-Note.MD", "sub/x.md/inner.txt"]);
        assert_eq!(files(&root, inclusions), expected);
        assert_eq!(files(&root, json!({})).len(), 5);
        assert_eq!(files(&root, json!({ "includeHidden": true })).len(), 6);
    }

    #[test]
    fn a_stopped_search_sends_nothing_more() {
        let root = temp_dir("stop");
        for index in 0..5 {
            write(&root, &format!("{index}.md"), "hello\n");
        }
        let mut log = Log { limit: Some(1), ..Log::default() };
        search_text("s", &[display(&root)], "hello", &Options::default(), &mut log);
        assert_eq!(log.events.len(), 2);

        let mut log = Log { limit: Some(2), ..Log::default() };
        search_files("s", &[display(&root)], &Options::default(), &mut log).unwrap();
        assert_eq!(log.events.len(), 4);
    }

    #[test]
    fn cancel_ends_a_running_search() {
        let root = temp_dir("cancel");
        for index in 0..50 {
            write(&root, &format!("{index}.md"), "hello\n");
        }
        let (events, received) = mpsc::channel();
        let (opened, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        // The first event holds the search until the test lets it go on.
        let send: Sender = Box::new(move |channel, payload| {
            let first = channel == PROGRESS && payload["num"] == 1;
            events.send((channel, payload)).unwrap();
            if first {
                gate.lock().unwrap().recv().unwrap();
            }
        });
        let searches = Arc::new(Searches::default());
        let request = Request {
            search_id: "c".into(),
            mode: "text".into(),
            directories: vec![display(&root)],
            pattern: "hello".into(),
            options: json!({}),
        };
        assert!(searches.start(7, request, send));
        assert_eq!(received.recv().unwrap().0, PROGRESS);

        let canceller = searches.clone();
        let cancelling = std::thread::spawn(move || canceller.cancel(7, "c"));
        std::thread::sleep(Duration::from_millis(100));
        opened.send(()).unwrap();
        cancelling.join().unwrap();

        let mut rest = Vec::new();
        while let Ok((channel, _)) = received.recv_timeout(Duration::from_millis(500)) {
            rest.push(channel);
        }
        assert_eq!(rest.last(), Some(&CANCELLED), "{rest:?}");
        assert_eq!(rest.iter().filter(|channel| [DONE, ERROR, CANCELLED].contains(channel)).count(), 1);
        assert!(rest.iter().filter(|channel| **channel == PROGRESS).count() < 49, "{rest:?}");
        assert!(lock(&searches.0).is_empty());

        // Cancelling again, or a search that is not running, sends nothing.
        searches.cancel(7, "c");
        searches.cancel(7, "other");
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn closing_the_window_stops_its_searches_without_a_word() {
        let root = temp_dir("close");
        for index in 0..50 {
            write(&root, &format!("{index}.md"), "hello\n");
        }
        let request = |id: &str| Request {
            search_id: id.into(),
            mode: "text".into(),
            directories: vec![display(&root)],
            pattern: "hello".into(),
            options: json!({}),
        };
        let (events, received) = mpsc::channel();
        // A search that holds at its first event until the test lets it go on.
        let held = |name: &'static str| {
            let events = events.clone();
            let (opened, gate) = mpsc::channel::<()>();
            let gate = Mutex::new(gate);
            let send: Sender = Box::new(move |channel, payload| {
                let first = channel == PROGRESS && payload["num"] == 1;
                events.send((name, channel)).unwrap();
                if first {
                    gate.lock().unwrap().recv().unwrap();
                }
            });
            (send, opened)
        };

        let searches = Searches::default();
        let (send, open_first) = held("first");
        searches.start(7, request("s"), send);
        assert_eq!(received.recv().unwrap(), ("first", PROGRESS));
        // The same id again: the first search could no longer be reached.
        let (send, open_second) = held("second");
        searches.start(7, request("s"), send);
        assert_eq!(received.recv().unwrap(), ("second", PROGRESS));

        searches.close(7);
        assert!(lock(&searches.0).is_empty());
        open_first.send(()).unwrap();
        open_second.send(()).unwrap();
        assert!(received.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
