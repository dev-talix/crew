//! The first-run wizard's backend: whether to show it, installing the Codex and Claude CLIs, and
//! opening Terminal to sign in. Every command it runs is fixed here; the window only names a provider.

use crate::db::Provider;
use crate::providers::{cli_path, command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

const DONE: &str = "onboarding_done";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// One install at a time: two package managers writing the same prefix can break both installs.
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// The official installer for each CLI, run by /bin/bash with the PATH Grimoire uses for the CLIs.
/// Claude ships a native installer that needs nothing else. Codex comes from Homebrew, or npm when
/// Homebrew is missing. `pipefail` makes a failed download fail the install.
pub fn install_script(provider: &Provider) -> &'static str {
    match provider {
        Provider::Claude => "set -o pipefail; curl -fsSL https://claude.ai/install.sh | bash",
        Provider::Codex => {
            "if command -v brew >/dev/null 2>&1; then brew install --cask codex; \
             elif command -v npm >/dev/null 2>&1; then npm install -g @openai/codex; \
             else echo 'Install Homebrew from https://brew.sh or Node.js from https://nodejs.org, then try again.' >&2; exit 127; fi"
        }
    }
}

/// The command that signs the CLI in. Both open a browser and wait for it.
pub fn sign_in_command(provider: &Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude auth login",
        Provider::Codex => "codex login",
    }
}

/// A Terminal script that runs the sign-in with `path`, the PATH Grimoire found the CLI on, and says
/// whether it worked.
pub fn sign_in_script(provider: &Provider, path: &str) -> String {
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    format!(
        "#!/bin/bash\nexport PATH={quoted}\n{}\nstatus=$?\necho\nif [ $status -eq 0 ]; then echo 'Signed in. Close this window and go back to Grimoire.'; else echo \"Sign-in failed (exit $status). Close this window and try again from Grimoire.\"; fi\nexit $status\n",
        sign_in_command(provider)
    )
}

/// The last few lines of an installer's output, for the error message.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[tauri::command]
pub(crate) fn onboarding_needed(state: State<'_, crate::AppState>) -> Result<bool, String> {
    if state.db.setting(DONE)?.is_some() {
        return Ok(false);
    }
    Ok(state.db.list_agents()?.is_empty())
}

#[tauri::command]
pub(crate) fn finish_onboarding(state: State<'_, crate::AppState>) -> Result<(), String> {
    state.db.set_setting(DONE, "1")
}

#[tauri::command]
pub(crate) async fn install_provider(provider: Provider) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("Another install is still running. Wait for it to finish.".into());
    }
    let result = install(&provider).await;
    INSTALLING.store(false, Ordering::SeqCst);
    result
}

async fn install(provider: &Provider) -> Result<(), String> {
    let home = crate::home()?;
    let mut cmd = command("/bin/bash", &home.to_string_lossy());
    // Its own process group, so a timeout stops the package manager's children too.
    cmd.args(["-c", install_script(provider)]).stdin(std::process::Stdio::null()).process_group(0);
    let child = cmd.spawn().map_err(|e| format!("Could not start the installer: {e}"))?;
    let group = child.id();
    let output = match tokio::time::timeout(INSTALL_TIMEOUT, child.wait_with_output()).await {
        Ok(output) => output.map_err(|e| format!("The installer stopped unexpectedly: {e}"))?,
        Err(_) => {
            if let Some(pid) = group {
                // SAFETY: killpg only sends a signal to the group this install started.
                unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
            }
            return Err("The install took more than 10 minutes, so Grimoire stopped it.".into());
        }
    };
    if output.status.success() {
        return Ok(());
    }
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    Err(format!("The install failed:\n{}", tail(&text, 6)))
}

#[tauri::command]
pub(crate) async fn open_sign_in(app: AppHandle, provider: Provider) -> Result<(), String> {
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = match provider {
        Provider::Claude => "sign-in-claude.command",
        Provider::Codex => "sign-in-codex.command",
    };
    let path = dir.join(name);
    std::fs::write(&path, sign_in_script(&provider, &cli_path())).map_err(|e| format!("Could not write the sign-in script: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let status = tokio::process::Command::new("/usr/bin/open")
        .args(["-a", "Terminal"])
        .arg(&path)
        .status()
        .await
        .map_err(|e| format!("Could not open Terminal: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Could not open Terminal.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_only_the_official_installers() {
        assert_eq!(install_script(&Provider::Claude), "set -o pipefail; curl -fsSL https://claude.ai/install.sh | bash");
        let codex = install_script(&Provider::Codex);
        assert!(codex.contains("brew install --cask codex") && codex.contains("npm install -g @openai/codex"));
        assert!(codex.contains("exit 127"), "says what to install when neither package manager is there");
    }

    #[test]
    fn signs_in_on_the_path_grimoire_found_the_cli_on() {
        let script = sign_in_script(&Provider::Claude, "/Users/me/.local/bin:/usr/bin");
        assert!(script.starts_with("#!/bin/bash\nexport PATH='/Users/me/.local/bin:/usr/bin'\nclaude auth login\n"));
        assert!(script.ends_with("exit $status\n"));
        assert!(sign_in_script(&Provider::Codex, "/a'b").contains("export PATH='/a'\\''b'\ncodex login\n"));
    }

    #[test]
    fn a_failed_download_fails_the_install() {
        // The same pipeline with curl replaced by a failing command.
        let script = install_script(&Provider::Claude).replace("curl -fsSL https://claude.ai/install.sh", "false");
        let status = std::process::Command::new("/bin/bash").args(["-c", &script]).status().unwrap();
        assert!(!status.success());
    }

    #[test]
    fn keeps_the_end_of_the_output() {
        assert_eq!(tail("a\n\nb\nc  \nd\n", 2), "c\nd");
        assert_eq!(tail("only", 6), "only");
        assert_eq!(tail("", 6), "");
    }
}
