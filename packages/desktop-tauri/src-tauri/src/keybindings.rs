// The user's shortcut overrides, standing in for the file handling in
// packages/desktop/src/main/keyboard/shortcutHandler.ts. Rust only stores the
// raw `keybindings.json`; each window's page merges it with the defaults
// (src/keybindings.ts), because the page also knows the platform tables.
// A save is broadcast as `mt::tauri::user-keybindings`, as Electron reloads
// the accelerators of every editor window (main/app/index.ts:805).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, State};

pub struct Keybindings {
    path: PathBuf,
    // Held across the write so two windows cannot interleave their saves.
    write_lock: Mutex<()>,
}

impl Keybindings {
    pub fn new(path: PathBuf) -> Self {
        Self { path, write_lock: Mutex::new(()) }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// A missing or unreadable file means no overrides. The broken file stays in
// place for the user to fix.
fn read(path: &Path) -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(path) else { return Map::new() };
    match serde_json::from_str(&text) {
        Ok(Value::Object(values)) => values,
        Ok(_) => {
            eprintln!("[keybindings] {}: not a JSON object", path.display());
            Map::new()
        }
        Err(err) => {
            eprintln!("[keybindings] {}: {err}", path.display());
            Map::new()
        }
    }
}

fn write(path: &Path, values: &Map<String, Value>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(values).map_err(|err| err.to_string())?;
    crate::write_atomic(path, text.as_bytes()).map_err(|err| format!("{}: {err}", path.display()))
}

fn ensure(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    write(path, &Map::new())
}

#[tauri::command]
pub fn read_user_keybindings(keybindings: State<'_, Keybindings>) -> Map<String, Value> {
    let _guard = keybindings.lock();
    read(&keybindings.path)
}

#[tauri::command]
pub fn write_user_keybindings(
    app: AppHandle,
    keybindings: State<'_, Keybindings>,
    values: Map<String, Value>,
) -> Result<(), String> {
    {
        let _guard = keybindings.lock();
        write(&keybindings.path, &values)?;
    }
    app.emit("mt::tauri::user-keybindings", values).map_err(|err| err.to_string())
}

// Creates an empty file when there is none, as `mt::open-keybindings-config`
// does before opening it (main/app/index.ts:793).
#[tauri::command]
pub fn ensure_keybindings_file(keybindings: State<'_, Keybindings>) -> Result<String, String> {
    let _guard = keybindings.lock();
    ensure(&keybindings.path)?;
    Ok(keybindings.path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-keys-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let dir = temp_dir("missing");
        assert!(read(&dir.join("keybindings.json")).is_empty());
    }

    #[test]
    fn broken_or_non_object_file_reads_as_empty_and_stays() {
        let dir = temp_dir("broken");
        let path = dir.join("keybindings.json");
        std::fs::write(&path, "{ nope").unwrap();
        assert!(read(&path).is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ nope");
        std::fs::write(&path, "[1]").unwrap();
        assert!(read(&path).is_empty());
    }

    #[test]
    fn written_values_read_back() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("keybindings.json");
        let values = map(json!({ "file.save": "Ctrl+Alt+S", "file.quit": "" }));
        write(&path, &values).unwrap();
        assert_eq!(read(&path), values);
    }

    #[test]
    fn ensure_creates_an_empty_object_and_keeps_an_existing_file() {
        let dir = temp_dir("ensure");
        let path = dir.join("keybindings.json");
        ensure(&path).unwrap();
        assert!(read(&path).is_empty());
        std::fs::write(&path, r#"{ "file.save": "" }"#).unwrap();
        ensure(&path).unwrap();
        assert_eq!(read(&path).len(), 1);
    }
}
