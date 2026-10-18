// pandoc for the File menu's Import and Convert with Pandoc, standing in for
// packages/desktop/src/main/utils/pandoc.ts. The page decides what to convert
// (src/pandoc.ts); each command builds pandoc's arguments itself and checks
// what the page sends, because pandoc can run other programs (--filter,
// --lua-filter) and the page must not be able to ask it to.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cmd;
use crate::fileops::is_executable;

// The writers of the menu's formats (pandoc.ts PANDOC_EXPORT_FORMATS).
const TARGETS: [&str; 9] = ["docx", "odt", "rtf", "epub3", "latex", "rst", "org", "mediawiki", "textile"];

// pandoc.ts resolve and findCommand: the MARKTEXT_PANDOC override if it can
// run (and is no batch file on Windows, as Electron cannot start one without a
// shell), else pandoc's Windows install folders, else the usual lookup
// (src/cmd.rs). A hit is kept for the run; a miss is looked for again.
fn find() -> Option<PathBuf> {
    static FOUND: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut found = FOUND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if found.is_none() {
        *found = locate();
    }
    found.clone()
}

fn locate() -> Option<PathBuf> {
    if let Some(chosen) = std::env::var_os("MARKTEXT_PANDOC").filter(|value| !value.is_empty()) {
        let chosen = PathBuf::from(chosen);
        return (!is_batch_file(&chosen) && is_executable(&chosen)).then_some(chosen);
    }
    install_folders().into_iter().find(|path| is_executable(path)).or_else(|| cmd::resolve("pandoc"))
}

fn is_batch_file(path: &Path) -> bool {
    cfg!(windows)
        && path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "bat" | "cmd"))
}

#[cfg(windows)]
fn install_folders() -> Vec<PathBuf> {
    ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|base| PathBuf::from(base).join("Pandoc").join("pandoc.exe"))
        .collect()
}

#[cfg(not(windows))]
fn install_folders() -> Vec<PathBuf> {
    Vec::new()
}

// `mt::pandoc::command`: the pandoc that would run, for the General page.
#[tauri::command]
pub async fn pandoc_command() -> Option<String> {
    tauri::async_runtime::spawn_blocking(|| find().map(|path| path.to_string_lossy().into_owned())).await.ok().flatten()
}

// Runs pandoc with `input` on stdin, written from another thread so that a
// large output cannot block the writing.
fn run(args: &[String], input: Option<&str>, cwd: Option<&Path>) -> Result<Output, String> {
    let program = find().ok_or("pandoc was not found")?;
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if let Some(path) = cmd::child_path() {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|err| format!("pandoc: {err}"))?;
    let stdin = child.stdin.take();
    let input = input.unwrap_or_default().to_string();
    // pandoc may stop reading early; the failure then shows in its exit code.
    let writer = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(input.as_bytes());
        }
    });
    let output = child.wait_with_output().map_err(|err| format!("pandoc: {err}"));
    let _ = writer.join();
    output
}

fn failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    match output.status.code() {
        Some(code) => format!("pandoc exited with code {code}"),
        None => format!("pandoc stopped ({})", output.status),
    }
}

// file.ts openPandocFile: pandoc picks the reader from the file's extension.
// Electron opens an empty tab when the conversion fails; here it fails. The
// path is absolute, as the dialogs and drops give it, so that it cannot be
// read as an option.
#[tauri::command]
pub async fn pandoc_import(path: String) -> Result<String, String> {
    if !Path::new(&path).is_absolute() {
        return Err(format!("Not an absolute path: {path}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let output = run(&["-s".into(), path, "-t".into(), "markdown".into()], None, None)?;
        if !output.status.success() {
            return Err(failure(&output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    })
    .await
    .map_err(|err| err.to_string())?
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Extension {
    name: String,
    enabled: bool,
}

#[derive(Clone, Serialize)]
pub struct ReaderExtensions {
    gfm: Vec<Extension>,
    markdown: Vec<Extension>,
}

// pandoc.ts getReaderExtensions: what the installed pandoc's gfm and markdown
// readers carry, and whether each is on by default. Kept for the run once
// both listings came through.
#[tauri::command]
pub async fn pandoc_reader_extensions() -> Option<ReaderExtensions> {
    static LISTED: Mutex<Option<ReaderExtensions>> = Mutex::new(None);
    tauri::async_runtime::spawn_blocking(|| {
        let mut listed = LISTED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if listed.is_none() {
            *listed = Some(ReaderExtensions { gfm: list_extensions("gfm")?, markdown: list_extensions("markdown")? });
        }
        listed.clone()
    })
    .await
    .ok()
    .flatten()
}

fn list_extensions(format: &str) -> Option<Vec<Extension>> {
    let output = run(&[format!("--list-extensions={format}")], None, None).ok()?;
    if !output.status.success() {
        return None;
    }
    let extensions = parse_extensions(&String::from_utf8_lossy(&output.stdout));
    (!extensions.is_empty()).then_some(extensions)
}

// Lines such as "+footnotes" or "-emoji".
fn parse_extensions(listing: &str) -> Vec<Extension> {
    listing
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            let enabled = line.starts_with('+');
            let name = line.strip_prefix(['+', '-'])?;
            (!name.is_empty()).then(|| Extension { name: name.to_string(), enabled })
        })
        .collect()
}

// A reader the page built (pandoc.ts getPandocReader): gfm or markdown and
// extension switches, nothing that pandoc could read as another option.
fn is_reader(reader: &str) -> bool {
    let rest = reader.strip_prefix("markdown").or_else(|| reader.strip_prefix("gfm"));
    rest.is_some_and(|rest| {
        rest.split(['+', '-']).skip(1).all(|name| {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        }) && (rest.is_empty() || rest.starts_with(['+', '-']))
    })
}

// pandoc.ts listLinkedMedia: the addresses of the document's images, from
// pandoc's syntax tree. Empty when pandoc cannot read the document.
#[tauri::command]
pub async fn pandoc_linked_media(markdown: String, reader: String) -> Vec<String> {
    if !is_reader(&reader) {
        return Vec::new();
    }
    tauri::async_runtime::spawn_blocking(move || {
        let args = ["-f".to_string(), reader, "-t".into(), "json".into()];
        let Ok(output) = run(&args, Some(&markdown), None) else { return Vec::new() };
        if !output.status.success() {
            return Vec::new();
        }
        let mut urls = Vec::new();
        if let Ok(tree) = serde_json::from_slice::<Value>(&output.stdout) {
            collect_images(&tree, &mut urls);
        }
        urls
    })
    .await
    .unwrap_or_default()
}

// An Image node is {"t": "Image", "c": [attributes, alt text, [url, title]]}.
fn collect_images(value: &Value, urls: &mut Vec<String>) {
    match value {
        Value::Object(node) => {
            if node.get("t").and_then(Value::as_str) == Some("Image") {
                if let Some(url) = node.get("c").and_then(|c| c.get(2)).and_then(|target| target.get(0)).and_then(Value::as_str) {
                    urls.push(url.to_string());
                }
            }
            node.values().for_each(|child| collect_images(child, urls));
        }
        Value::Array(items) => items.iter().for_each(|child| collect_images(child, urls)),
        _ => {}
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    target: String,
    output_path: String,
    markdown: String,
    reader: String,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    cwd: Option<String>,
    #[serde(default)]
    mirror_media: bool,
    resource_path: Option<String>,
}

// pandoc.ts toFile: pandoc writes the file itself, binary formats included,
// and its warnings come back.
#[tauri::command]
pub async fn pandoc_export(request: ExportRequest) -> Result<String, String> {
    let args = export_args(&request)?;
    tauri::async_runtime::spawn_blocking(move || {
        // Images are copied next to the output when they are mirrored.
        let cwd = if request.mirror_media {
            Path::new(&request.output_path).parent().map(Path::to_path_buf)
        } else {
            request.cwd.as_ref().map(PathBuf::from)
        };
        let output = run(&args, Some(&request.markdown), cwd.as_deref())?;
        if !output.status.success() {
            return Err(failure(&output));
        }
        Ok(String::from_utf8_lossy(&output.stderr).trim().to_string())
    })
    .await
    .map_err(|err| err.to_string())?
}

fn export_args(request: &ExportRequest) -> Result<Vec<String>, String> {
    if !TARGETS.contains(&request.target.as_str()) {
        return Err(format!("Unknown pandoc format: {}", request.target));
    }
    if !is_reader(&request.reader) {
        return Err(format!("Not a pandoc reader: {}", request.reader));
    }
    let mut args = vec!["-f".into(), request.reader.clone(), "-t".into(), request.target.clone(), "-s".into()];
    for (key, value) in &request.metadata {
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err(format!("Not a metadata field: {key}"));
        }
        if !value.is_empty() {
            args.push(format!("--metadata={key}:{value}"));
        }
    }
    if request.mirror_media {
        args.push("--extract-media=.".into());
        if let Some(dir) = &request.resource_path {
            args.push(format!("--resource-path={dir}"));
        }
    }
    args.push("-o".into());
    args.push(request.output_path.clone());
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(target: &str, reader: &str) -> ExportRequest {
        ExportRequest {
            target: target.into(),
            output_path: "/out/doc.tex".into(),
            markdown: String::new(),
            reader: reader.into(),
            metadata: BTreeMap::from([("lang".into(), "en".into()), ("title".into(), String::new())]),
            cwd: None,
            mirror_media: false,
            resource_path: Some("/notes".into()),
        }
    }

    #[test]
    fn readers_are_gfm_or_markdown_with_switches() {
        assert!(is_reader("gfm"));
        assert!(is_reader("gfm+superscript+subscript-footnotes"));
        assert!(is_reader("markdown-tex_math_dollars+tex_math_single_backslash"));
        assert!(!is_reader("gfm --filter=evil"));
        assert!(!is_reader("gfm+"));
        assert!(!is_reader("html"));
        assert!(!is_reader("gfmx"));
        assert!(!is_reader("--lua-filter=x.lua"));
    }

    #[test]
    fn export_arguments_follow_electron() {
        let args = export_args(&request("latex", "gfm")).unwrap();
        assert_eq!(args, ["-f", "gfm", "-t", "latex", "-s", "--metadata=lang:en", "-o", "/out/doc.tex"]);
        let mut mirrored = request("latex", "gfm");
        mirrored.mirror_media = true;
        let args = export_args(&mirrored).unwrap();
        assert_eq!(&args[6..], ["--extract-media=.", "--resource-path=/notes", "-o", "/out/doc.tex"]);
    }

    #[test]
    fn export_refuses_unknown_formats_readers_and_fields() {
        assert!(export_args(&request("html", "gfm")).is_err());
        assert!(export_args(&request("docx", "gfm --filter=x")).is_err());
        let mut odd = request("docx", "gfm");
        odd.metadata.insert("--filter".into(), "x".into());
        assert!(export_args(&odd).is_err());
    }

    #[test]
    fn extension_listings_are_parsed() {
        let listed = parse_extensions("+footnotes\n-emoji\n\n+tex_math_dollars\nnoise\n");
        assert_eq!(
            listed,
            [
                Extension { name: "footnotes".into(), enabled: true },
                Extension { name: "emoji".into(), enabled: false },
                Extension { name: "tex_math_dollars".into(), enabled: true },
            ]
        );
    }

    #[test]
    fn images_are_found_in_the_syntax_tree() {
        let tree: Value = serde_json::from_str(
            r#"{"blocks":[{"t":"Para","c":[{"t":"Image","c":[["",[],[]],[{"t":"Str","c":"a"}],["pic.png",""]]},
            {"t":"Emph","c":[{"t":"Image","c":[["",[],[]],[],["https://x/y.png","t"]]}]}]}]}"#,
        )
        .unwrap();
        let mut urls = Vec::new();
        collect_images(&tree, &mut urls);
        assert_eq!(urls, ["pic.png", "https://x/y.png"]);
    }
}
