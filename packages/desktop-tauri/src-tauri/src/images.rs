// The `mt-image` protocol that local images load through (src/localImages.ts),
// in place of Tauri's asset protocol: that one resolves each path before
// checking its scope, which fails on some drives (the Windows VM's shared
// folder answered 403). Only files whose content is an image are served,
// since an image is all the page asks for here.

use std::borrow::Cow;
use std::io::ErrorKind;

use tauri::http::{header, Request, Response, StatusCode};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

pub const SCHEME: &str = "mt-image";

pub fn protocol<R: Runtime>(context: UriSchemeContext<'_, R>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    // As the asset protocol does, so that the page may read the image's pixels.
    let origin = context
        .app_handle()
        .get_webview_window(context.webview_label())
        .and_then(|webview| webview.url().ok())
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default();
    // convertFileSrc encodes the whole path as one part of the address.
    let encoded = request.uri().path().trim_start_matches('/').to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let path = percent_encoding::percent_decode_str(&encoded).decode_utf8_lossy().into_owned();
        responder.respond(image_response(&path, &origin));
    });
}

fn image_response(path: &str, origin: &str) -> Response<Cow<'static, [u8]>> {
    let (status, mime, body) = match std::fs::read(path) {
        Ok(bytes) => match image_type(&bytes) {
            Some(mime) => (StatusCode::OK, Some(mime), bytes),
            None => (StatusCode::UNSUPPORTED_MEDIA_TYPE, None, Vec::new()),
        },
        Err(err) if err.kind() == ErrorKind::NotFound => (StatusCode::NOT_FOUND, None, Vec::new()),
        Err(err) if err.kind() == ErrorKind::PermissionDenied => (StatusCode::FORBIDDEN, None, Vec::new()),
        Err(err) => {
            eprintln!("[images] {path}: {err}");
            (StatusCode::INTERNAL_SERVER_ERROR, None, Vec::new())
        }
    };
    let mut response = Response::builder().status(status).header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    if let Some(mime) = mime {
        response = response.header(header::CONTENT_TYPE, mime);
    }
    response.body(Cow::Owned(body)).expect("the headers are valid")
}

// The image formats the webviews show, told by their first bytes; an SVG by
// an `<svg` element near the start of a text that begins with markup.
fn image_type(bytes: &[u8]) -> Option<&'static str> {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if starts(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("image/gif")
    } else if starts(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if starts(b"BM") {
        Some("image/bmp")
    } else if starts(b"\0\0\x01\0") {
        Some("image/x-icon")
    } else if matches!(bytes.get(4..12), Some(b"ftypavif" | b"ftypavis")) {
        Some("image/avif")
    } else if starts(b"II*\0") || starts(b"MM\0*") {
        Some("image/tiff")
    } else if is_svg(bytes) {
        Some("image/svg+xml")
    } else {
        None
    }
}

fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with('<') && text.to_ascii_lowercase().contains("<svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_told_by_their_content() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(image_type(b"\xff\xd8\xff\xe0...."), Some("image/jpeg"));
        assert_eq!(image_type(b"GIF89a...."), Some("image/gif"));
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_type(b"\0\0\0\x1cftypavif"), Some("image/avif"));
        assert_eq!(image_type(b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<svg xmlns=\"...\"/>"), Some("image/svg+xml"));
        assert_eq!(image_type(b"<SVG></SVG>"), Some("image/svg+xml"));
    }

    #[test]
    fn other_files_are_not_images() {
        assert_eq!(image_type(b"# Notes\n\n<svg> in text"), None);
        assert_eq!(image_type(b"-----BEGIN OPENSSH PRIVATE KEY-----"), None);
        assert_eq!(image_type(b"<html><body></body></html>"), None);
        assert_eq!(image_type(b""), None);
    }

    #[test]
    fn responses_carry_the_image_type_or_a_status() {
        let dir = std::env::temp_dir().join(format!("mt-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("pixel");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nrest").unwrap();
        let text = dir.join("notes.png");
        std::fs::write(&text, b"not an image").unwrap();

        let ok = image_response(&png.to_string_lossy(), "tauri://localhost");
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(ok.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(ok.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "tauri://localhost");
        assert_eq!(image_response(&text.to_string_lossy(), "").status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let missing = dir.join("missing.png");
        assert_eq!(image_response(&missing.to_string_lossy(), "").status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(dir);
    }
}
