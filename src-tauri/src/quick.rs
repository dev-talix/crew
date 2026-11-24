use crate::db::Db;
use crate::dock::Area;
use crate::AppState;
use std::sync::Mutex;
use tauri::{AppHandle, LogicalPosition, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub const LABEL: &str = "quick";
pub const DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+Space";
const SHORTCUT: &str = "quick_ask_shortcut";
const ENABLED: &str = "quick_ask_enabled";
/// The familiar Quick Ask last talked to, so it reopens on the same one.
const AGENT: &str = "quick_ask_agent";
const WIDTH: f64 = 420.0;
const HEIGHT: f64 = 520.0;

/// Reads a shortcut the way the plugin will register it. On macOS the plugin registers keys with
/// Carbon's RegisterEventHotKey, which needs no permission, but media and volume keys go through a
/// CGEventTap that needs Accessibility, so they're refused here. A shortcut also needs Command,
/// Control, or Option, or it would take a key away from typing in every app.
pub fn parse(text: &str) -> Result<Shortcut, String> {
    let text = text.trim();
    let shortcut: Shortcut = text.parse().map_err(|_| format!("Grimoire can't read the shortcut \"{text}\"."))?;
    let key = shortcut.key.to_string();
    if key.starts_with("Media") || key.starts_with("Audio") {
        return Err("Media and volume keys can't open Quick Ask.".into());
    }
    if !shortcut.mods.intersects(tauri_plugin_global_shortcut::Modifiers::SUPER | tauri_plugin_global_shortcut::Modifiers::CONTROL | tauri_plugin_global_shortcut::Modifiers::ALT) {
        return Err("The shortcut needs Command, Control, or Option.".into());
    }
    Ok(shortcut)
}

pub fn shortcut_text(db: &Db) -> String {
    db.setting(SHORTCUT).ok().flatten().unwrap_or_else(|| DEFAULT_SHORTCUT.into())
}

pub fn enabled(db: &Db) -> bool {
    db.setting(ENABLED).ok().flatten().as_deref() != Some("off")
}

/// Centered across the screen, with the panel's middle on the upper third line, kept on screen.
pub fn place(area: Area, width: f64, height: f64) -> (f64, f64) {
    let x = area.x + (area.width - width) / 2.0;
    let y = (area.y + area.height / 3.0 - height / 2.0).clamp(area.y, (area.y + area.height - height).max(area.y));
    (x, y)
}

#[derive(Default)]
pub struct Quick {
    current: Mutex<Option<Shortcut>>,
    /// Why the saved shortcut didn't register at launch, shown in Settings until a save works.
    error: Mutex<Option<String>>,
}

impl Quick {
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    /// Registers the saved shortcut at launch. A failure leaves Quick Ask off and says why in Settings.
    pub fn install(&self, app: &AppHandle, db: &Db) {
        if !enabled(db) {
            return;
        }
        let text = shortcut_text(db);
        let result = parse(&text).and_then(|shortcut| register(app, shortcut, &text).map(|_| shortcut));
        match result {
            Ok(shortcut) => *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(shortcut),
            Err(e) => {
                eprintln!("Quick Ask is off: {e}");
                *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
            }
        }
    }

    /// Swaps in the new shortcut, or turns it off, then saves. If the new one won't register,
    /// the old one goes back and nothing is saved.
    pub fn apply(&self, app: &AppHandle, db: &Db, on: bool, text: &str) -> Result<(), String> {
        let text = text.trim();
        let parsed = parse(text)?;
        let next = on.then_some(parsed);
        let mut current = self.current.lock().map_err(|e| e.to_string())?;
        if *current != next {
            // Take the new shortcut before letting go of the old one, so a refused shortcut leaves
            // the working one in place.
            if let Some(new) = next {
                register(app, new, text)?;
            }
            if let Some(old) = *current {
                if let Err(e) = app.global_shortcut().unregister(old) {
                    if let Some(new) = next {
                        let _ = app.global_shortcut().unregister(new);
                    }
                    return Err(e.to_string());
                }
            }
            *current = next;
        }
        db.set_setting(SHORTCUT, text)?;
        db.set_setting(ENABLED, if on { "on" } else { "off" })?;
        *self.error.lock().map_err(|e| e.to_string())? = None;
        Ok(())
    }
}

fn register(app: &AppHandle, shortcut: Shortcut, text: &str) -> Result<(), String> {
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _, event| {
            if event.state == ShortcutState::Pressed {
                if let Err(e) = toggle(app) {
                    eprintln!("Quick Ask couldn't open: {e}");
                }
            }
        })
        .map_err(|e| format!("Grimoire couldn't use {text}. Another app may already use it. ({e})"))
}

/// Shows the panel on the screen with the pointer, or hides it when it's already in front.
fn toggle(app: &AppHandle) -> Result<(), String> {
    let window = match app.get_webview_window(LABEL) {
        Some(window) => window,
        None => WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
            .title("Quick Ask")
            .inner_size(WIDTH, HEIGHT)
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible_on_all_workspaces(true)
            // Let the chat take dropped files itself.
            .disable_drag_drop_handler()
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?,
    };
    if window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false) {
        return window.hide().map_err(|e| e.to_string());
    }
    if !window.is_visible().unwrap_or(false) {
        let area = pointer_screen(app)?;
        let (x, y) = place(area, WIDTH, HEIGHT);
        window.set_position(LogicalPosition::new(x, y)).map_err(|e| e.to_string())?;
        window.show().map_err(|e| e.to_string())?;
    }
    window.set_focus().map_err(|e| e.to_string())
}

fn pointer_screen(app: &AppHandle) -> Result<Area, String> {
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten())
        .ok_or("No display found for Quick Ask.")?;
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    Ok(Area {
        x: f64::from(work.position.x) / scale,
        y: f64::from(work.position.y) / scale,
        width: f64::from(work.size.width) / scale,
        height: f64::from(work.size.height) / scale,
    })
}

#[tauri::command]
pub fn quick_hide(app: AppHandle) -> Result<(), String> {
    match app.get_webview_window(LABEL) {
        Some(window) => window.hide().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

#[tauri::command]
pub fn quick_agent(state: State<AppState>) -> Result<Option<String>, String> {
    state.db.setting(AGENT)
}

#[tauri::command]
pub fn set_quick_agent(state: State<AppState>, agent_id: String) -> Result<(), String> {
    state.db.set_setting(AGENT, &agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_shortcuts_the_plugin_can_register_without_permissions() {
        let shortcut = parse(" CommandOrControl+Shift+Space ").unwrap();
        assert_eq!(parse("cmd+shift+space").unwrap(), shortcut, "names and case don't matter");
        assert!(parse("Alt+KeyK").is_ok());
        assert!(parse("Space").is_err(), "a bare key would steal typing");
        assert!(parse("Shift+A").is_err(), "so would Shift alone");
        assert!(parse("Cmd+MediaPlayPause").is_err(), "media keys need Accessibility");
        assert!(parse("Cmd+Shift+Nope").is_err());
        assert!(parse("Cmd+Shift+").is_err());
    }

    #[test]
    fn opens_in_the_upper_third_and_stays_on_screen() {
        let screen = Area { x: 0.0, y: 0.0, width: 1512.0, height: 900.0 };
        assert_eq!(place(screen, 420.0, 520.0), (546.0, 40.0));
        let short = Area { x: 0.0, y: 25.0, width: 1280.0, height: 600.0 };
        assert_eq!(place(short, 420.0, 520.0), (430.0, 25.0));
        let tall = Area { x: 1512.0, y: 0.0, width: 1080.0, height: 1920.0 };
        assert_eq!(place(tall, 420.0, 520.0), (1842.0, 380.0));
    }

    #[test]
    fn defaults_to_on_with_the_standard_shortcut() {
        let db = Db::open_in_memory().unwrap();
        assert!(enabled(&db));
        assert_eq!(shortcut_text(&db), DEFAULT_SHORTCUT);
        db.set_setting(ENABLED, "off").unwrap();
        assert!(!enabled(&db));
    }
}
