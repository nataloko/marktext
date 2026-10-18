// The image settings and image history, standing in for the data center in
// packages/desktop/src/main/dataCenter/index.ts. Windows read them through
// `get_user_data`, change them through `set_user_data`, and hear about every
// change through the `mt::tauri::user-data` event. Nothing in it is secret:
// Electron's list of keys kept in the keychain is empty, so this is plain JSON.
//
// Not ported yet: the image history (webImages, cloudImages) is stored but
// nothing adds to it, because uploading is not ported.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Map, Value};
use tauri::{AppHandle, Emitter, State};

pub struct UserData {
    path: PathBuf,
    values: Mutex<Map<String, Value>>,
}

impl UserData {
    // `folder` is the app data folder, where the image and screenshot folders
    // are placed on first run.
    pub fn load(path: PathBuf, folder: &Path) -> Self {
        let values = match read_stored(&path) {
            None => {
                let values = defaults(folder);
                // The screenshot folder is created with the defaults, as Electron does.
                if let Some(dir) = values["screenshotFolderPath"].as_str() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(err) = write(&path, &values) {
                    eprintln!("[user data] {err}");
                }
                values
            }
            Some(stored) => {
                let values = migrate(stored.clone());
                if values != stored {
                    if let Err(err) = write(&path, &values) {
                        eprintln!("[user data] {err}");
                    }
                }
                values
            }
        };
        Self { path, values: Mutex::new(values) }
    }

    fn lock(&self) -> MutexGuard<'_, Map<String, Value>> {
        self.values.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Stores every change, as Electron does even for a value that is the same.
    fn apply(&self, changes: &Map<String, Value>) -> Result<(), String> {
        let mut values = self.lock();
        let mut next = values.clone();
        for (key, value) in changes {
            next.insert(key.clone(), value.clone());
        }
        // A screenshot folder is created as soon as it is chosen.
        if let Some(dir) = changes.get("screenshotFolderPath").and_then(Value::as_str) {
            let _ = std::fs::create_dir_all(dir);
        }
        write(&self.path, &next)?;
        *values = next;
        Ok(())
    }

    // Stores the changes, then tells every window about them.
    pub fn update(&self, app: &AppHandle, changes: Map<String, Value>) -> Result<(), String> {
        if changes.is_empty() {
            return Ok(());
        }
        self.apply(&changes)?;
        app.emit("mt::tauri::user-data", changes).map_err(|err| err.to_string())
    }
}

fn defaults(folder: &Path) -> Map<String, Value> {
    let values = json!({
        "imageFolderPath": folder.join("images").to_string_lossy(),
        "screenshotFolderPath": folder.join("screenshot").to_string_lossy(),
        "webImages": [],
        "cloudImages": [],
        "currentUploader": "picgo",
    });
    values.as_object().cloned().unwrap_or_default()
}

fn read_stored(path: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(values) => Some(values),
        Err(err) => {
            // Keep the broken file for the user instead of overwriting it.
            let backup = path.with_extension("json.broken");
            eprintln!("[user data] {}: {err}; moved to {}", path.display(), backup.display());
            let _ = std::fs::rename(path, &backup);
            None
        }
    }
}

// Uploaders that no longer exist fall back to PicGo (dataCenter init).
fn migrate(mut values: Map<String, Value>) -> Map<String, Value> {
    if matches!(values.get("currentUploader").and_then(Value::as_str), Some("none" | "github")) {
        values.insert("currentUploader".into(), "picgo".into());
    }
    values
}

fn write(path: &Path, values: &Map<String, Value>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(values).map_err(|err| err.to_string())?;
    crate::write_atomic(path, text.as_bytes()).map_err(|err| format!("{}: {err}", path.display()))
}

#[tauri::command]
pub fn get_user_data(user_data: State<'_, UserData>) -> Map<String, Value> {
    user_data.lock().clone()
}

#[tauri::command]
pub fn set_user_data(
    app: AppHandle,
    user_data: State<'_, UserData>,
    changes: Map<String, Value>,
) -> Result<(), String> {
    user_data.update(&app, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-userdata-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stored(path: &Path) -> Map<String, Value> {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn first_run_writes_defaults_and_makes_the_screenshot_folder() {
        let dir = temp_dir("first");
        let path = dir.join("dataCenter.json");
        let data = UserData::load(path.clone(), &dir);
        let values = data.lock().clone();
        assert_eq!(values["imageFolderPath"], dir.join("images").to_string_lossy().as_ref());
        assert_eq!(values["currentUploader"], "picgo");
        assert_eq!(values["webImages"], json!([]));
        assert_eq!(stored(&path), values);
        assert!(dir.join("screenshot").is_dir());
    }

    #[test]
    fn legacy_uploaders_become_picgo_and_other_keys_stay() {
        for legacy in ["none", "github"] {
            let dir = temp_dir(legacy);
            let path = dir.join("dataCenter.json");
            std::fs::write(&path, format!(r#"{{ "currentUploader": "{legacy}", "imageFolderPath": "/x" }}"#)).unwrap();
            let data = UserData::load(path.clone(), &dir);
            assert_eq!(data.lock()["currentUploader"], "picgo");
            assert_eq!(data.lock()["imageFolderPath"], "/x");
            assert_eq!(stored(&path)["currentUploader"], "picgo");
        }
        let dir = temp_dir("cli");
        let path = dir.join("dataCenter.json");
        std::fs::write(&path, r#"{ "currentUploader": "cliScript" }"#).unwrap();
        assert_eq!(UserData::load(path, &dir).lock()["currentUploader"], "cliScript");
    }

    #[test]
    fn apply_merges_and_persists() {
        let dir = temp_dir("apply");
        let path = dir.join("dataCenter.json");
        let data = UserData::load(path.clone(), &dir);
        let screenshots = dir.join("elsewhere");
        data.apply(&map(json!({ "cliScript": "/bin/up", "screenshotFolderPath": screenshots.to_string_lossy() })))
            .unwrap();
        let written = stored(&path);
        assert_eq!(written["cliScript"], "/bin/up");
        assert_eq!(written["currentUploader"], "picgo");
        assert_eq!(UserData::load(path, &dir).lock().clone(), written);
        assert!(screenshots.is_dir());
    }

    #[test]
    fn broken_file_is_kept_aside() {
        let dir = temp_dir("broken");
        let path = dir.join("dataCenter.json");
        std::fs::write(&path, "{ not json").unwrap();
        let data = UserData::load(path.clone(), &dir);
        assert_eq!(data.lock()["currentUploader"], "picgo");
        assert_eq!(std::fs::read_to_string(dir.join("dataCenter.json.broken")).unwrap(), "{ not json");
    }
}
