// `mt::fonts::list`: the installed font families for the Settings pages' font
// boxes, in place of Electron's font-list (packages/desktop/src/main/ipc/
// fonts.ts). fontdb reads the folders the webviews take their fonts from:
// fontconfig's on Linux, the system and the user's Fonts folders on Windows.

use std::collections::BTreeSet;

// Reading every font takes a moment, so it runs off the main thread.
#[tauri::command]
pub async fn fonts_list() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(installed_families).await.unwrap_or_default()
}

fn installed_families() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    // A face's first family name is the English one where the font has it.
    sorted_unique(db.faces().filter_map(|face| face.families.first().map(|(name, _)| name.clone())))
}

// Sorted as a person reads a list, ignoring case.
fn sorted_unique(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> =
        names.into_iter().filter(|name| !name.trim().is_empty()).collect::<BTreeSet<_>>().into_iter().collect();
    names.sort_by_cached_key(|name| name.to_lowercase());
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sorted_ignoring_case_without_repeats() {
        let names = ["Noto Sans", "arial", "Noto Sans", "", "DejaVu Sans Mono"].map(String::from);
        assert_eq!(sorted_unique(names), ["arial", "DejaVu Sans Mono", "Noto Sans"]);
    }
}
