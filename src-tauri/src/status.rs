use crate::providers::command;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub provider: &'static str,
    pub installed: bool,
    pub version: Option<String>,
    pub signed_in: Option<bool>,
    pub method: Option<String>,
    pub detail: Option<String>,
}

async fn run(program: &str, args: &[&str]) -> Option<(bool, String)> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let mut cmd = command(program, &home);
    cmd.args(args).stdin(std::process::Stdio::null());
    let output = tokio::time::timeout(Duration::from_secs(15), cmd.output()).await.ok()?.ok()?;
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    Some((output.status.success(), text.trim().to_string()))
}

fn first_version(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))
        .map(str::to_string)
}

pub fn parse_claude_auth(text: &str) -> (Option<bool>, Option<String>) {
    let start = text.find('{');
    let parsed = start.and_then(|s| serde_json::from_str::<Value>(&text[s..]).ok());
    match parsed {
        Some(json) => {
            let signed_in = json["loggedIn"].as_bool();
            let method = match json["authMethod"].as_str() {
                Some("oauth_token") | Some("claude.ai") => Some("Claude subscription".to_string()),
                Some("api_key") | Some("apiKey") => Some("API key".to_string()),
                Some(other) => Some(other.replace('_', " ")),
                None => None,
            };
            (signed_in, method)
        }
        None => (None, None),
    }
}

pub fn parse_codex_login(text: &str) -> (Option<bool>, Option<String>) {
    let lower = text.to_lowercase();
    if lower.contains("not logged in") {
        (Some(false), None)
    } else if lower.contains("chatgpt") {
        (Some(true), Some("ChatGPT plan".into()))
    } else if lower.contains("api key") {
        (Some(true), Some("API key".into()))
    } else if lower.contains("logged in") {
        (Some(true), None)
    } else {
        (None, None)
    }
}

pub async fn check(provider: &'static str) -> ProviderStatus {
    let version = run(provider, &["--version"]).await;
    let Some((true, version_text)) = version else {
        return ProviderStatus {
            provider,
            installed: false,
            version: None,
            signed_in: None,
            method: None,
            detail: Some(format!("Install the `{provider}` command-line tool, then check again.")),
        };
    };
    let (signed_in, method, raw) = if provider == "claude" {
        let output = run("claude", &["auth", "status"]).await.map(|(_, t)| t).unwrap_or_default();
        let (signed_in, method) = parse_claude_auth(&output);
        (signed_in, method, output)
    } else {
        let output = run("codex", &["login", "status"]).await.map(|(_, t)| t).unwrap_or_default();
        let (signed_in, method) = parse_codex_login(&output);
        (signed_in, method, output)
    };
    let detail = match signed_in {
        Some(false) => Some(format!("Run `{provider} {}` in a terminal to sign in.", if provider == "claude" { "auth login" } else { "login" })),
        None => Some(format!("Could not read the sign-in state: {}", raw.lines().next().unwrap_or("no output"))),
        Some(true) => None,
    };
    ProviderStatus { provider, installed: true, version: first_version(&version_text), signed_in, method, detail }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claude_auth_json() {
        let text = r#"{"loggedIn": true, "authMethod": "oauth_token", "apiProvider": "firstParty"}"#;
        assert_eq!(parse_claude_auth(text), (Some(true), Some("Claude subscription".into())));
        assert_eq!(parse_claude_auth(r#"{"loggedIn": false}"#), (Some(false), None));
        assert_eq!(parse_claude_auth("command not found"), (None, None));
    }

    #[test]
    fn reads_codex_login_text() {
        assert_eq!(parse_codex_login("Logged in using ChatGPT"), (Some(true), Some("ChatGPT plan".into())));
        assert_eq!(parse_codex_login("Logged in using an API key - sk-***"), (Some(true), Some("API key".into())));
        assert_eq!(parse_codex_login("Not logged in"), (Some(false), None));
    }

    #[test]
    fn finds_the_version_number() {
        assert_eq!(first_version("2.1.330 (Claude Code)").as_deref(), Some("2.1.330"));
        assert_eq!(first_version("codex-cli 0.171.0").as_deref(), Some("0.171.0"));
    }
}
