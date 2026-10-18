// Remembers where editor windows were, standing in for electron-window-state
// as used in packages/desktop/src/main/windows/editor.ts and
// ensureWindowPosition in main/windows/utils.ts. Every editor window starts
// from the one `window-state.json` and the file is rewritten when a window
// closes, so the window closed last decides the next start.
//
// Sizes and positions are logical pixels, as Electron's are. The window's
// own scale factor converts the physical values Tauri reports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, PhysicalPosition, PhysicalSize, Window};

pub const DEFAULT_WIDTH: f64 = 1200.0;
pub const DEFAULT_HEIGHT: f64 = 800.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }

    fn contains(&self, inner: &Rect) -> bool {
        inner.x >= self.x
            && inner.y >= self.y
            && inner.x + inner.width <= self.x + self.width
            && inner.y + inner.height <= self.y + self.height
    }
}

// The keys and meaning of electron-window-state's file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub is_maximized: bool,
    pub is_full_screen: bool,
    // The monitor the window was on, to notice that it has gone.
    pub display_bounds: Option<Rect>,
}

impl State {
    fn has_bounds(&self) -> bool {
        matches!((self.x, self.y), (Some(_), Some(_)))
            && self.width.is_some_and(|width| width > 0)
            && self.height.is_some_and(|height| height > 0)
    }

    fn rect(&self) -> Option<Rect> {
        match (self.x, self.y, self.width, self.height) {
            (Some(x), Some(y), Some(width), Some(height)) => Some(Rect { x, y, width, height }),
            _ => None,
        }
    }
}

// Where and how a new window opens. Without a position the window is centred.
#[derive(Debug, PartialEq)]
pub struct Placement {
    pub width: f64,
    pub height: f64,
    pub position: Option<(f64, f64)>,
    pub maximized: bool,
    pub full_screen: bool,
}

// A saved state is dropped when it holds nothing usable. One whose window no
// longer fits on a monitor keeps only its maximized and full screen flags.
// With no monitor list there is nothing to check against.
fn validate(saved: Option<State>, monitors: &[Rect]) -> Option<State> {
    let mut state = saved?;
    if !(state.has_bounds() || state.is_maximized || state.is_full_screen) {
        return None;
    }
    if let (Some(bounds), Some(_)) = (state.rect(), state.display_bounds) {
        if !monitors.is_empty() && !monitors.iter().any(|monitor| monitor.contains(&bounds)) {
            state.x = None;
            state.y = None;
            state.width = None;
            state.height = None;
        }
    }
    Some(state)
}

// `area` is the primary monitor's usable area, which bounds a window that has
// no position yet.
pub fn placement(saved: Option<State>, monitors: &[Rect], area: Option<Rect>) -> Placement {
    let state = validate(saved, monitors);
    let (maximized, full_screen) = state.as_ref().map_or((false, false), |s| (s.is_maximized, s.is_full_screen));
    let bounds = state.as_ref().filter(|s| s.has_bounds()).and_then(State::rect);
    let Some(bounds) = bounds else {
        let mut width = DEFAULT_WIDTH;
        let mut height = DEFAULT_HEIGHT;
        if let Some(area) = area {
            width = width.min(f64::from(area.width));
            height = height.min(f64::from(area.height));
        }
        return Placement { width, height, position: None, maximized, full_screen };
    };
    // Electron centres a window whose corner is on no monitor at all.
    let on_a_monitor = monitors.is_empty() || monitors.iter().any(|monitor| monitor.contains_point(bounds.x, bounds.y));
    Placement {
        width: f64::from(bounds.width),
        height: f64::from(bounds.height),
        position: on_a_monitor.then(|| (f64::from(bounds.x), f64::from(bounds.y))),
        maximized,
        full_screen,
    }
}

fn read(path: &Path) -> Option<State> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn logical(value: i32, scale: f64) -> i32 {
    (f64::from(value) / scale).round() as i32
}

fn logical_rect(position: PhysicalPosition<i32>, size: PhysicalSize<u32>, scale: f64) -> Rect {
    Rect {
        x: logical(position.x, scale),
        y: logical(position.y, scale),
        width: logical(i32::try_from(size.width).unwrap_or(i32::MAX), scale),
        height: logical(i32::try_from(size.height).unwrap_or(i32::MAX), scale),
    }
}

fn monitor_rects(app: &AppHandle) -> Vec<Rect> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| logical_rect(*monitor.position(), *monitor.size(), monitor.scale_factor()))
        .collect()
}

// Like Electron, Linux uses the whole monitor, as its work area is not reported.
fn primary_area(app: &AppHandle) -> Option<Rect> {
    let monitor = app.primary_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    if cfg!(target_os = "linux") {
        return Some(logical_rect(*monitor.position(), *monitor.size(), scale));
    }
    let area = monitor.work_area();
    Some(logical_rect(area.position, area.size, scale))
}

pub struct WindowStates {
    path: PathBuf,
    // What each open editor window would save: its last normal bounds and its
    // current maximized and full screen flags.
    windows: Mutex<BTreeMap<u32, State>>,
}

impl WindowStates {
    pub fn new(path: PathBuf) -> Self {
        Self { path, windows: Mutex::new(BTreeMap::new()) }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<u32, State>> {
        self.windows.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Reads the file afresh, so a window opened later starts from the state
    // the last closed one saved, and registers the window for updates.
    pub fn begin(&self, app: &AppHandle, id: u32) -> Placement {
        let monitors = monitor_rects(app);
        let placement = placement(read(&self.path), &monitors, primary_area(app));
        let state = State {
            x: None,
            y: None,
            width: Some(placement.width.round() as i32),
            height: Some(placement.height.round() as i32),
            is_maximized: placement.maximized,
            is_full_screen: placement.full_screen,
            display_bounds: None,
        };
        let state = match placement.position {
            Some((x, y)) => State { x: Some(x.round() as i32), y: Some(y.round() as i32), ..state },
            None => state,
        };
        self.lock().insert(id, state);
        placement
    }

    pub fn update(&self, window: &Window, id: u32) {
        let mut windows = self.lock();
        let Some(state) = windows.get_mut(&id) else { return };
        let (Ok(maximized), Ok(minimized), Ok(full_screen)) =
            (window.is_maximized(), window.is_minimized(), window.is_fullscreen())
        else {
            return;
        };
        let scale = window.scale_factor().unwrap_or(1.0);
        let bounds = match (window.outer_position(), window.outer_size()) {
            (Ok(position), Ok(size)) => Some(logical_rect(position, size, scale)),
            _ => None,
        };
        if let Some(bounds) = bounds {
            if !maximized && !minimized && !full_screen {
                state.x = Some(bounds.x);
                state.y = Some(bounds.y);
                state.width = Some(bounds.width);
                state.height = Some(bounds.height);
            }
            state.display_bounds = window
                .current_monitor()
                .ok()
                .flatten()
                .map(|monitor| logical_rect(*monitor.position(), *monitor.size(), monitor.scale_factor()));
        }
        state.is_maximized = maximized;
        state.is_full_screen = full_screen;
    }

    // Writes the state of a window that has closed.
    pub fn save(&self, id: u32) {
        let Some(state) = self.lock().remove(&id) else { return };
        let result = serde_json::to_string_pretty(&state)
            .map_err(|err| err.to_string())
            .and_then(|text| crate::write_atomic(&self.path, text.as_bytes()).map_err(|err| err.to_string()));
        if let Err(err) = result {
            eprintln!("[window-state] {}: {err}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect { x: 0, y: 0, width: 1920, height: 1080 };
    const SECOND: Rect = Rect { x: 1920, y: 0, width: 1280, height: 1024 };

    fn state(x: i32, y: i32, width: i32, height: i32, display: Rect) -> State {
        State {
            x: Some(x),
            y: Some(y),
            width: Some(width),
            height: Some(height),
            display_bounds: Some(display),
            ..State::default()
        }
    }

    #[test]
    fn no_state_gives_a_centred_default_window() {
        let placed = placement(None, &[SCREEN], Some(SCREEN));
        assert_eq!(
            placed,
            Placement { width: 1200.0, height: 800.0, position: None, maximized: false, full_screen: false }
        );
    }

    #[test]
    fn the_default_size_shrinks_to_a_small_screen() {
        let small = Rect { x: 0, y: 0, width: 1024, height: 600 };
        let placed = placement(None, &[small], Some(small));
        assert_eq!((placed.width, placed.height), (1024.0, 600.0));
    }

    #[test]
    fn a_saved_window_on_a_monitor_is_restored() {
        let placed = placement(Some(state(100, 50, 900, 700, SCREEN)), &[SCREEN, SECOND], Some(SCREEN));
        assert_eq!(placed.position, Some((100.0, 50.0)));
        assert_eq!((placed.width, placed.height), (900.0, 700.0));
    }

    #[test]
    fn a_window_on_a_monitor_that_is_gone_gets_the_default() {
        let saved = state(2000, 100, 900, 700, SECOND);
        let placed = placement(Some(saved), &[SCREEN], Some(SCREEN));
        assert_eq!(placed.position, None);
        assert_eq!((placed.width, placed.height), (1200.0, 800.0));
    }

    #[test]
    fn a_window_sticking_out_of_its_monitor_gets_the_default() {
        let placed = placement(Some(state(1500, 100, 900, 700, SCREEN)), &[SCREEN], Some(SCREEN));
        assert_eq!(placed.position, None);
        assert_eq!(placed.width, 1200.0);
    }

    #[test]
    fn a_window_across_two_monitors_is_checked_against_its_own_display() {
        let saved = state(1800, 100, 400, 300, SECOND);
        let placed = placement(Some(saved), &[SCREEN, SECOND], Some(SCREEN));
        assert_eq!(placed.position, None);
    }

    #[test]
    fn state_without_a_display_skips_the_visibility_check() {
        let saved = State { display_bounds: None, ..state(5000, 5000, 900, 700, SCREEN) };
        let placed = placement(Some(saved), &[SCREEN], Some(SCREEN));
        assert_eq!(placed.position, None);
        assert_eq!(placed.width, 900.0);
    }

    #[test]
    fn maximized_survives_a_lost_monitor() {
        let saved = State { is_maximized: true, ..state(2000, 100, 900, 700, SECOND) };
        let placed = placement(Some(saved), &[SCREEN], Some(SCREEN));
        assert!(placed.maximized);
        assert_eq!(placed.position, None);
    }

    #[test]
    fn full_screen_without_bounds_is_kept() {
        let saved = State { is_full_screen: true, ..State::default() };
        let placed = placement(Some(saved), &[SCREEN], Some(SCREEN));
        assert!(placed.full_screen);
        assert_eq!(placed.width, 1200.0);
    }

    #[test]
    fn empty_or_broken_bounds_are_ignored() {
        let zero = State { width: Some(0), ..state(10, 10, 0, 700, SCREEN) };
        assert_eq!(validate(Some(zero), &[SCREEN]), None);
        let half = State { x: Some(10), ..State::default() };
        assert_eq!(validate(Some(half), &[SCREEN]), None);
        assert_eq!(validate(None, &[SCREEN]), None);
    }

    #[test]
    fn no_monitor_list_trusts_the_saved_bounds() {
        let placed = placement(Some(state(100, 50, 900, 700, SCREEN)), &[], None);
        assert_eq!(placed.position, Some((100.0, 50.0)));
    }

    #[test]
    fn the_file_uses_electrons_keys() {
        let saved = State { is_maximized: true, ..state(1, 2, 3, 4, SCREEN) };
        let text = serde_json::to_string(&saved).unwrap();
        assert!(text.contains("\"isMaximized\":true") && text.contains("\"displayBounds\""));
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), saved);
        let electron = r#"{"width":800,"height":600,"x":10,"y":20,"displayBounds":{"x":0,"y":0,"width":1920,"height":1080},"isMaximized":false,"isFullScreen":false}"#;
        assert_eq!(serde_json::from_str::<State>(electron).unwrap().width, Some(800));
    }

    #[test]
    fn a_missing_or_corrupt_file_reads_as_no_state() {
        let dir = std::env::temp_dir().join(format!("mt-window-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir.join("window-state.json")), None);
        std::fs::write(dir.join("window-state.json"), "{not json").unwrap();
        assert_eq!(read(&dir.join("window-state.json")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn physical_values_become_logical() {
        let rect = logical_rect(PhysicalPosition::new(300, 150), PhysicalSize::new(1800, 1200), 1.5);
        assert_eq!(rect, Rect { x: 200, y: 100, width: 1200, height: 800 });
    }
}
