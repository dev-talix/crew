//! Background mode, for an always-on Mac: Grimoire runs with no window and no Dock icon, can start
//! at login, and can keep the Mac awake, so rituals, triggers, the phone companion, and linked Macs
//! keep working. Opening Grimoire again shows the window.

use crate::db::Db;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};

/// The launch argument the login item passes. It turns background mode on for that launch.
pub const ARG: &str = "--background";
/// The settings keys; "on" turns each on.
const KEY: &str = "background_mode";
const AWAKE_KEY: &str = "keep_awake";
/// The LaunchAgent's label and file name in ~/Library/LaunchAgents.
const LABEL: &str = "dev.talix.grimoire.background";

pub struct Background {
    db: Arc<Db>,
    /// Started with `--background`.
    launched: bool,
    /// The `caffeinate` that holds the power assertion, while it runs.
    awake: Mutex<Option<Child>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundSettings {
    pub run_in_background: bool,
    pub start_at_login: bool,
    pub keep_awake: bool,
    /// This launch came from the login item, so background mode is on until Grimoire quits.
    #[serde(default)]
    pub launched_in_background: bool,
}

pub fn launched_in_background(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().skip(1).any(|a| a == ARG)
}

fn setting_on(db: &Db, key: &str) -> bool {
    db.setting(key).ok().flatten().as_deref() == Some("on")
}

/// Sets up background mode for this launch. Returns true when the main window should stay hidden.
pub fn install(app: &AppHandle, db: Arc<Db>) -> bool {
    let launched = launched_in_background(std::env::args());
    let background = Background { db, launched, awake: Mutex::default() };
    if setting_on(&background.db, AWAKE_KEY) {
        if let Err(e) = background.set_awake(true) {
            eprintln!("Couldn't keep this Mac awake: {e}");
        }
    }
    app.manage(background);
    if launched {
        set_dock_icon(app, false);
    }
    launched
}

/// Background mode is on: closing the window hides it, and the Dock icon goes with it.
pub fn on(app: &AppHandle) -> bool {
    app.try_state::<Background>().is_some_and(|b| b.launched || setting_on(&b.db, KEY))
}

/// The main window went away in background mode, so the Dock icon goes too.
pub fn window_hidden(app: &AppHandle) {
    if on(app) {
        set_dock_icon(app, false);
    }
}

/// The main window is coming up. It needs the Dock icon and the app menu, or Copy and Paste stop working.
pub fn window_shown(app: &AppHandle) {
    set_dock_icon(app, true);
}

fn set_dock_icon(app: &AppHandle, show: bool) {
    #[cfg(target_os = "macos")]
    {
        let policy = if show { tauri::ActivationPolicy::Regular } else { tauri::ActivationPolicy::Accessory };
        if let Err(e) = app.set_activation_policy(policy) {
            eprintln!("Couldn't change the Dock icon: {e}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, show);
}

impl Background {
    /// Starts or stops `caffeinate -i -w <pid>`. It stops on its own when Grimoire quits, and leaves display sleep alone.
    fn set_awake(&self, on: bool) -> Result<(), String> {
        let mut awake = self.awake.lock().map_err(|_| "keep-awake state poisoned")?;
        // A caffeinate that died, say from Activity Monitor, holds nothing.
        if awake.as_mut().is_some_and(|c| !matches!(c.try_wait(), Ok(None))) {
            *awake = None;
        }
        match (on, awake.take()) {
            (true, None) => {
                let child = Command::new("/usr/bin/caffeinate")
                    .args(caffeinate_args(std::process::id()))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| format!("caffeinate didn't start: {e}"))?;
                *awake = Some(child);
            }
            (true, Some(child)) => *awake = Some(child),
            (false, Some(mut child)) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            (false, None) => {}
        }
        Ok(())
    }

    fn settings(&self) -> BackgroundSettings {
        BackgroundSettings {
            run_in_background: setting_on(&self.db, KEY),
            start_at_login: home().is_some_and(|h| agent_path(&h).exists()),
            keep_awake: setting_on(&self.db, AWAKE_KEY),
            launched_in_background: self.launched,
        }
    }
}

/// `-i` keeps the system from idle sleep; `-w` ends the assertion when Grimoire's process exits.
fn caffeinate_args(pid: u32) -> [String; 3] {
    ["-i".into(), "-w".into(), pid.to_string()]
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute())
}

fn agent_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

/// What the login item runs. From an app bundle it goes through `open`, so the job ends at once and
/// removing it later can't take a running Grimoire down with it. A bare binary, as in development,
/// runs directly.
fn program(exe: &Path) -> Vec<String> {
    let bundle = exe.ancestors().nth(3).filter(|b| b.extension().is_some_and(|e| e == "app") && exe.parent().is_some_and(|p| p.ends_with("Contents/MacOS")));
    match bundle {
        Some(app) => vec!["/usr/bin/open".into(), "-g".into(), "-a".into(), app.display().to_string(), "--args".into(), ARG.into()],
        None => vec![exe.display().to_string(), ARG.into()],
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The LaunchAgent: runs once when you log in, and only in a GUI session.
fn plist(program: &[String]) -> String {
    let args: String = program.iter().map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a))).collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
\t<key>Label</key>
\t<string>{LABEL}</string>
\t<key>ProgramArguments</key>
\t<array>
{args}\t</array>
\t<key>RunAtLoad</key>
\t<true/>
\t<key>LimitLoadToSessionType</key>
\t<string>Aqua</string>
</dict>
</plist>
"
    )
}

fn write_agent(home: &Path, exe: &Path) -> Result<(), String> {
    let path = agent_path(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, plist(&program(exe))).map_err(|e| format!("Couldn't write {}: {e}", path.display()))
}

fn remove_agent(home: &Path) -> Result<(), String> {
    match std::fs::remove_file(agent_path(home)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Couldn't remove the login item: {e}")),
        _ => Ok(()),
    }
}

/// Turns the login item on or off. Turning it on only writes the file, which macOS loads at the next
/// login; loading it now would run it, and `open` would bring this Grimoire's window up. Turning it
/// off unloads it from this session too.
fn set_start_at_login(on: bool) -> Result<(), String> {
    let home = home().ok_or("HOME is not set.")?;
    if on {
        let exe = std::env::current_exe().map_err(|e| format!("Couldn't find Grimoire's app: {e}"))?;
        write_agent(&home, &exe)
    } else {
        // Not loaded is fine; it was written this session, or never loaded.
        let _ = launchctl(&["bootout", &service()]);
        remove_agent(&home)
    }
}

/// The login item in this user's GUI session, as launchctl names it.
fn service() -> String {
    format!("gui/{}/{LABEL}", unsafe { libc::getuid() })
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl").args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

#[tauri::command]
pub fn background_settings(background: State<Background>) -> BackgroundSettings {
    background.settings()
}

#[tauri::command]
pub fn save_background_settings(background: State<Background>, settings: BackgroundSettings) -> Result<BackgroundSettings, String> {
    let before = background.settings();
    if settings.run_in_background != before.run_in_background {
        background.db.set_setting(KEY, if settings.run_in_background { "on" } else { "off" })?;
    }
    if settings.start_at_login != before.start_at_login {
        set_start_at_login(settings.start_at_login)?;
    }
    if settings.keep_awake != before.keep_awake {
        background.set_awake(settings.keep_awake)?;
        background.db.set_setting(AWAKE_KEY, if settings.keep_awake { "on" } else { "off" })?;
    }
    Ok(background.settings())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_the_background_argument_starts_hidden() {
        assert!(launched_in_background(args(&["/Applications/Grimoire.app/Contents/MacOS/grimoire", "--background"])));
        assert!(launched_in_background(args(&["grimoire", "-psn_0_12345", "--background"])));
        assert!(!launched_in_background(args(&["grimoire"])));
        assert!(!launched_in_background(args(&["grimoire", "--backgrounds"])));
        assert!(!launched_in_background(args(&["--background"])), "the program's own path doesn't count");
    }

    #[test]
    fn a_bundle_launches_through_open_and_a_bare_binary_directly() {
        assert_eq!(
            program(Path::new("/Applications/Grimoire.app/Contents/MacOS/grimoire")),
            args(&["/usr/bin/open", "-g", "-a", "/Applications/Grimoire.app", "--args", "--background"])
        );
        assert_eq!(program(Path::new("/tmp/build/grimoire")), args(&["/tmp/build/grimoire", "--background"]));
        assert_eq!(program(Path::new("/tmp/Grimoire.app/grimoire")), args(&["/tmp/Grimoire.app/grimoire", "--background"]));
    }

    #[test]
    fn the_login_item_runs_once_in_a_gui_session() {
        let text = plist(&args(&["/usr/bin/open", "-a", "/Users/me/Apps & Tools/Grimoire.app", "--args", "--background"]));
        assert!(text.contains("<key>Label</key>\n\t<string>dev.talix.grimoire.background</string>"));
        assert!(text.contains("\t\t<string>/usr/bin/open</string>\n\t\t<string>-a</string>\n\t\t<string>/Users/me/Apps &amp; Tools/Grimoire.app</string>\n"));
        assert!(text.contains("<string>--background</string>\n\t</array>"));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(text.contains("<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>"));
        assert!(!text.contains("KeepAlive"), "quitting Grimoire must not relaunch it");
    }

    #[test]
    fn writes_and_removes_the_login_item_under_home() {
        let home = std::env::temp_dir().join(format!("grimoire-bg-{}", uuid::Uuid::new_v4()));
        let path = agent_path(&home);
        assert!(path.ends_with("Library/LaunchAgents/dev.talix.grimoire.background.plist"));
        write_agent(&home, Path::new("/Applications/Grimoire.app/Contents/MacOS/grimoire")).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("<string>/Applications/Grimoire.app</string>"));
        if Path::new("/usr/bin/plutil").exists() {
            let lint = Command::new("/usr/bin/plutil").arg("-lint").arg(&path).output().unwrap();
            assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
        }
        remove_agent(&home).unwrap();
        assert!(!path.exists());
        remove_agent(&home).unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Loads a login item into this Mac's GUI session, so it stays out of CI. It runs `/usr/bin/true`.
    #[test]
    #[ignore]
    fn launchd_loads_and_unloads_the_login_item() {
        let home = std::env::temp_dir().join(format!("grimoire-bg-{}", uuid::Uuid::new_v4()));
        write_agent(&home, Path::new("/usr/bin/true")).unwrap();
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        assert!(launchctl(&["bootstrap", &domain, &agent_path(&home).display().to_string()]), "launchd refused the plist");
        assert!(launchctl(&["print", &service()]));
        assert!(launchctl(&["bootout", &service()]));
        assert!(!launchctl(&["print", &service()]));
        remove_agent(&home).unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn keeps_the_system_awake_for_this_process_only() {
        assert_eq!(caffeinate_args(4242), ["-i", "-w", "4242"]);
    }

    #[test]
    fn settings_default_off() {
        let db = Db::open_in_memory().unwrap();
        assert!(!setting_on(&db, KEY));
        assert!(!setting_on(&db, AWAKE_KEY));
        db.set_setting(KEY, "on").unwrap();
        assert!(setting_on(&db, KEY));
    }
}
