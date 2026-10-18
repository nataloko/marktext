// The two requests the page asks the main process to make, in place of
// Electron's net.fetch (packages/desktop/src/main/ipc/linkHtml.ts and
// main/ipc/diagram.ts): a pasted link's page, for its title, and a rendered
// PlantUML diagram, for exporting it. The page cannot make them itself, since
// the sites do not allow cross-origin reads. Certificates are checked against
// the system's store, and a proxy is taken from the environment (and from the
// Windows settings), where Chromium would use the system's settings.

use std::time::Duration;

use serde::Serialize;
use tauri::State;
use ureq::tls::{RootCerts, TlsConfig};

use crate::preferences::Preferences;

const USER_AGENT: &str = concat!("MarkText/", env!("CARGO_PKG_VERSION"));

fn agent(timeout: Duration, max_redirects: u32) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(max_redirects)
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .build()
        .into()
}

const LINK_TIMEOUT: Duration = Duration::from_millis(1500);
const LINK_MAX_BYTES: u64 = 1024 * 1024;

// The page at a pasted address, or "" for anything but an HTML page that
// arrives in time and within the size limit.
#[tauri::command]
pub async fn fetch_link_html(url: String) -> String {
    tauri::async_runtime::spawn_blocking(move || link_html(&url).unwrap_or_default()).await.unwrap_or_default()
}

fn link_html(url: &str) -> Option<String> {
    let parsed = tauri::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let mut response = agent(LINK_TIMEOUT, 10).get(url).call().ok()?;
    let content_type = response.headers().get("content-type").and_then(|value| value.to_str().ok()).unwrap_or("");
    if response.status() != 200 || !is_html(content_type) {
        return None;
    }
    let bytes = response.body_mut().with_config().limit(LINK_MAX_BYTES).read_to_vec().ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

// /^text\/html\b/i
fn is_html(content_type: &str) -> bool {
    let bytes = content_type.as_bytes();
    bytes.len() >= 9
        && bytes[..9].eq_ignore_ascii_case(b"text/html")
        && !bytes.get(9).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

// plantumlUrl.ts: an empty preference means this server, which Muya renders
// against too.
const PLANTUML_DEFAULT_SERVER: &str = "https://www.plantuml.com/plantuml";
const PLANTUML_TIMEOUT: Duration = Duration::from_secs(15);
const PLANTUML_MAX_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Serialize)]
pub struct Diagram {
    mime: String,
    data: Vec<u8>,
}

// A rendered diagram from the PlantUML server in the preferences.
#[tauri::command]
pub async fn fetch_plantuml(
    preferences: State<'_, Preferences>,
    server: String,
    encoded: String,
    format: String,
) -> Result<Diagram, String> {
    let configured = preferences.get("plantumlServer").and_then(|value| value.as_str().map(String::from)).unwrap_or_default();
    let url = plantuml_url(&server, &encoded, &format, &configured).ok_or("Invalid PlantUML request")?;
    tauri::async_runtime::spawn_blocking(move || plantuml_image(&url))
        .await
        .map_err(|err| err.to_string())?
        .inspect_err(|err| eprintln!("[plantuml] {err}"))
}

// plantumlUrl.ts buildPlantumlUrl. The page names the server, but only the one
// in the preferences may be reached; otherwise the page could have Rust fetch
// from any host, such as one on the local network.
fn plantuml_url(server: &str, encoded: &str, format: &str, configured: &str) -> Option<String> {
    if !matches!(format, "svg" | "png") || !is_server(server) || !is_encoded(encoded) {
        return None;
    }
    let configured = configured.trim().trim_end_matches('/');
    let allowed = if configured.is_empty() { PLANTUML_DEFAULT_SERVER } else { configured };
    if !is_server(allowed) || server.trim().trim_end_matches('/') != allowed {
        return None;
    }
    Some(format!("{allowed}/{format}/{encoded}"))
}

// /^https?:\/\/[^\s?#@]{1,512}$/
fn is_server(server: &str) -> bool {
    let Some(rest) = server.strip_prefix("https://").or_else(|| server.strip_prefix("http://")) else {
        return false;
    };
    (1..=512).contains(&rest.chars().count()) && !rest.chars().any(|c| c.is_whitespace() || matches!(c, '?' | '#' | '@'))
}

// /^[A-Za-z0-9_-]{1,20000}$/
fn is_encoded(encoded: &str) -> bool {
    (1..=20000).contains(&encoded.len()) && encoded.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

// plantumlFetch.ts fetchPlantumlImage. Redirects are not followed: the address
// was checked, and a redirect would lead to a host that was not.
fn plantuml_image(url: &str) -> Result<Diagram, String> {
    let mut response = agent(PLANTUML_TIMEOUT, 0).get(url).call().map_err(|err| err.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).map(String::from);
    let mime = header("content-type").unwrap_or_default();
    if !mime.starts_with("image/") {
        return Err(format!("Unexpected content type {mime}"));
    }
    if header("content-length").and_then(|length| length.parse::<u64>().ok()).is_some_and(|length| length > PLANTUML_MAX_BYTES) {
        return Err("Response too large".into());
    }
    let data = response.body_mut().with_config().limit(PLANTUML_MAX_BYTES).read_to_vec().map_err(|err| match err {
        ureq::Error::BodyExceedsLimit(_) => "Response too large".to_string(),
        other => other.to_string(),
    })?;
    Ok(Diagram { mime, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_content_types() {
        assert!(is_html("text/html"));
        assert!(is_html("Text/HTML; charset=utf-8"));
        assert!(!is_html("text/htmlx"));
        assert!(!is_html("text/plain"));
        assert!(!is_html("text/ht"));
        assert!(!is_html(""));
    }

    #[test]
    fn only_web_addresses_are_fetched() {
        assert_eq!(link_html("file:///etc/passwd"), None);
        assert_eq!(link_html("not a url"), None);
    }

    #[test]
    fn plantuml_goes_to_the_configured_server_only() {
        let default = "https://www.plantuml.com/plantuml";
        assert_eq!(plantuml_url(&format!("{default}/"), "SoWk-IImg_A", "svg", ""), Some(format!("{default}/svg/SoWk-IImg_A")));
        assert_eq!(plantuml_url("https://example.com/plantuml", "abc", "svg", ""), None);
        assert_eq!(plantuml_url("http://localhost:8080/", "abc", "png", "http://localhost:8080"), Some("http://localhost:8080/png/abc".into()));
        assert_eq!(plantuml_url(default, "abc", "pdf", ""), None);
        assert_eq!(plantuml_url(default, "a/b", "svg", ""), None);
        assert_eq!(plantuml_url(default, "", "svg", ""), None);
        assert_eq!(plantuml_url("https://user@host", "abc", "svg", "https://user@host"), None);
        assert_eq!(plantuml_url("ftp://host", "abc", "svg", "ftp://host"), None);
    }
}
