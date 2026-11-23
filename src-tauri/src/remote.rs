//! Familiars on another Mac. Grimoire runs the provider CLI there through the system `ssh`, with the
//! person's own keys and agent, and talks to it over the same stdio streams it uses locally.
//!
//! The host and the remote workspace never pass through a shell on this Mac: `ssh` gets them as
//! separate arguments. The remote command does pass through the remote shell, so every piece of it
//! is single-quoted with `quote`.

use crate::db::{Agent, Db};
use serde::Serialize;
use std::collections::BTreeSet;
use std::future::Future;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::process::Command;

const MAX_HOST: usize = 255;
const MAX_PATH: usize = 1024;
const TIMEOUT: Duration = Duration::from_secs(20);
/// How long a setup try before a turn, or in the background, waits before calling the Mac offline.
const QUICK: Duration = Duration::from_secs(8);
/// How often Grimoire retries familiars waiting on their Mac.
const RETRY_EVERY: Duration = Duration::from_secs(180);

/// The SSH destination, like `me@studio.local` or an alias from ~/.ssh/config. Empty means this Mac.
pub fn clean_host(host: Option<&str>) -> Result<Option<String>, String> {
    let Some(host) = host.map(str::trim).filter(|h| !h.is_empty()) else { return Ok(None) };
    // '%' allows a scoped IPv6 address like fe80::1%en0.
    let allowed = |c: char| c.is_ascii_alphanumeric() || "._-@:%".contains(c);
    let (user, name) = match host.split_once('@') {
        Some((user, name)) => (Some(user), name),
        None => (None, host),
    };
    let ok = host.len() <= MAX_HOST
        && host.chars().all(allowed)
        && !host.starts_with('-')
        && user.is_none_or(|u| !u.is_empty())
        && !name.is_empty()
        && !name.contains('@')
        && !name.starts_with(['-', '.']);
    if ok {
        Ok(Some(host.to_string()))
    } else {
        Err("Write the other Mac as an SSH destination, like me@studio.local or an alias from ~/.ssh/config.".into())
    }
}

/// The workspace on the other Mac: a full path or one under ~/, never the root or the home folder.
/// No path picks a new folder in ~/Grimoire there, like a local familiar gets.
pub fn clean_workspace(requested: Option<&str>, name: &str, id: &str) -> Result<String, String> {
    let Some(path) = requested.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(format!("~/Grimoire/{}-{}", crate::slug(name, "agent"), &id[..6]));
    };
    let path = path.trim_end_matches('/');
    let rest = path.strip_prefix("~/").or_else(|| path.strip_prefix('/'));
    let ok = path.len() <= MAX_PATH
        && !path.chars().any(char::is_control)
        && rest.is_some_and(|r| !r.is_empty() && !r.split('/').any(|segment| segment == ".."));
    if ok {
        Ok(path.to_string())
    } else {
        Err("The workspace on the other Mac must be a full path or start with ~/, like ~/Projects/site, without '..'.".into())
    }
}

/// One word for a POSIX shell: single quotes, with each `'` closed, escaped, and reopened.
pub fn quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// A remote workspace for the remote shell. `~/` becomes `"$HOME"/` so it still points at home.
fn quote_path(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"/{}", quote(rest)),
        None => quote(path),
    }
}

/// Runs `script` in a login shell, so PATH includes what the person's profile adds, like Homebrew
/// or ~/.local/bin. sshd runs commands in a non-login shell that often lacks those. Grimoire reads
/// PATH from a login shell locally too.
fn in_login_shell(script: &str) -> String {
    format!("exec \"$SHELL\" -lc {}", quote(script))
}

/// The command the remote Mac runs for a familiar: make and enter its workspace, then become the CLI.
/// Making the folder here too covers imported familiars and a folder someone deleted.
pub fn launch_script(workspace: &str, program: &str, args: &[String]) -> String {
    let dir = quote_path(workspace);
    let mut script = format!("mkdir -p -- {dir} && cd -- {dir} && exec {}", quote(program));
    for arg in args {
        script.push(' ');
        script.push_str(&quote(arg));
    }
    in_login_shell(&script)
}

pub fn ssh_args(host: &str, remote_command: String) -> Vec<String> {
    let mut args: Vec<String> = ["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=30"]
        .map(String::from)
        .to_vec();
    args.extend([host.to_string(), "--".into(), remote_command]);
    args
}

fn ssh(host: &str, remote_command: String) -> Command {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let mut cmd = crate::providers::command("ssh", &home);
    cmd.args(ssh_args(host, remote_command));
    cmd
}

/// The provider CLI on `host`, ready to spawn with piped stdio like a local one.
pub fn command(host: &str, workspace: &str, program: &str, args: &[String]) -> Command {
    ssh(host, launch_script(workspace, program, args))
}

/// Why an ssh command didn't succeed: the Mac never answered, or it answered and something failed.
#[derive(Debug, PartialEq)]
pub enum SshError {
    Offline,
    Failed(String),
}

async fn run(host: &str, remote_command: String, timeout: Duration) -> Result<std::process::Output, SshError> {
    let mut cmd = ssh(host, remote_command);
    cmd.stdin(Stdio::null());
    tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| SshError::Offline)?
        .map_err(|e| SshError::Failed(crate::providers::spawn_error("ssh", e)))
}

/// How ssh itself starts a line when it can't reach the other Mac or loses it, as opposed to reaching
/// it and being turned away, like a refused key.
const OFFLINE_SIGNS: &[&str] = &[
    "ssh: connect to host ",
    "ssh: Could not resolve hostname ",
    "kex_exchange_identification:",
    "Connection timed out during banner exchange",
    "Connection closed by UNKNOWN",
    "client_loop: send disconnect:",
    "Timeout, server ",
];

/// Whether a failed process's stderr says ssh couldn't reach the other Mac.
pub fn offline_text(stderr: &str) -> bool {
    stderr.lines().map(str::trim).any(|line| OFFLINE_SIGNS.iter().any(|sign| line.starts_with(sign)))
}

/// ssh exits with 255 for its own errors. With no message, or one about the connection, the Mac didn't
/// answer. A refused key or a changed host key also exits 255, but those need the person, so they stay
/// errors.
pub fn unreachable(code: Option<i32>, stderr: &str) -> bool {
    code == Some(255) && (stderr.trim().is_empty() || offline_text(stderr))
}

fn failure(host: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    match (output.status.code(), detail.is_empty()) {
        (_, false) => format!("{host}: {detail}"),
        (Some(255), true) => format!("Could not connect to {host}."),
        (code, true) => format!("{host}: the command ended with code {}.", code.unwrap_or(-1)),
    }
}

/// Creates the familiar's workspace on `host`, which also proves Grimoire can reach it.
async fn make_workspace(host: &str, workspace: &str, timeout: Duration) -> Result<(), SshError> {
    let output = run(host, format!("mkdir -p -- {}", quote_path(workspace)), timeout).await?;
    if output.status.success() {
        Ok(())
    } else if unreachable(output.status.code(), &String::from_utf8_lossy(&output.stderr)) {
        Err(SshError::Offline)
    } else {
        Err(SshError::Failed(format!("Could not create the workspace on the other Mac. {}", failure(host, &output))))
    }
}

/// The setup when someone summons a remote familiar. Returns whether it's saved with setup pending:
/// an offline Mac still gets the familiar, and any other failure, like a bad path, stops it.
/// `setup` is `make_workspace` in the app.
pub async fn setup_on_create<F, Fut>(host: &str, workspace: &str, setup: F) -> Result<bool, String>
where
    F: FnOnce(String, String) -> Fut,
    Fut: Future<Output = Result<(), SshError>>,
{
    match setup(host.to_string(), workspace.to_string()).await {
        Ok(()) => {
            set_offline(host, false);
            Ok(false)
        }
        Err(SshError::Offline) => {
            set_offline(host, true);
            Ok(true)
        }
        Err(SshError::Failed(reason)) => Err(reason),
    }
}

/// `make_workspace` with the full timeout, since the person is waiting on the form anyway.
pub async fn first_setup(host: String, workspace: String) -> Result<(), SshError> {
    make_workspace(&host, &workspace, TIMEOUT).await
}

pub async fn quick_setup(host: String, workspace: String) -> Result<(), SshError> {
    make_workspace(&host, &workspace, QUICK).await
}

/// Hosts that didn't answer the last time Grimoire tried them. Only this run of the app knows: a host
/// counts as online until a try fails.
static OFFLINE: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

static APP: OnceLock<(AppHandle, Arc<Db>)> = OnceLock::new();

pub fn is_offline(host: &str) -> bool {
    OFFLINE.lock().is_ok_and(|hosts| hosts.contains(host))
}

/// Records what the last try found, and tells the UI when that changed.
pub fn set_offline(host: &str, offline: bool) {
    let changed = OFFLINE.lock().is_ok_and(|mut hosts| if offline { hosts.insert(host.to_string()) } else { hosts.remove(host) });
    if changed {
        agents_changed();
    }
}

fn agents_changed() {
    if let Some((app, _)) = APP.get() {
        let _ = app.emit("agents_changed", ());
    }
}

/// What the person sees when a turn can't run because the familiar's Mac is offline.
pub fn offline_message(name: &str, host: &str, pending: bool) -> String {
    let next = if pending { "Grimoire will finish setting it up when it connects." } else { "Try again when it's back." };
    format!("{name} runs on {host}, and that Mac is offline. {next}")
}

/// A familiar as the UI lists it, with where its Mac stands.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    #[serde(flatten)]
    pub agent: Agent,
    /// Saved while its Mac was offline; Grimoire creates its workspace there once it connects.
    pub setup_pending: bool,
    /// Its Mac didn't answer the last time Grimoire tried.
    pub offline: bool,
}

pub fn with_status(db: &Db, agents: Vec<Agent>) -> Result<Vec<AgentStatus>, String> {
    let pending = db.pending_setups()?;
    Ok(agents
        .into_iter()
        .map(|agent| AgentStatus {
            setup_pending: pending.contains(&agent.id),
            offline: agent.host.as_deref().is_some_and(is_offline),
            agent,
        })
        .collect())
}

/// Gets a remote familiar ready for a turn. One saved while its Mac was offline, or whose Mac didn't
/// answer last time, gets a setup try first: success clears the pending flag and the turn goes on, and
/// failure returns what to tell the person instead. `setup` is `quick_setup` in the app.
pub async fn ready<F, Fut>(db: &Db, agent: &Agent, setup: F) -> Result<(), String>
where
    F: FnOnce(String, String) -> Fut,
    Fut: Future<Output = Result<(), SshError>>,
{
    let Some(host) = &agent.host else { return Ok(()) };
    let pending = db.pending_setups()?.contains(&agent.id);
    if !pending && !is_offline(host) {
        return Ok(());
    }
    match setup(host.clone(), agent.workspace.clone()).await {
        Ok(()) => {
            if pending {
                db.set_setup_pending(&agent.id, false)?;
                agents_changed();
            }
            set_offline(host, false);
            Ok(())
        }
        Err(SshError::Offline) => {
            set_offline(host, true);
            Err(offline_message(&agent.name, host, pending))
        }
        Err(SshError::Failed(reason)) => Err(reason),
    }
}

/// `ready` with a real ssh try.
async fn ready_now(db: &Db, agent: &Agent) -> Result<(), String> {
    ready(db, agent, quick_setup).await
}

/// Why a turn on another Mac couldn't start. ssh can end before it prints anything Grimoire reads, say
/// when the CLI's stdin closes first, so an unclear failure gets one quick try to see whether the Mac
/// answers at all.
pub async fn explain(agent: &Agent, reason: String) -> String {
    let Some(host) = &agent.host else { return reason };
    if offline_text(&reason) || quick_setup(host.clone(), agent.workspace.clone()).await == Err(SshError::Offline) {
        set_offline(host, true);
        return offline_message(&agent.name, host, false);
    }
    reason
}

/// Retries the familiars waiting on their Mac one at a time: pending setups, and familiars whose Mac
/// was offline. `only` limits it to one host. A host that stays offline is skipped for the rest of the
/// round, so each round costs at most one timeout per host.
async fn retry_waiting(db: &Db, only: Option<&str>) {
    let (Ok(agents), Ok(pending)) = (db.list_agents(), db.pending_setups()) else { return };
    let mut skip = BTreeSet::new();
    for agent in agents {
        let Some(host) = agent.host.as_deref().filter(|h| only.is_none_or(|only| only == *h)) else { continue };
        if skip.contains(host) || !(pending.contains(&agent.id) || is_offline(host)) {
            continue;
        }
        if ready_now(db, &agent).await.is_err() && is_offline(host) {
            skip.insert(host.to_string());
        }
    }
}

/// Lets status changes reach the UI, and starts the background retries.
pub fn install(app: AppHandle, db: Arc<Db>) {
    if APP.set((app, db.clone())).is_err() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(RETRY_EVERY).await;
            retry_waiting(&db, None).await;
        }
    });
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostCheck {
    pub reachable: bool,
    /// Where each CLI is on the other Mac, if it's on PATH there.
    pub codex: Option<String>,
    pub claude: Option<String>,
    pub error: Option<String>,
}

const CHECK_SCRIPT: &str = r#"printf 'codex=%s\n' "$(command -v codex)"; printf 'claude=%s\n' "$(command -v claude)""#;

/// Reads the CLI paths from the check script's output, skipping anything a profile printed.
pub fn parse_check(stdout: &str) -> (Option<String>, Option<String>) {
    let find = |key: &str| {
        stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(String::from)
    };
    (find("codex"), find("claude"))
}

#[tauri::command]
pub async fn check_host(host: String) -> Result<HostCheck, String> {
    let host = clean_host(Some(&host))?.ok_or("Write the other Mac's SSH destination first.")?;
    let unanswered = |error: String| Ok(HostCheck { reachable: false, codex: None, claude: None, error: Some(error) });
    let output = match run(&host, in_login_shell(CHECK_SCRIPT), TIMEOUT).await {
        Ok(output) => output,
        Err(SshError::Offline) => {
            set_offline(&host, true);
            return unanswered(format!("{host} did not answer within {} seconds.", TIMEOUT.as_secs()));
        }
        Err(SshError::Failed(error)) => return unanswered(error),
    };
    if !output.status.success() {
        if unreachable(output.status.code(), &String::from_utf8_lossy(&output.stderr)) {
            set_offline(&host, true);
        }
        return unanswered(failure(&host, &output));
    }
    // It answered, so finish setting up the familiars waiting on it.
    set_offline(&host, false);
    if let Some((_, db)) = APP.get() {
        let (db, waiting) = (db.clone(), host.clone());
        tauri::async_runtime::spawn(async move { retry_waiting(&db, Some(&waiting)).await });
    }
    let (codex, claude) = parse_check(&String::from_utf8_lossy(&output.stdout));
    Ok(HostCheck { reachable: true, codex, claude, error: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{now_ms, Access, Avatar, Provider};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn tells_an_offline_mac_from_other_ssh_failures() {
        assert!(unreachable(Some(255), ""), "ssh's own failure with no message");
        assert!(unreachable(Some(255), "ssh: connect to host studio.local port 22: Operation timed out\n"));
        assert!(unreachable(Some(255), "ssh: connect to host 10.0.0.4 port 22: No route to host"));
        assert!(unreachable(Some(255), "ssh: Could not resolve hostname studio.local: nodename nor servname provided"));
        assert!(unreachable(Some(255), "Welcome\nkex_exchange_identification: read: Connection reset by peer"));
        assert!(!unreachable(Some(255), "me@studio.local: Permission denied (publickey)."), "a refused key needs the person");
        assert!(!unreachable(Some(255), "Host key verification failed."));
        assert!(!unreachable(Some(1), "mkdir: /System/x: Operation not permitted"), "the Mac answered");
        assert!(!unreachable(None, ""), "killed by a signal");
        assert!(offline_text("Could not start\nssh: connect to host studio.local port 22: Connection refused"));
        assert!(!offline_text("zsh: command not found: claude"));
    }

    fn remote_agent(id: &str, host: &str) -> Agent {
        Agent {
            id: id.into(),
            name: "Ada".into(),
            provider: Provider::Claude,
            model: None,
            workspace: "~/Grimoire/ada".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            host: Some(host.into()),
            created_at: now_ms(),
        }
    }

    #[tokio::test]
    async fn an_offline_mac_still_gets_its_familiar() {
        let host = "create-test.local";
        let pending = setup_on_create(host, "~/w", |_, _| async { Err(SshError::Offline) }).await;
        assert_eq!(pending, Ok(true), "saved with setup pending");
        assert!(is_offline(host));
        let online = setup_on_create(host, "~/w", |_, _| async { Ok(()) }).await;
        assert_eq!(online, Ok(false));
        assert!(!is_offline(host));
        let bad = setup_on_create(host, "/System/w", |_, _| async { Err(SshError::Failed("Could not create the workspace.".into())) }).await;
        assert_eq!(bad, Err("Could not create the workspace.".into()), "other failures still stop it");
    }

    #[tokio::test]
    async fn the_first_turn_after_it_connects_finishes_setup() {
        let host = "ready-test.local";
        let db = Db::open_in_memory().unwrap();
        let agent = remote_agent("r", host);
        db.insert_agent(&agent).unwrap();
        db.set_setup_pending("r", true).unwrap();
        set_offline(host, true);

        let tries = AtomicUsize::new(0);
        let offline = |_: String, _: String| {
            tries.fetch_add(1, Ordering::SeqCst);
            async { Err(SshError::Offline) }
        };
        let refused = ready(&db, &agent, offline).await.unwrap_err();
        assert_eq!(refused, "Ada runs on ready-test.local, and that Mac is offline. Grimoire will finish setting it up when it connects.");
        assert!(db.pending_setups().unwrap().contains("r"), "still pending");

        let made = std::sync::Mutex::new(None);
        let online = |host: String, workspace: String| {
            *made.lock().unwrap() = Some((host, workspace));
            async { Ok(()) }
        };
        assert_eq!(ready(&db, &agent, online).await, Ok(()));
        assert_eq!(made.lock().unwrap().clone(), Some((host.to_string(), "~/Grimoire/ada".to_string())), "makes its workspace there");
        assert!(db.pending_setups().unwrap().is_empty(), "setup is done");
        assert!(!is_offline(host));

        // A familiar that's set up on a Mac that answered last time goes straight to its turn.
        assert_eq!(ready(&db, &agent, offline).await, Ok(()));
        assert_eq!(tries.load(Ordering::SeqCst), 1, "no ssh try");
        // Once its Mac stops answering, the person gets told to try again later.
        set_offline(host, true);
        assert_eq!(ready(&db, &agent, offline).await.unwrap_err(), offline_message("Ada", host, false));
        assert!(offline_message("Ada", host, false).ends_with("that Mac is offline. Try again when it's back."));
    }

    #[test]
    fn lists_where_each_familiar_stands() {
        let db = Db::open_in_memory().unwrap();
        let (pending, offline) = (remote_agent("p", "status-a.local"), remote_agent("o", "status-b.local"));
        db.insert_agent(&pending).unwrap();
        db.insert_agent(&offline).unwrap();
        db.set_setup_pending("p", true).unwrap();
        set_offline("status-b.local", true);
        let listed = with_status(&db, db.list_agents().unwrap()).unwrap();
        let states: Vec<(&str, bool, bool)> = listed.iter().map(|s| (s.agent.id.as_str(), s.setup_pending, s.offline)).collect();
        assert_eq!(states, [("p", true, false), ("o", false, true)]);
        let json = serde_json::to_value(&listed[0]).unwrap();
        assert_eq!((json["id"].as_str(), json["setupPending"].as_bool(), json["offline"].as_bool()), (Some("p"), Some(true), Some(false)));
    }

    #[test]
    fn accepts_ssh_destinations_only() {
        for good in ["studio.local", "me@studio.local", "build-box", "me@10.0.0.4", "fe80::1%en0", "me.name@host_1"] {
            assert_eq!(clean_host(Some(good)).unwrap().as_deref(), Some(good), "{good}");
        }
        assert_eq!(clean_host(Some("  me@mac\n")).unwrap().as_deref(), Some("me@mac"), "pasted whitespace is trimmed");
        assert_eq!(clean_host(None).unwrap(), None);
        assert_eq!(clean_host(Some("   ")).unwrap(), None);
        for bad in [
            "-oProxyCommand=sh",
            "me@-oProxyCommand=sh",
            "me@.hidden",
            "@host",
            "me@",
            "a@b@c",
            "me host",
            "ho\nst",
            "host;rm",
            "host$(id)",
            "host`id`",
            "host|x",
            "ho'st",
            "ho\"st",
            "host/x",
            "ssh://host",
            "host\0",
            "hôst",
        ] {
            assert!(clean_host(Some(bad)).is_err(), "{bad:?}");
        }
        assert!(clean_host(Some(&"a".repeat(MAX_HOST))).is_ok());
        assert!(clean_host(Some(&"a".repeat(MAX_HOST + 1))).is_err());
    }

    #[test]
    fn checks_remote_workspaces() {
        assert_eq!(clean_workspace(None, "Site Builder!", "abcdef123").unwrap(), "~/Grimoire/site-builder-abcdef");
        assert_eq!(clean_workspace(Some(" "), "a", "abcdef").unwrap(), "~/Grimoire/a-abcdef");
        assert_eq!(clean_workspace(Some("~/code/app/"), "a", "abcdef").unwrap(), "~/code/app");
        assert_eq!(clean_workspace(Some("/Users/me/My Site"), "a", "abcdef").unwrap(), "/Users/me/My Site");
        assert_eq!(clean_workspace(Some("~/..hidden/x"), "a", "abcdef").unwrap(), "~/..hidden/x");
        for bad in ["/", "//", "~", "~/", "~//", "relative/dir", "~other/dir", "/a/../b", "~/..", "/a/..", "/a\nb", "/a\0b", "/a\rb"] {
            assert!(clean_workspace(Some(bad), "a", "abcdef").is_err(), "{bad:?}");
        }
        assert!(clean_workspace(Some(&format!("/{}", "a".repeat(MAX_PATH))), "a", "abcdef").is_err());
    }

    #[test]
    fn quotes_words_for_a_posix_shell() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("a b\nc $HOME `id` \"x\" \\ ;|&"), "'a b\nc $HOME `id` \"x\" \\ ;|&'");
        assert_eq!(quote_path("~/My Site"), r#""$HOME"/'My Site'"#);
        assert_eq!(quote_path("/srv/it's"), r"'/srv/it'\''s'");
    }

    #[test]
    fn builds_the_ssh_command_line() {
        let args = ssh_args("me@studio.local", "echo hi".into());
        assert_eq!(
            args,
            ["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=30", "me@studio.local", "--", "echo hi"]
        );
        let script = launch_script("~/w", "codex", &["app-server".into()]);
        assert_eq!(script, r#"exec "$SHELL" -lc 'mkdir -p -- "$HOME"/'\''w'\'' && cd -- "$HOME"/'\''w'\'' && exec '\''codex'\'' '\''app-server'\'''"#);
    }

    /// Runs the remote command in a local `sh`, standing in for the remote shell and its login shell,
    /// and checks every argument arrives intact.
    #[test]
    fn arguments_survive_both_shells() {
        let root = std::env::temp_dir().join(format!("grimoire-remote-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("it's a $dir `x`");
        let tricky = [
            "plain",
            "",
            "two words",
            "it's",
            "'",
            "''",
            "line one\nline two",
            "$HOME and ${PATH}",
            "`id` $(id)",
            "\"double\"",
            "back\\slash",
            "; rm -rf / &",
            "--flag=value",
            "*",
            "~/x",
            "tab\there",
        ];
        let mut args: Vec<String> = vec!["-c".into(), "printf '%s\\0' \"$PWD\" \"$@\"".into(), "sh".into()];
        args.extend(tricky.iter().map(|s| s.to_string()));
        let script = launch_script(&workspace.to_string_lossy(), "sh", &args);
        let output = std::process::Command::new("/bin/sh").arg("-c").arg(&script).env("SHELL", "/bin/sh").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let words: Vec<String> = String::from_utf8(output.stdout).unwrap().split_terminator('\0').map(String::from).collect();
        let pwd = std::fs::canonicalize(&workspace).unwrap();
        assert_eq!(std::fs::canonicalize(&words[0]).unwrap(), pwd, "runs in the workspace it made");
        assert_eq!(&words[1..], tricky);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn home_relative_workspaces_expand_on_the_remote_side() {
        let home = std::env::temp_dir().join(format!("grimoire-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let script = launch_script("~/Grimoire/a b", "pwd", &[]);
        let output = std::process::Command::new("/bin/sh").arg("-c").arg(&script).env("SHELL", "/bin/sh").env("HOME", &home).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let pwd = String::from_utf8(output.stdout).unwrap();
        assert_eq!(std::fs::canonicalize(pwd.trim()).unwrap(), std::fs::canonicalize(home.join("Grimoire/a b")).unwrap());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn reads_which_clis_the_other_mac_has() {
        let out = "Welcome to the studio\ncodex=/opt/homebrew/bin/codex\nclaude=\n";
        assert_eq!(parse_check(out), (Some("/opt/homebrew/bin/codex".into()), None));
        assert_eq!(parse_check("codex=\nclaude=/Users/me/.local/bin/claude"), (None, Some("/Users/me/.local/bin/claude".into())));
        assert_eq!(parse_check(""), (None, None));
    }

    #[test]
    fn the_check_script_reports_both_clis() {
        let output = std::process::Command::new("/bin/sh").arg("-c").arg(in_login_shell(CHECK_SCRIPT)).env("SHELL", "/bin/sh").output().unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("codex=") && text.contains("claude="), "{text}");
    }
}
