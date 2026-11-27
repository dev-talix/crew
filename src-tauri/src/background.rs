//! Background mode, for an always-on Mac: Grimoire runs with no window and no Dock icon, can start
//! at login, and can keep the Mac awake, so rituals, triggers, the phone companion, and linked Macs
//! keep working. Opening Grimoire again shows the window.

use crate::db::Db;
use serde::{Deserialize, Serialize};
use std::io::Write;
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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundSettings {
    pub run_in_background: bool,
    pub start_at_login: bool,
    pub keep_awake: bool,
    /// Keep awake is on and `caffeinate` is running. False with `keep_awake` on means it couldn't start.
    pub awake_holding: bool,
    /// This launch came from the login item, so background mode is on until Grimoire quits.
    pub launched_in_background: bool,
}

/// One switch in Settings, General, Background.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Switch {
    RunInBackground,
    StartAtLogin,
    KeepAwake,
}

pub fn launched_in_background(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().skip(1).any(|a| a == ARG)
}

fn setting_on(db: &Db, key: &str) -> bool {
    db.setting(key).ok().flatten().as_deref() == Some("on")
}

/// Sets up background mode for this launch. `launched` is whether it started with `--background`;
/// `run` already set the Dock icon for that before the app finished launching.
pub fn install(app: &AppHandle, db: Arc<Db>, launched: bool) {
    let background = Background { db, launched, awake: Mutex::default() };
    if setting_on(&background.db, AWAKE_KEY) {
        if let Err(e) = background.set_awake(true) {
            eprintln!("Couldn't keep this Mac awake: {e}");
        }
    }
    app.manage(background);
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

    fn holding(&self) -> bool {
        self.awake.lock().is_ok_and(|mut a| a.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None))))
    }

    /// The saved switches and what's live. A keep-awake that died gets one restart here.
    fn settings(&self) -> BackgroundSettings {
        let keep_awake = setting_on(&self.db, AWAKE_KEY);
        if keep_awake && !self.holding() {
            if let Err(e) = self.set_awake(true) {
                eprintln!("Couldn't keep this Mac awake: {e}");
            }
        }
        BackgroundSettings {
            run_in_background: setting_on(&self.db, KEY),
            start_at_login: home().is_some_and(|h| agent_path(&h).exists()),
            keep_awake,
            awake_holding: keep_awake && self.holding(),
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

fn agent_dir(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
}

fn agent_path(home: &Path) -> PathBuf {
    agent_dir(home).join(format!("{LABEL}.plist"))
}

/// This app's bundle identifier, when it runs from an app bundle. A bare binary, as under
/// `tauri dev`, has none.
#[cfg(target_os = "macos")]
fn bundle_id() -> Option<String> {
    let bundle = objc2_foundation::NSBundle::mainBundle();
    if !bundle.bundlePath().to_string().ends_with(".app") {
        return None;
    }
    bundle.bundleIdentifier().map(|id| id.to_string())
}

#[cfg(not(target_os = "macos"))]
fn bundle_id() -> Option<String> {
    None
}

/// What the login item runs. It opens the app by bundle identifier, so moving or renaming
/// Grimoire.app keeps it working, and the job ends at once, so unloading it can't take a running
/// Grimoire down with it.
fn program(bundle_id: &str) -> Vec<String> {
    ["/usr/bin/open", "-g", "-b", bundle_id, "--args", ARG].map(String::from).to_vec()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A LaunchAgent that runs once when you log in, and only in a GUI session.
fn plist(label: &str, program: &[String]) -> String {
    let args: String = program.iter().map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a))).collect();
    let label = xml_escape(label);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
\t<key>Label</key>
\t<string>{label}</string>
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

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Writes the login item through a temporary file in the same folder, so launchd never reads half a
/// file. A symlinked folder or file could point anywhere, so it refuses those.
fn write_agent(home: &Path, bundle_id: &str) -> Result<(), String> {
    let dir = agent_dir(home);
    if is_symlink(&dir) {
        return Err(format!("{} is a symlink, so Grimoire won't write a login item there.", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't make {}: {e}", dir.display()))?;
    let path = agent_path(home);
    if is_symlink(&path) {
        return Err(format!("{} is a symlink, so Grimoire won't replace it.", path.display()));
    }
    let temp = dir.join(format!(".{LABEL}.{}.tmp", uuid::Uuid::new_v4()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut f| f.write_all(plist(LABEL, &program(bundle_id)).as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&temp, &path));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("Couldn't write {}: {e}", path.display())
    })
}

fn remove_agent(home: &Path) -> Result<(), String> {
    match std::fs::remove_file(agent_path(home)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Couldn't remove the login item: {e}")),
        _ => Ok(()),
    }
}

fn service(label: &str) -> String {
    format!("gui/{}/{label}", unsafe { libc::getuid() })
}

fn launchctl(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("/bin/launchctl").args(args).stdin(Stdio::null()).output().map_err(|e| format!("launchctl didn't run: {e}"))
}

fn loaded(label: &str) -> bool {
    launchctl(&["print", &service(label)]).is_ok_and(|o| o.status.success())
}

/// Unloads a login item from this GUI session. One that isn't loaded counts as unloaded; one that
/// stays loaded is an error.
fn unload(label: &str) -> Result<(), String> {
    let out = launchctl(&["bootout", &service(label)])?;
    if out.status.success() || !loaded(label) {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(format!("Couldn't unload the login item: {detail}"))
}

/// Turns the login item on or off. Turning it on only writes the file, which macOS loads at the next
/// login; loading it now would run it, and `open` would bring this Grimoire's window up. Turning it
/// off unloads it from this session first, and keeps the file if that fails.
fn set_start_at_login(on: bool) -> Result<(), String> {
    let home = home().ok_or("HOME is not set.")?;
    if on {
        let id = bundle_id().ok_or("Start at login works only from Grimoire.app, not a development build.")?;
        write_agent(&home, &id)
    } else {
        unload(LABEL)?;
        remove_agent(&home)
    }
}

#[tauri::command]
pub fn background_settings(background: State<Background>) -> BackgroundSettings {
    background.settings()
}

/// Changes one switch and returns all of them as they now are.
#[tauri::command]
pub fn set_background_switch(background: State<Background>, switch: Switch, on: bool) -> Result<BackgroundSettings, String> {
    let value = if on { "on" } else { "off" };
    match switch {
        Switch::RunInBackground => background.db.set_setting(KEY, value)?,
        Switch::StartAtLogin => set_start_at_login(on)?,
        Switch::KeepAwake => {
            background.db.set_setting(AWAKE_KEY, value)?;
            background.set_awake(on)?;
        }
    }
    Ok(background.settings())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn temp_home() -> PathBuf {
        std::env::temp_dir().join(format!("grimoire-bg-{}", uuid::Uuid::new_v4()))
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
    fn the_login_item_opens_the_app_by_bundle_id() {
        assert_eq!(program("dev.talix.grimoire"), args(&["/usr/bin/open", "-g", "-b", "dev.talix.grimoire", "--args", "--background"]));
    }

    #[test]
    fn the_login_item_runs_once_in_a_gui_session() {
        let text = plist(LABEL, &args(&["/bin/sh", "-c", "echo a & b > \"c\""]));
        assert!(text.contains("<key>Label</key>\n\t<string>dev.talix.grimoire.background</string>"));
        assert!(text.contains("\t\t<string>/bin/sh</string>\n\t\t<string>-c</string>\n\t\t<string>echo a &amp; b &gt; &quot;c&quot;</string>\n\t</array>"));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(text.contains("<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>"));
        assert!(!text.contains("KeepAlive"), "quitting Grimoire must not relaunch it");
    }

    #[test]
    fn writes_and_removes_the_login_item_under_home() {
        let home = temp_home();
        let path = agent_path(&home);
        assert!(path.ends_with("Library/LaunchAgents/dev.talix.grimoire.background.plist"));
        write_agent(&home, "dev.talix.grimoire").unwrap();
        write_agent(&home, "dev.talix.grimoire").unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("<string>-b</string>\n\t\t<string>dev.talix.grimoire</string>"));
        let left: Vec<_> = std::fs::read_dir(agent_dir(&home)).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 1, "no temporary files left: {left:?}");
        if Path::new("/usr/bin/plutil").exists() {
            let lint = Command::new("/usr/bin/plutil").arg("-lint").arg(&path).output().unwrap();
            assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
        }
        remove_agent(&home).unwrap();
        assert!(!path.exists());
        remove_agent(&home).unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_login_items() {
        let home = temp_home();
        let elsewhere = home.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(home.join("Library")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, agent_dir(&home)).unwrap();
        assert!(write_agent(&home, "dev.talix.grimoire").unwrap_err().contains("symlink"));
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);

        std::fs::remove_file(agent_dir(&home)).unwrap();
        std::fs::create_dir_all(agent_dir(&home)).unwrap();
        let target = elsewhere.join("target.plist");
        std::fs::write(&target, "mine").unwrap();
        std::os::unix::fs::symlink(&target, agent_path(&home)).unwrap();
        assert!(write_agent(&home, "dev.talix.grimoire").unwrap_err().contains("symlink"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Unloads a test login item even when an assertion fails.
    struct Loaded(String);

    impl Drop for Loaded {
        fn drop(&mut self) {
            let _ = launchctl(&["bootout", &service(&self.0)]);
        }
    }

    /// Loads a login item into this Mac's GUI session, so it stays out of CI. It has its own label
    /// and only touches a file in a temp folder.
    #[test]
    #[ignore]
    fn launchd_runs_and_unloads_a_login_item() {
        let home = temp_home();
        let dir = agent_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        let label = format!("dev.talix.grimoire.test.{}", uuid::Uuid::new_v4().simple());
        let marker = home.join("ran");
        let path = dir.join(format!("{label}.plist"));
        std::fs::write(&path, plist(&label, &args(&["/usr/bin/touch", &marker.display().to_string()]))).unwrap();
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let guard = Loaded(label.clone());
        let out = launchctl(&["bootstrap", &domain, &path.display().to_string()]).unwrap();
        assert!(out.status.success(), "launchd refused the plist: {}", String::from_utf8_lossy(&out.stderr));
        assert!(loaded(&label));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(marker.exists(), "the login item never ran");
        unload(&label).unwrap();
        assert!(!loaded(&label));
        unload(&label).expect("unloading one that isn't loaded is fine");
        drop(guard);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn keeps_the_system_awake_for_this_process_only() {
        assert_eq!(caffeinate_args(4242), ["-i", "-w", "4242"]);
    }

    #[test]
    fn keep_awake_reports_whether_caffeinate_holds() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let background = Background { db: db.clone(), launched: false, awake: Mutex::default() };
        assert!(!background.holding());
        // A caffeinate that already exited holds nothing.
        *background.awake.lock().unwrap() = Some(Command::new("/usr/bin/true").spawn().unwrap());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!background.holding());
        background.set_awake(false).unwrap();
        assert!(background.awake.lock().unwrap().is_none());
    }

    #[test]
    fn switches_parse_from_the_window() {
        assert_eq!(serde_json::from_str::<Switch>("\"startAtLogin\"").unwrap(), Switch::StartAtLogin);
        assert!(serde_json::from_str::<Switch>("\"everything\"").is_err());
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
