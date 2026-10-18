// The Image settings' "Upload to cloud", standing in for
// packages/desktop/src/main/ipc/uploader.ts: PicGo or the user's own script
// uploads a local image and prints its web address. Both run without a shell
// and get the image's path as one argument, so a file name can hold any
// character. The script has to be named by an absolute path to a program,
// as the page cannot be allowed to run one picked out of PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::Value;

use crate::cmd;
use crate::fileops::{is_executable, paths_is_image};

// fileSystem.ts uploadImage: an image already on disk, or the bytes of a
// pasted one.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Image {
    Path(String),
    Bytes { data: Vec<u8>, name: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadPreferences {
    current_uploader: String,
    #[serde(default)]
    cli_script: String,
}

#[derive(Deserialize)]
pub struct UploadRequest {
    pathname: String,
    image: Image,
    preferences: UploadPreferences,
}

// The image's web address. A path that is not an image file comes back as
// it is, as in Electron.
#[tauri::command]
pub async fn upload_image(request: UploadRequest) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || upload(request)).await.map_err(|err| err.to_string())?
}

fn upload(request: UploadRequest) -> Result<String, String> {
    let UploadRequest { pathname, image, preferences } = request;
    match image {
        Image::Path(image) => {
            // Relative to the document's folder.
            let path = Path::new(&pathname).parent().unwrap_or(Path::new("")).join(&image);
            if !paths_is_image(path.to_string_lossy().into_owned()) {
                return Ok(image);
            }
            upload_file(&path, &preferences)
        }
        Image::Bytes { data, name } => {
            let file = TempImage::write(&data, &name)?;
            upload_file(&file.path, &preferences)
        }
    }
}

fn upload_file(path: &Path, preferences: &UploadPreferences) -> Result<String, String> {
    match preferences.current_uploader.as_str() {
        "picgo" => picgo(path),
        "cliScript" => script(&preferences.cli_script, path),
        other => Err(format!("Unsupported uploader: {other}")),
    }
}

fn picgo(path: &Path) -> Result<String, String> {
    let program = cmd::resolve("picgo").ok_or("PicGo command not found in PATH")?;
    let output = run(Command::new(program).arg("u").arg(path))?;
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        return Err(failure("PicGo", &output));
    }
    parse_picgo_output(&text)
        .ok_or_else(|| format!("PicGo upload error: cannot parse output\n{}", text.chars().take(400).collect::<String>()))
}

// The script prints the address and nothing else.
fn script(script: &str, path: &Path) -> Result<String, String> {
    let program = Path::new(script);
    if !program.is_absolute() || !is_executable(program) {
        return Err(format!("The upload script has to be an absolute path to a program: \"{script}\""));
    }
    let output = run(Command::new(program).arg(path))?;
    if !output.status.success() {
        return Err(failure(script, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// On the login shell's PATH (src/cmd.rs), and on Windows without a console
// window popping up.
fn run(command: &mut Command) -> Result<Output, String> {
    if let Some(path) = cmd::child_path() {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.stdin(Stdio::null()).output().map_err(|err| err.to_string())
}

fn failure(name: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr.trim();
    if message.is_empty() {
        format!("{name} failed ({})", output.status)
    } else {
        format!("{name} failed ({}):\n{}", output.status, message.chars().take(400).collect::<String>())
    }
}

// uploader.ts writeBinaryToTmp: a folder of its own for each upload, since
// two images pasted at once would otherwise share a name, and a timestamp as
// the name, which uploaders use for the remote file. Removed afterwards.
struct TempImage {
    dir: PathBuf,
    path: PathBuf,
}

static TEMP_IMAGES: AtomicU32 = AtomicU32::new(0);

impl TempImage {
    fn write(data: &[u8], name: &str) -> Result<Self, String> {
        let dir = loop {
            let count = TEMP_IMAGES.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("marktext-upload-{}-{count}", std::process::id()));
            match std::fs::create_dir(&dir) {
                Ok(()) => break dir,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(format!("{}: {err}", dir.display())),
            }
        };
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis());
        let path = dir.join(format!("{millis}{}", extension(name)));
        let file = Self { dir, path };
        std::fs::write(&file.path, data).map_err(|err| format!("{}: {err}", file.path.display()))?;
        Ok(file)
    }
}

impl Drop for TempImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// The pasted file's extension, such as ".png", if it is a plain one.
fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default()
}

// uploader.ts parsePicgoOutput: a JSON line PicGo prints with --json style
// output, a "success: <address>" line, or the address after
// "[PicGo SUCCESS]:".
fn parse_picgo_output(text: &str) -> Option<String> {
    let cleaned = strip_colors(text);
    for line in cleaned.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let bracketed = (line.starts_with('{') && line.ends_with('}')) || (line.starts_with('[') && line.ends_with(']'));
        if bracketed {
            if let Some(url) = serde_json::from_str::<Value>(line).ok().as_ref().and_then(json_address) {
                return Some(url);
            }
        }
        if let Some(url) = labelled_address(line) {
            return Some(url);
        }
    }
    let marker = "[PicGo SUCCESS]:";
    let candidate = cleaned[cleaned.rfind(marker)? + marker.len()..].trim();
    is_web_address(candidate).then(|| candidate.to_string())
}

fn json_address(value: &Value) -> Option<String> {
    if value.get("success") != Some(&Value::Bool(true)) {
        return None;
    }
    if let Some(url) = value.get("imgUrl").and_then(Value::as_str) {
        return Some(url.to_string());
    }
    if let Some(last) = value.get("result").and_then(Value::as_array).and_then(|result| result.last()) {
        return Some(last.as_str().map_or_else(|| last.to_string(), str::to_string));
    }
    value.get("url").and_then(Value::as_str).map(str::to_string)
}

// /\x1b\[[0-9;]*m/g, ANSI colours.
fn strip_colors(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\x1b[") {
        cleaned.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let params = after.find(|c: char| !(c.is_ascii_digit() || c == ';')).unwrap_or(after.len());
        if after[params..].starts_with('m') {
            rest = &after[params + 1..];
        } else {
            cleaned.push_str("\x1b[");
            rest = after;
        }
    }
    cleaned.push_str(rest);
    cleaned
}

fn is_web_address(text: &str) -> bool {
    let lower = text.get(..8).unwrap_or(text).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

// /(?:success|succeeded|uploaded)\s*:?\s*(https?:\/\/\S+)/i
fn labelled_address(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for (start, _) in lower.char_indices() {
        for label in ["success", "succeeded", "uploaded"] {
            if !lower[start..].starts_with(label) {
                continue;
            }
            let rest = line[start + label.len()..].trim_start();
            let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
            let address = rest.split(char::is_whitespace).next().unwrap_or("");
            let scheme = if address.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) { 8 } else { 7 };
            if is_web_address(address) && address.len() > scheme {
                return Some(address.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picgo_output_in_its_forms() {
        assert_eq!(
            parse_picgo_output("[PicGo INFO]: Before transform\n[PicGo SUCCESS]:\nhttps://i.example.com/a.png\n"),
            Some("https://i.example.com/a.png".into())
        );
        assert_eq!(
            parse_picgo_output("\x1b[32m[PicGo SUCCESS]:\x1b[39m \nhttps://i.example.com/b.png"),
            Some("https://i.example.com/b.png".into())
        );
        assert_eq!(
            parse_picgo_output(r#"{"success":true,"result":["https://x/1.png","https://x/2.png"]}"#),
            Some("https://x/2.png".into())
        );
        assert_eq!(parse_picgo_output(r#"{"success":true,"imgUrl":"https://x/i.png"}"#), Some("https://x/i.png".into()));
        assert_eq!(parse_picgo_output("Uploaded: https://x/u.png done"), Some("https://x/u.png".into()));
        assert_eq!(parse_picgo_output("upload succeeded https://x/s.png"), Some("https://x/s.png".into()));
        assert_eq!(parse_picgo_output(r#"{"success":false,"url":"https://x/f.png"}"#), None);
        assert_eq!(parse_picgo_output("[PicGo ERROR]: no config\n"), None);
        assert_eq!(parse_picgo_output("success: https://"), None);
        assert_eq!(parse_picgo_output("téléversé — success: https://x/é.png"), Some("https://x/é.png".into()));
    }

    #[test]
    fn colours_are_stripped() {
        assert_eq!(strip_colors("\x1b[1;32mok\x1b[0m and \x1b[Kkept"), "ok and \x1b[Kkept");
    }

    #[test]
    fn pasted_images_keep_plain_extensions_only() {
        assert_eq!(extension("clip.png"), ".png");
        assert_eq!(extension("photo.JPEG"), ".JPEG");
        assert_eq!(extension("no-extension"), "");
        assert_eq!(extension("odd.p g"), "");
        assert_eq!(extension("../../x.gif"), ".gif");
    }

    #[test]
    fn temporary_images_are_removed_afterwards() {
        let file = TempImage::write(b"\x89PNG", "clip.png").unwrap();
        let dir = file.dir.clone();
        assert!(file.path.is_file());
        assert!(file.path.extension().is_some_and(|ext| ext == "png"));
        drop(file);
        assert!(!dir.exists());
    }

    #[test]
    fn the_script_must_be_an_absolute_program() {
        let preferences = |script: &str| UploadPreferences { current_uploader: "cliScript".into(), cli_script: script.into() };
        let image = Path::new("a.png");
        assert!(upload_file(image, &preferences("")).is_err());
        assert!(upload_file(image, &preferences("upload.sh")).is_err());
        let missing = std::env::temp_dir().join("mt-no-such-script");
        assert!(upload_file(image, &preferences(&missing.to_string_lossy())).is_err());
        let other = UploadPreferences { current_uploader: "github".into(), cli_script: String::new() };
        assert_eq!(upload_file(image, &other), Err("Unsupported uploader: github".into()));
    }

    // The path reaches the script as one argument, quotes and all.
    #[cfg(unix)]
    #[test]
    fn the_script_gets_the_path_and_its_output_is_the_address() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("mt-upload-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("upload.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'https://example.com/%s\\n' \"$1\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let image = dir.join("it's a \"pic\" $HOME.png");
        let preferences = UploadPreferences { current_uploader: "cliScript".into(), cli_script: script.to_string_lossy().into() };
        assert_eq!(upload_file(&image, &preferences), Ok(format!("https://example.com/{}", image.display())));
        let _ = std::fs::remove_dir_all(dir);
    }
}
