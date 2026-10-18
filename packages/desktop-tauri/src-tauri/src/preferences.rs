// The preference store for the whole app, standing in for
// packages/desktop/src/main/preferences/index.ts. Windows read it through
// `get_preferences`, change it through `set_preferences`, and hear about every
// change through the `mt::user-preference` event, as Electron's window manager
// broadcasts it (packages/desktop/src/main/app/windowManager.ts:479).
//
// Not ported yet: validation against preferences/schema.json, and detecting
// the system language on first run (main/app/index.ts initLanguage).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, State};

const DEFAULTS: &str = include_str!("../../../desktop/static/preference.json");

pub struct Preferences {
    path: PathBuf,
    store: Mutex<Store>,
}

struct Store {
    values: Map<String, Value>,
    // No preferences file existed at start. Electron picks the theme from the
    // system before writing the first file; here the first window reports it.
    first_run: bool,
}

impl Preferences {
    pub fn load(path: PathBuf) -> Self {
        let defaults: Map<String, Value> =
            serde_json::from_str(DEFAULTS).expect("static preference.json is valid JSON");
        let (values, first_run) = match read_stored(&path) {
            None => (defaults, true),
            Some(stored) => {
                let values = reconcile(migrate(stored.clone()), &defaults);
                if values != stored {
                    if let Err(err) = write(&path, &values) {
                        eprintln!("[preferences] {err}");
                    }
                }
                (values, false)
            }
        };
        Self { path, store: Mutex::new(Store { values, first_run }) }
    }

    fn lock(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.lock().values.get(key).cloned()
    }
}

fn read_stored(path: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(values) => Some(values),
        Err(err) => {
            // Keep the broken file for the user instead of overwriting it.
            let backup = path.with_extension("json.broken");
            eprintln!("[preferences] {}: {err}; moved to {}", path.display(), backup.display());
            let _ = std::fs::rename(path, &backup);
            None
        }
    }
}

// electron-store migrations in main/preferences/index.ts.
fn migrate(mut values: Map<String, Value>) -> Map<String, Value> {
    if values.get("startUpAction").and_then(Value::as_str) == Some("lastState") {
        values.insert("startUpAction".into(), "openLastFolder".into());
    }
    values
}

// Drops settings that no longer exist and adds new ones with their defaults.
fn reconcile(mut values: Map<String, Value>, defaults: &Map<String, Value>) -> Map<String, Value> {
    values.retain(|key, _| defaults.contains_key(key));
    for (key, value) in defaults {
        values.entry(key.clone()).or_insert_with(|| value.clone());
    }
    values
}

fn write(path: &Path, values: &Map<String, Value>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(values).map_err(|err| err.to_string())?;
    crate::write_atomic(path, text.as_bytes()).map_err(|err| format!("{}: {err}", path.display()))
}

#[tauri::command]
pub fn get_preferences(
    preferences: State<'_, Preferences>,
    prefers_dark: bool,
) -> Result<Map<String, Value>, String> {
    let mut store = preferences.lock();
    if store.first_run {
        if prefers_dark {
            store.values.insert("theme".into(), "dark".into());
        }
        write(&preferences.path, &store.values)?;
        store.first_run = false;
    }
    Ok(store.values.clone())
}

impl Preferences {
    // Stores the settings that differ from the current ones and returns them.
    // Every window saves its sidebar state while booting, so most calls change
    // nothing and should neither write the file nor wake the other windows.
    fn apply(&self, mut changes: Map<String, Value>) -> Result<Map<String, Value>, String> {
        let mut store = self.lock();
        changes.retain(|key, value| store.values.get(key) != Some(value));
        if changes.is_empty() {
            return Ok(changes);
        }
        for (key, value) in &changes {
            store.values.insert(key.clone(), value.clone());
        }
        write(&self.path, &store.values)?;
        store.first_run = false;
        Ok(changes)
    }
}

impl Preferences {
    // Stores the changes and tells every window about them.
    pub fn update(&self, app: &AppHandle, changes: Map<String, Value>) -> Result<(), String> {
        let mut changes = self.apply(changes)?;
        // The title bar style cannot change while windows are open.
        changes.remove("titleBarStyle");
        if changes.is_empty() {
            return Ok(());
        }
        app.emit("mt::user-preference", changes).map_err(|err| err.to_string())
    }
}

#[tauri::command]
pub fn set_preferences(
    app: AppHandle,
    preferences: State<'_, Preferences>,
    changes: Map<String, Value>,
) -> Result<(), String> {
    preferences.update(&app, changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-prefs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn migrates_last_state() {
        let values = migrate(map(json!({ "startUpAction": "lastState" })));
        assert_eq!(values["startUpAction"], "openLastFolder");
    }

    #[test]
    fn reconcile_drops_unknown_and_adds_new_keys() {
        let defaults = map(json!({ "autoSave": false, "theme": "light" }));
        let values = reconcile(map(json!({ "theme": "dark", "removed": 1 })), &defaults);
        assert_eq!(values, map(json!({ "autoSave": false, "theme": "dark" })));
    }

    #[test]
    fn first_run_has_defaults_and_no_file() {
        let dir = temp_dir("first");
        let path = dir.join("preferences.json");
        let preferences = Preferences::load(path.clone());
        let store = preferences.lock();
        assert!(store.first_run);
        assert_eq!(store.values, serde_json::from_str::<Map<String, Value>>(DEFAULTS).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn existing_file_is_reconciled_and_rewritten() {
        let dir = temp_dir("existing");
        let path = dir.join("preferences.json");
        std::fs::write(&path, r#"{ "theme": "dark", "removed": 1 }"#).unwrap();
        let preferences = Preferences::load(path.clone());
        let store = preferences.lock();
        assert!(!store.first_run);
        assert_eq!(store.values["theme"], "dark");
        assert!(!store.values.contains_key("removed"));
        let written: Map<String, Value> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written, store.values);
    }

    #[test]
    fn apply_stores_and_returns_only_real_changes() {
        let dir = temp_dir("apply");
        let path = dir.join("preferences.json");
        std::fs::write(&path, r#"{ "theme": "dark" }"#).unwrap();
        let preferences = Preferences::load(path.clone());
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();

        let unchanged = preferences.apply(map(json!({ "theme": "dark" }))).unwrap();
        assert!(unchanged.is_empty());
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);

        let changed = preferences.apply(map(json!({ "theme": "light", "autoSave": false }))).unwrap();
        assert_eq!(changed, map(json!({ "theme": "light" })));
        let written: Map<String, Value> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["theme"], "light");
    }

    #[test]
    fn broken_file_is_kept_aside() {
        let dir = temp_dir("broken");
        let path = dir.join("preferences.json");
        std::fs::write(&path, "{ not json").unwrap();
        let preferences = Preferences::load(path.clone());
        assert!(preferences.lock().first_run);
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(dir.join("preferences.json.broken")).unwrap(), "{ not json");
    }
}
