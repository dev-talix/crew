pub mod claude;
pub mod codex;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, Command};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    /// Tokens the turn has used so far; a later event replaces an earlier one.
    Usage { input: u64, output: u64 },
    /// Claude's latest plan limit snapshot, as its rate_limit_event reports it.
    Limits(serde_json::Value),
    Session(String),
    Model(String),
    Delta(String),
    Tool(String),
    Done,
    Stopped,
    Failed(String),
}

pub type EventTx = mpsc::UnboundedSender<ProviderEvent>;

/// Where a side conversation's turn runs: its own key for the CLI process or thread, and its own session.
/// Main turns pass no lane and use the familiar's id and current session.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub key: String,
    pub session: LaneSession,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LaneSession {
    /// Carry on the side conversation's own session.
    Resume(String),
    /// Start from a copy of this session, the familiar's main chat.
    Fork(String),
    /// Start a new session: the familiar has no main session to copy yet.
    Fresh,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    pub is_default: bool,
}

pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];

static LOGIN_PATH: OnceLock<Option<String>> = OnceLock::new();

pub static USE_CLAUDE_SUBSCRIPTION: AtomicBool = AtomicBool::new(true);

const SUBSCRIPTION_OVERRIDES: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL"];

pub fn inherited_vars_to_drop(names: impl Iterator<Item = String>, use_subscription: bool) -> Vec<String> {
    names
        .filter(|name| {
            name == "CLAUDECODE"
                || name.starts_with("CLAUDE_CODE_")
                || name == "CLAUDE_AGENT_SDK_VERSION"
                || (use_subscription && SUBSCRIPTION_OVERRIDES.contains(&name.as_str()))
        })
        .collect()
}

fn login_shell_path() -> Option<String> {
    LOGIN_PATH
        .get_or_init(|| {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let output = std::process::Command::new(shell)
                .args(["-lc", "printf %s \"$PATH\""])
                .stdin(std::process::Stdio::null())
                .output()
                .ok()?;
            let path = String::from_utf8(output.stdout).ok()?;
            (output.status.success() && !path.trim().is_empty()).then(|| path.trim().to_string())
        })
        .clone()
}

pub fn command(program: &str, cwd: &str) -> Command {
    let mut cmd = Command::new(program);
    if let Some(path) = login_shell_path() {
        cmd.env("PATH", path);
    }
    let use_subscription = USE_CLAUDE_SUBSCRIPTION.load(Ordering::Relaxed);
    for name in inherited_vars_to_drop(std::env::vars().map(|(k, _)| k), use_subscription) {
        cmd.env_remove(name);
    }
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd
}

pub fn spawn_error(program: &str, err: std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::NotFound {
        format!("`{program}` is not installed or not on your PATH.")
    } else {
        format!("Could not start `{program}`: {err}")
    }
}

#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    const LINES: usize = 20;

    pub fn collect(&self, stderr: ChildStderr) {
        let tail = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut buf) = tail.0.lock() {
                    if buf.len() == Self::LINES {
                        buf.pop_front();
                    }
                    buf.push_back(line);
                }
            }
        });
    }

    pub fn text(&self) -> String {
        self.0
            .lock()
            .map(|buf| buf.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }
}

#[derive(Default)]
pub struct IdleTimer(Arc<AtomicU64>);

impl IdleTimer {
    pub fn arm(&self, after: Duration, on_idle: impl FnOnce() + Send + 'static) {
        let generation = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        let current = self.0.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(after).await;
            if current.load(Ordering::SeqCst) == generation {
                on_idle();
            }
        });
    }

    pub fn cancel(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn drops_nested_session_and_key_overrides() {
        let names = ["PATH", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "HOME"]
            .map(String::from);
        let mut dropped = inherited_vars_to_drop(names.clone().into_iter(), true);
        dropped.sort();
        assert_eq!(dropped, ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID"]);
        let kept_keys = inherited_vars_to_drop(names.into_iter(), false);
        assert!(!kept_keys.iter().any(|n| n.starts_with("ANTHROPIC")));
    }

    #[tokio::test]
    async fn idle_timer_fires_only_for_the_latest_arm() {
        let fired = Arc::new(AtomicUsize::new(0));
        let timer = IdleTimer::default();
        let short = Duration::from_millis(30);
        let count = |fired: &Arc<AtomicUsize>| {
            let fired = fired.clone();
            move || {
                fired.fetch_add(1, Ordering::SeqCst);
            }
        };

        timer.arm(short, count(&fired));
        timer.cancel();
        timer.arm(short, count(&fired));
        timer.arm(short, count(&fired));
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1);

        timer.arm(short, count(&fired));
        timer.cancel();
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }
}
