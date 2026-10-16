//! Best-effort version checks. This command never downloads or installs an update.
use std::time::Duration;

const UPDATE_URL: &str = "https://raw.githubusercontent.com/bestyize/FreeApiBox/refs/heads/main/update.txt";
const MAX_VERSION_BYTES: usize = 128;

fn parse_version(raw: &str) -> Option<[u64; 3]> {
    let text = raw.trim().trim_start_matches('\u{feff}').trim();
    let text = text.strip_prefix('v').unwrap_or(text);
    let parts: Vec<_> = text.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut version = [0; 3];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        version[i] = part.parse().ok()?;
    }
    Some(version)
}

fn newer_version(raw: &str, current: &str) -> Option<String> {
    let remote = parse_version(raw)?;
    (remote > parse_version(current)?).then(|| format!("{}.{}.{}", remote[0], remote[1], remote[2]))
}

async fn fetch_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .build().ok()?;
    let mut response = client.get(UPDATE_URL)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send().await.ok()?;
    if !response.status().is_success()
        || response.content_length().is_some_and(|len| len > MAX_VERSION_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > MAX_VERSION_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    newer_version(std::str::from_utf8(&body).ok()?, env!("CARGO_PKG_VERSION"))
}

/// Unreachable, malformed, equal and older versions all return None without an error prompt.
#[tauri::command]
pub async fn check_for_update() -> Option<String> {
    fetch_version().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_newer_numeric_versions_are_reported() {
        assert_eq!(newer_version(" \u{feff}v0.8.12\r\n", "0.8.11"), Some("0.8.12".into()));
        assert_eq!(newer_version("0.8.10", "0.8.9"), Some("0.8.10".into()));
        assert_eq!(newer_version("0.9.0", "0.8.99"), Some("0.9.0".into()));
        assert_eq!(newer_version("1.0.0", "0.99.99"), Some("1.0.0".into()));
        for raw in ["0.8.11", "0.8.9", "", "404: Not Found", "<html>", "0.8", "0.8.12-beta", "0.8.12\ntext", "+1.0.0"] {
            assert_eq!(newer_version(raw, "0.8.11"), None, "{raw}");
        }
    }
}
