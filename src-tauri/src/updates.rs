//! Checks GitHub for a newer Grimoire release. Installing it is still a download for now: automatic
//! installs need signed release builds.

use serde::Serialize;
use serde_json::Value;

/// `owner/name` from the repository URL in Cargo.toml.
fn repository() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/").trim_end_matches('/')
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    pub url: String,
    pub published_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub current: String,
    /// The latest published release, if there is one.
    pub latest: Option<Release>,
    pub newer: bool,
}

/// Compares dotted version numbers, ignoring a leading "v" and anything after a "-".
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v').split('-').next().unwrap_or_default().split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    let (a, b) = (parts(candidate), parts(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

pub fn parse_release(body: &Value) -> Option<Release> {
    let tag = body["tag_name"].as_str()?;
    // The Download button opens this link, so Grimoire builds it for this repository rather than
    // trusting the html_url GitHub sends.
    let ours = format!("https://github.com/{}/releases", repository());
    let safe_tag = !tag.is_empty() && !tag.contains("..") && tag.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    let url = if safe_tag { format!("{ours}/tag/{tag}") } else { format!("{ours}/latest") };
    Some(Release { version: tag.trim_start_matches('v').to_string(), url, published_at: body["published_at"].as_str().map(String::from) })
}

pub async fn check() -> Result<UpdateCheck, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repository());
    // macOS ships curl, which keeps an HTTP client out of the app for one request a day.
    let output = tokio::process::Command::new("/usr/bin/curl")
        .args(["-sS", "--max-time", "15", "-w", "\n%{http_code}", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: Grimoire", &url])
        .output()
        .await
        .map_err(|e| format!("Could not check for updates: {e}"))?;
    if !output.status.success() {
        return Err(format!("Could not reach GitHub: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, status) = text.rsplit_once('\n').unwrap_or((&text, ""));
    let current = env!("CARGO_PKG_VERSION").to_string();
    let latest = match status.trim() {
        "404" => None,
        "200" => parse_release(&serde_json::from_str(body).map_err(|_| "GitHub sent an unreadable reply.")?),
        other => return Err(format!("GitHub answered with status {other}. Try again later.")),
    };
    let newer = latest.as_ref().is_some_and(|r| is_newer(&r.version, &current));
    Ok(UpdateCheck { current, latest, newer })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compares_versions_by_number() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.10.0", "1.9.3"));
        assert!(is_newer("0.1.1", "0.1"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn reads_a_github_release() {
        let body = json!({ "tag_name": "v0.2.0", "html_url": "https://github.com/dev-talix/grimoire/releases/tag/v0.2.0", "published_at": "2026-12-01T00:00:00Z" });
        assert_eq!(parse_release(&body).unwrap().version, "0.2.0");
        assert!(parse_release(&json!({ "message": "Not Found" })).is_none());
        assert!(!repository().contains("github.com") && repository().contains('/'));
    }

    #[test]
    fn only_links_to_this_repository() {
        let url = |tag: &str, html_url: &str| parse_release(&json!({ "tag_name": tag, "html_url": html_url })).unwrap().url;
        let ours = "https://github.com/dev-talix/grimoire/releases/tag/v0.2.0";
        for html_url in [ours, "https://evil.example/grimoire.dmg", "https://github.com/dev-talix/grimoire/%2e%2e/%2e%2e/other/project", "javascript:alert(1)"] {
            assert_eq!(url("v0.2.0", html_url), ours, "html_url is never used: {html_url}");
        }
        let fallback = "https://github.com/dev-talix/grimoire/releases/latest";
        for tag in ["../../other", "v1/../../x", "%2e%2e", "v1?x=1", "v1#x", "", "v 1"] {
            assert_eq!(url(tag, ours), fallback, "{tag}");
        }
    }
}
