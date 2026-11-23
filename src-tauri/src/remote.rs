//! Familiars on another Mac. Grimoire runs the provider CLI there through the system `ssh`, with the
//! person's own keys and agent, and talks to it over the same stdio streams it uses locally.
//!
//! The host and the remote workspace never pass through a shell on this Mac: `ssh` gets them as
//! separate arguments. The remote command does pass through the remote shell, so every piece of it
//! is single-quoted with `quote`.

use serde::Serialize;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const MAX_HOST: usize = 255;
const MAX_PATH: usize = 1024;
const TIMEOUT: Duration = Duration::from_secs(20);

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

async fn run(host: &str, remote_command: String) -> Result<std::process::Output, String> {
    let mut cmd = ssh(host, remote_command);
    cmd.stdin(Stdio::null());
    tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("{host} did not answer within {} seconds.", TIMEOUT.as_secs()))?
        .map_err(|e| crate::providers::spawn_error("ssh", e))
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
pub async fn make_workspace(host: &str, workspace: &str) -> Result<(), String> {
    let output = run(host, format!("mkdir -p -- {}", quote_path(workspace))).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("Could not create the workspace on the other Mac. {}", failure(host, &output)))
    }
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
    let output = match run(&host, in_login_shell(CHECK_SCRIPT)).await {
        Ok(output) => output,
        Err(error) => return Ok(HostCheck { reachable: false, codex: None, claude: None, error: Some(error) }),
    };
    if !output.status.success() {
        return Ok(HostCheck { reachable: false, codex: None, claude: None, error: Some(failure(&host, &output)) });
    }
    let (codex, claude) = parse_check(&String::from_utf8_lossy(&output.stdout));
    Ok(HostCheck { reachable: true, codex, claude, error: None })
}

#[cfg(test)]
mod tests {
    use super::*;

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
