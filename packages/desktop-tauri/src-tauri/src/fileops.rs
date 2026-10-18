// The renderer's file operations: `window.fileUtils`
// (packages/desktop/src/main/ipc/fs.ts), deleting to the trash
// (main/filesystem/trash.ts) and renaming or moving a file or folder (the
// sidebar, and main/menu/actions/file.ts mt::rename and
// mt::response-file-move-to). The page asks the user first where Electron
// does (src/fileOps.ts).
//
// Not ported yet: stat, unlink and empty-dir, which the renderer does not call.

use std::io::{Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tauri::{State, WebviewWindow};

use crate::watch::FileWatcher;
use crate::windows::{self, Windows};

fn failed(path: &str) -> impl Fn(Error) -> String + '_ {
    move |err| format!("{path}: {err}")
}

// common/filesystem isFile and isDirectory look at a link itself, not its target.
#[tauri::command]
pub fn fs_is_file(path: String) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
}

#[tauri::command]
pub fn fs_is_directory(path: String) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

// common/filesystem isExecutableFile. Node's X_OK check on Windows only asks
// whether the file exists.
#[tauri::command]
pub fn fs_is_executable(path: String) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    meta.is_file()
}

#[tauri::command]
pub async fn fs_ensure_dir(path: String) -> Result<(), String> {
    std::fs::create_dir_all(&path).map_err(failed(&path))
}

// Text, or bytes sent as an array of numbers.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Data {
    Text(String),
    Bytes(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Data::Text(text) => text.as_bytes(),
            Data::Bytes(bytes) => bytes,
        }
    }
}

#[tauri::command]
pub async fn fs_write_file(path: String, data: Data) -> Result<(), String> {
    std::fs::write(&path, data.bytes()).map_err(failed(&path))
}

// fs-extra outputFile: writeFile that creates the missing folders first.
#[tauri::command]
pub async fn fs_output_file(path: String, data: Data) -> Result<(), String> {
    if let Some(dir) = Path::new(&path).parent() {
        std::fs::create_dir_all(dir).map_err(failed(&path))?;
    }
    std::fs::write(&path, data.bytes()).map_err(failed(&path))
}

// The bytes arrive in the page as an ArrayBuffer.
#[tauri::command]
pub async fn fs_read_file(path: String) -> Result<tauri::ipc::Response, String> {
    let bytes = std::fs::read(&path).map_err(failed(&path))?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub async fn fs_readdir(path: String) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(&path).map_err(failed(&path))?;
    Ok(entries.filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned())).collect())
}

// fs-extra copy: a folder is copied with everything in it, and links are
// copied as links. A move between drives keeps the files' modification times,
// which the sidebar may sort by.
fn copy_recursive(from: &Path, to: &Path, keep_times: bool) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(from)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, to);
        #[cfg(windows)]
        return if std::fs::metadata(from)?.is_dir() {
            std::os::windows::fs::symlink_dir(target, to)
        } else {
            std::os::windows::fs::symlink_file(target, to)
        };
    }
    if !meta.is_dir() {
        std::fs::copy(from, to)?;
        if keep_times {
            std::fs::File::options().write(true).open(to)?.set_modified(meta.modified()?)?;
        }
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_recursive(&entry.path(), &to.join(entry.file_name()), keep_times)?;
    }
    Ok(())
}

fn already_exists(to: &Path) -> Error {
    let name = to.file_name().unwrap_or(to.as_os_str()).to_string_lossy();
    Error::new(ErrorKind::AlreadyExists, format!("\"{name}\" already exists"))
}

fn inside(dir: &Path, path: &Path) -> bool {
    if cfg!(windows) {
        let fold = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
        fold(path).starts_with(fold(dir))
    } else {
        path.starts_with(dir)
    }
}

// A folder copied or moved into itself would never end. The paths are
// compared as given and, for links, as resolved; some drives, such as a
// virtual machine's shared folder, cannot resolve paths at all.
fn into_itself(from: &Path, to: &Path) -> bool {
    let parent = to.parent().unwrap_or(to);
    if inside(from, parent) {
        return true;
    }
    let Ok(from) = std::fs::canonicalize(from) else { return false };
    to.ancestors()
        .skip(1)
        .find_map(|dir| std::fs::canonicalize(dir).ok())
        .is_some_and(|parent| parent.starts_with(&from))
}

// Unlike fs-extra, which replaces files at the destination, the sidebar's
// paste is refused when the name is taken.
fn copy_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(already_exists(to));
    }
    if into_itself(from, to) {
        return Err(Error::new(ErrorKind::InvalidInput, "cannot copy a folder into itself"));
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    copy_recursive(from, to, false)
}

#[tauri::command]
pub async fn fs_copy(from: String, to: String) -> Result<(), String> {
    copy_path(Path::new(&from), Path::new(&to)).map_err(failed(&from))
}

// The source itself under another case of letters, which is how a file is
// renamed from "note.md" to "Note.md" where names ignore case. A link is not
// the entry it points to, so it never counts as its target.
fn case_change(from: &Path, to: &Path) -> bool {
    let (a, b) = (from.to_string_lossy(), to.to_string_lossy());
    if a == b || a.to_lowercase() != b.to_lowercase() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        matches!(
            (std::fs::symlink_metadata(from), std::fs::symlink_metadata(to)),
            (Ok(a), Ok(b)) if (a.dev(), a.ino()) == (b.dev(), b.ino())
        )
    }
    #[cfg(not(unix))]
    true
}

fn taken(from: &Path, to: &Path) -> bool {
    std::fs::symlink_metadata(to).is_ok() && !case_change(from, to)
}

fn crosses_devices(err: &Error) -> bool {
    // Windows ERROR_NOT_SAME_DEVICE.
    err.kind() == ErrorKind::CrossesDevices || (cfg!(windows) && err.raw_os_error() == Some(17))
}

// fs-extra move with overwrite off: a destination that exists is refused.
// Between drives the source is copied and then removed. The check and the
// rename are two steps, so a file created in between could still be replaced.
fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if taken(from, to) {
        return Err(already_exists(to));
    }
    if into_itself(from, to) {
        return Err(Error::new(ErrorKind::InvalidInput, "cannot move a folder into itself"));
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::rename(from, to) {
        Err(err) if crosses_devices(&err) => {
            copy_recursive(from, to, true)?;
            if std::fs::symlink_metadata(from)?.is_dir() {
                std::fs::remove_dir_all(from)
            } else {
                std::fs::remove_file(from)
            }
        }
        result => result,
    }
}

// Blocking work, off the async workers. On Windows the trash crate sets up
// COM on the thread it runs on and panics if that fails; the panic then comes
// back as an error rather than a promise that never settles.
async fn blocking<T: Send + 'static>(
    path: &str,
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, String> {
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result.map_err(failed(path)),
        Err(err) => Err(format!("{path}: {err}")),
    }
}

// common/filesystem/paths.ts IMAGE_EXTENSIONS.
const IMAGE_EXTENSIONS: [&str; 6] = ["jpeg", "jpg", "png", "gif", "svg", "webp"];

// common/filesystem/paths.ts isImageFile: an image extension on a file, not on
// a link or a folder.
#[tauri::command]
pub fn paths_is_image(path: String) -> bool {
    let ext = node_extname(&path).trim_start_matches('.').to_lowercase();
    IMAGE_EXTENSIONS.contains(&ext.as_str()) && fs_is_file(path)
}

// main/config.ts BLACK_LIST.
const IGNORED_NAMES: [&str; 1] = ["$RECYCLE.BIN"];

#[derive(Debug, PartialEq, Serialize)]
pub struct ImagePathEntry {
    file: String,
    // "directory" or "image".
    #[serde(rename = "type")]
    kind: &'static str,
}

// main/utils/imagePathAutoComplement.ts filesHandler: the folders and image
// files in `dir`, for completing an image path as it is typed (the page
// filters them by what was typed). Links are skipped, as Electron's lstat
// checks skip them. Electron caches each listing and watches the folder; one
// readdir per keystroke is cheap enough to do without.
#[tauri::command]
pub async fn image_path_entries(dir: String) -> Result<Vec<ImagePathEntry>, String> {
    let entries = std::fs::read_dir(&dir).map_err(failed(&dir))?;
    Ok(entries.filter_map(|entry| image_path_entry(&entry.ok()?)).collect())
}

fn image_path_entry(entry: &std::fs::DirEntry) -> Option<ImagePathEntry> {
    let file = entry.file_name().to_string_lossy().into_owned();
    if IGNORED_NAMES.contains(&file.as_str()) {
        return None;
    }
    let file_type = entry.file_type().ok()?;
    let kind = if file_type.is_dir() {
        "directory"
    } else if file_type.is_file() && ends_with_image_extension(&file) {
        "image"
    } else {
        return None;
    };
    Some(ImagePathEntry { file, kind })
}

// Electron's IMAGE_REG matches the extension at the end of the name without
// a dot, so "screenpng" counts too.
fn ends_with_image_extension(name: &str) -> bool {
    let name = name.to_lowercase();
    IMAGE_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
}

// Node's path.extname: from the last dot of the file name, but not for a name
// that only starts with a dot.
fn node_extname(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(0) | None => "",
        Some(_) if name == ".." => "",
        Some(index) => &name[index..],
    }
}

// main/filesystem copyFileWithContentHash: the source is hashed while it is
// copied, and the copy is named after the hash. If that name is taken the
// content is already there, so the new copy is dropped.
fn copy_with_content_hash(src: &Path, output_dir: &Path) -> std::io::Result<PathBuf> {
    let temp = output_dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let copied = (|| {
        let mut input = std::fs::File::open(src)?;
        let mut output = std::fs::File::create_new(&temp)?;
        let mut hash = Sha1::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            output.write_all(&buffer[..read])?;
        }
        output.flush()?;
        let name: String = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
        let ext = node_extname(&src.to_string_lossy()).to_string();
        Ok(output_dir.join(format!("{name}{ext}")))
    })();
    let result = copied.and_then(|dest| {
        if dest.exists() {
            std::fs::remove_file(&temp)?;
        } else {
            std::fs::rename(&temp, &dest)?;
        }
        Ok(dest)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[tauri::command]
pub async fn fs_copy_with_content_hash(src: String, output_dir: String) -> Result<String, String> {
    let (from, dir) = (PathBuf::from(&src), PathBuf::from(output_dir));
    let dest = blocking(&src, move || copy_with_content_hash(&from, &dir)).await?;
    Ok(dest.to_string_lossy().into_owned())
}

fn trash(path: &Path) -> std::io::Result<()> {
    trash::delete(path).map_err(|err| Error::other(err.to_string()))
}

// `replace` is for a destination the user agreed to replace (the menu's
// Rename and Move To). It goes to the trash instead of being overwritten: the
// question cannot make Cancel its default button as Electron's does, so an
// Enter pressed too soon must not lose a file. `follow` says the renderer
// moves the document's tab along, as it does for a rename but not for a
// paste; this window's records of the open file then follow too
// (main/app/windowManager.ts window-change-file-path).
#[tauri::command]
pub async fn move_file(
    window: WebviewWindow,
    windows: State<'_, Windows>,
    watcher: State<'_, FileWatcher>,
    from: String,
    to: String,
    replace: bool,
    follow: bool,
) -> Result<(), String> {
    let (source, target) = (PathBuf::from(&from), PathBuf::from(&to));
    blocking(&from, move || {
        if replace && taken(&source, &target) {
            trash(&target)?;
        }
        move_path(&source, &target)
    })
    .await?;
    if follow {
        windows::file_moved(&window, &windows, &watcher, &from, &to);
    }
    Ok(())
}

#[tauri::command]
pub async fn trash_item(path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    blocking(&path, move || trash(&target)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mt-fileops-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn move_refuses_a_taken_name() {
        let dir = temp_dir("move");
        std::fs::write(dir.join("a.md"), "a").unwrap();
        std::fs::write(dir.join("b.md"), "b").unwrap();
        let err = move_path(&dir.join("a.md"), &dir.join("b.md")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(dir.join("b.md")).unwrap(), b"b");

        move_path(&dir.join("a.md"), &dir.join("new/deeper/c.md")).unwrap();
        assert!(dir.join("new/deeper/c.md").is_file());
        assert!(!dir.join("a.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_never_moved_onto_its_target() {
        let dir = temp_dir("link");
        std::fs::create_dir_all(dir.join("y")).unwrap();
        std::fs::write(dir.join("y/note.md"), "keep me").unwrap();
        std::os::unix::fs::symlink(dir.join("y/note.md"), dir.join("link.md")).unwrap();
        assert!(!case_change(&dir.join("link.md"), &dir.join("y/note.md")));
        let err = move_path(&dir.join("link.md"), &dir.join("y/note.md")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(dir.join("y/note.md")).unwrap(), b"keep me");
    }

    #[test]
    fn folders_are_copied_whole_but_not_into_themselves() {
        let dir = temp_dir("copy");
        std::fs::create_dir_all(dir.join("src/inner")).unwrap();
        std::fs::write(dir.join("src/a.md"), "a").unwrap();
        std::fs::write(dir.join("src/inner/b.md"), "b").unwrap();
        copy_path(&dir.join("src"), &dir.join("src (copy)")).unwrap();
        assert_eq!(std::fs::read(dir.join("src (copy)/inner/b.md")).unwrap(), b"b");
        assert!(dir.join("src/a.md").is_file());
        let err = copy_path(&dir.join("src/a.md"), &dir.join("src (copy)/a.md")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);

        let err = copy_path(&dir.join("src"), &dir.join("src/inner/src")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(!dir.join("src/inner/src").exists());
        let err = move_path(&dir.join("src"), &dir.join("src/inner/src")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn into_itself_without_resolving_paths() {
        let gone = std::env::temp_dir().join("mt-fileops-gone");
        assert!(into_itself(&gone, &gone.join("inner/gone")));
        assert!(into_itself(&gone, &gone.join("gone")));
        assert!(!into_itself(&gone.join("a"), &gone.join("b")));
        assert!(!into_itself(&gone.join("a"), &gone.join("ab/a")));
    }

    #[test]
    fn image_paths_need_an_image_extension_and_a_file() {
        let dir = temp_dir("image");
        for name in ["a.PNG", "b.jpeg", "c.txt", ".png", "d"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        std::fs::create_dir_all(dir.join("folder.png")).unwrap();
        let is_image = |name: &str| paths_is_image(dir.join(name).to_string_lossy().into_owned());
        assert!(is_image("a.PNG"));
        assert!(is_image("b.jpeg"));
        assert!(!is_image("c.txt"));
        assert!(!is_image(".png"));
        assert!(!is_image("d"));
        assert!(!is_image("folder.png"));
        assert!(!is_image("missing.png"));
    }

    #[test]
    fn extname_follows_node() {
        assert_eq!(node_extname("/a.b/c.tar.gz"), ".gz");
        assert_eq!(node_extname("/a.b/c"), "");
        assert_eq!(node_extname("/a/.hidden"), "");
        assert_eq!(node_extname("c."), ".");
        assert_eq!(node_extname("C:\\x\\p.PNG"), ".PNG");
    }

    #[test]
    fn copies_are_named_by_content_and_not_repeated() {
        let dir = temp_dir("hash");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(dir.join("one.png"), "abc").unwrap();
        std::fs::write(dir.join("two.png"), "abc").unwrap();
        std::fs::write(dir.join("other.PNG"), "abd").unwrap();

        let first = copy_with_content_hash(&dir.join("one.png"), &out).unwrap();
        // SHA-1 of "abc".
        assert_eq!(first, out.join("a9993e364706816aba3e25717850c26c9cd0d89d.png"));
        assert_eq!(std::fs::read(&first).unwrap(), b"abc");
        let second = copy_with_content_hash(&dir.join("two.png"), &out).unwrap();
        assert_eq!(second, first);
        let third = copy_with_content_hash(&dir.join("other.PNG"), &out).unwrap();
        assert_eq!(third.extension().unwrap(), "PNG");

        let mut names: Vec<_> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|name| !name.to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn a_failed_copy_leaves_no_temp_file() {
        let dir = temp_dir("hash-fail");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(copy_with_content_hash(&dir.join("missing.png"), &out).is_err());
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
    }

    #[test]
    fn image_paths_list_folders_and_images_only() {
        let dir = temp_dir("image-paths");
        for folder in ["assets", "$RECYCLE.BIN", "pics.png"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        for file in ["a.png", "B.JPEG", "c.webp", "screenpng", "notes.md", "d.png.txt", ".hidden.gif"] {
            std::fs::write(dir.join(file), "").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.png", dir.join("link.png")).unwrap();

        let mut entries =
            tauri::async_runtime::block_on(image_path_entries(crate::display(&dir))).unwrap();
        entries.sort_by(|a, b| a.file.cmp(&b.file));
        let entry = |file: &str, kind| ImagePathEntry { file: file.into(), kind };
        assert_eq!(
            entries,
            [
                entry(".hidden.gif", "image"),
                entry("B.JPEG", "image"),
                entry("a.png", "image"),
                entry("assets", "directory"),
                entry("c.webp", "image"),
                entry("pics.png", "directory"),
                entry("screenpng", "image"),
            ]
        );
        assert_eq!(serde_json::to_value(&entries[3]).unwrap(), serde_json::json!({ "file": "assets", "type": "directory" }));
        assert!(tauri::async_runtime::block_on(image_path_entries(crate::display(&dir.join("missing")))).is_err());
    }
}
