// Recently used documents for the File menu, standing in for
// packages/desktop/src/main/menu/index.ts addRecentlyUsedDocument: newest
// first, at most 12, kept in `recently-used-documents.json` in the app data dir.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tauri::State;

const MAX_RECENTLY_USED_DOCUMENTS: usize = 12;

pub struct RecentFiles {
    path: PathBuf,
    list: Mutex<Vec<String>>,
}

impl RecentFiles {
    pub fn load(path: PathBuf) -> Self {
        let list = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
            .unwrap_or_default();
        Self { path, list: Mutex::new(list) }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<String>> {
        self.list.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn add(&self, file: &str) {
        let mut list = self.lock();
        if list.first().is_some_and(|first| first == file) {
            return;
        }
        list.retain(|other| other != file);
        list.insert(0, file.to_string());
        list.truncate(MAX_RECENTLY_USED_DOCUMENTS);
        save(&self.path, &list);
    }

    fn clear(&self) {
        let mut list = self.lock();
        list.clear();
        save(&self.path, &list);
    }
}

fn save(path: &Path, list: &[String]) {
    let result = serde_json::to_string_pretty(list)
        .map_err(|err| err.to_string())
        .and_then(|text| crate::write_atomic(path, text.as_bytes()).map_err(|err| err.to_string()));
    if let Err(err) = result {
        eprintln!("[recent] {}: {err}", path.display());
    }
}

#[tauri::command]
pub fn recent_files(recent: State<'_, RecentFiles>) -> Vec<String> {
    recent.lock().clone()
}

#[tauri::command]
pub fn add_recent_file(recent: State<'_, RecentFiles>, path: String) {
    recent.add(&path);
}

#[tauri::command]
pub fn clear_recent_files(recent: State<'_, RecentFiles>) {
    recent.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates_and_capped() {
        let dir = std::env::temp_dir().join(format!("mt-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("recently-used-documents.json");
        let recent = RecentFiles::load(path.clone());
        for index in 0..14 {
            recent.add(&format!("/f{index}.md"));
        }
        recent.add("/f5.md");
        let list = recent.lock().clone();
        assert_eq!(list.len(), MAX_RECENTLY_USED_DOCUMENTS);
        assert_eq!(list[0], "/f5.md");
        assert_eq!(list[1], "/f13.md");
        assert_eq!(list.iter().filter(|file| *file == "/f5.md").count(), 1);
        assert_eq!(RecentFiles::load(path).lock().clone(), list);
    }
}
