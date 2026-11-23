//! The menu bar item: Grimoire's seal, the number of familiars working beside it, and a menu of
//! familiars with their state. Settings, General can hide it.

use crate::db::{Agent, Db};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Listener, Manager, State};

/// The settings key; "hidden" hides the item.
const KEY: &str = "menu_bar";
const ID: &str = "grimoire";
const OPEN: &str = "open";
const QUIT: &str = "quit";
const AGENT: &str = "agent:";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Status {
    Working,
    NewReply,
    Idle,
}

fn label(name: &str, status: Status) -> String {
    let state = match status {
        Status::Working => "Working",
        Status::NewReply => "New reply",
        Status::Idle => "Idle",
    };
    format!("{name} · {state}")
}

/// Shown beside the icon: how many familiars are working, or nothing.
fn title(working: usize) -> Option<String> {
    (working > 0).then(|| working.to_string())
}

fn tooltip(working: usize) -> String {
    match working {
        0 => "Grimoire".into(),
        1 => "Grimoire, 1 familiar working".into(),
        n => format!("Grimoire, {n} familiars working"),
    }
}

#[derive(Default)]
struct Seen {
    busy: HashSet<String>,
    unread: HashSet<String>,
    /// The familiar whose chat the main window shows.
    open: Option<String>,
}

impl Seen {
    fn status(&self, id: &str) -> Status {
        if self.busy.contains(id) {
            Status::Working
        } else if self.unread.contains(id) {
            Status::NewReply
        } else {
            Status::Idle
        }
    }

    /// A finished turn is new unless its chat is open in a window that's in front, like the sidebar.
    fn finished(&mut self, id: &str, main_in_front: bool) {
        self.busy.remove(id);
        if !(main_in_front && self.open.as_deref() == Some(id)) {
            self.unread.insert(id.to_string());
        }
    }
}

pub struct Tray {
    db: Arc<Db>,
    seen: Mutex<Seen>,
    icon: Mutex<Option<TrayIcon>>,
}

pub fn visible(db: &Db) -> bool {
    db.setting(KEY).ok().flatten().as_deref() != Some("hidden")
}

pub fn install(app: &AppHandle, db: Arc<Db>) {
    let show = visible(&db);
    app.manage(Tray { db, seen: Mutex::default(), icon: Mutex::default() });
    if show {
        if let Err(e) = set_visible(app, true) {
            eprintln!("The menu bar item is off: {e}");
        }
    }
    let handle = app.clone();
    app.listen_any("agents_changed", move |_| refresh(&handle));
}

fn set_visible(app: &AppHandle, show: bool) -> Result<(), String> {
    let tray = app.state::<Tray>();
    let mut icon = tray.icon.lock().map_err(|_| "menu bar state poisoned")?;
    match (show, icon.as_ref()) {
        (true, None) => {
            let built = TrayIconBuilder::with_id(ID)
                .icon(Image::from_bytes(icon_png(app)).map_err(|e| e.to_string())?)
                .icon_as_template(true)
                .tooltip("Grimoire")
                .menu(&Menu::new(app).map_err(|e| e.to_string())?)
                .on_menu_event(on_menu)
                .build(app)
                .map_err(|e| e.to_string())?;
            *icon = Some(built);
        }
        (false, Some(_)) => {
            // Dropping the last handle doesn't remove it; the app's tray list keeps one.
            if let Some(old) = icon.take() {
                let _ = old.set_visible(false);
                app.remove_tray_by_id(old.id());
            }
        }
        _ => {}
    }
    drop(icon);
    refresh(app);
    Ok(())
}

/// macOS draws the icon 18 points tall, so the 36 px one is for Retina screens.
fn icon_png(app: &AppHandle) -> &'static [u8] {
    let scale = app.primary_monitor().ok().flatten().map_or(2.0, |m| m.scale_factor());
    if scale > 1.0 {
        include_bytes!("../icons/tray@2x.png")
    } else {
        include_bytes!("../icons/tray.png")
    }
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref();
    let _ = match id {
        OPEN => crate::dock::show_main(app, None),
        QUIT => {
            app.exit(0);
            Ok(())
        }
        _ => match id.strip_prefix(AGENT) {
            Some(agent_id) => crate::dock::show_main(app, Some(agent_id.to_string())),
            None => Ok(()),
        },
    };
}

/// Rebuilds the menu and the count from the current familiars and their state.
fn refresh(app: &AppHandle) {
    let Some(tray) = app.try_state::<Tray>() else { return };
    let Some(icon) = tray.icon.lock().ok().and_then(|i| i.clone()) else { return };
    let agents = tray.db.list_agents().unwrap_or_default();
    let rows: Vec<(String, String)> = {
        let Ok(seen) = tray.seen.lock() else { return };
        agents.iter().map(|a: &Agent| (a.id.clone(), label(&a.name, seen.status(&a.id)))).collect()
    };
    let working = tray.seen.lock().map(|s| agents.iter().filter(|a| s.busy.contains(&a.id)).count()).unwrap_or(0);
    let _ = icon.set_title(title(working));
    let _ = icon.set_tooltip(Some(tooltip(working)));
    if let Ok(menu) = build_menu(app, &rows) {
        let _ = icon.set_menu(Some(menu));
    }
}

fn build_menu(app: &AppHandle, rows: &[(String, String)]) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for (id, text) in rows {
        menu.append(&MenuItem::with_id(app, format!("{AGENT}{id}"), text, true, None::<&str>)?)?;
    }
    if rows.is_empty() {
        menu.append(&MenuItem::new(app, "No familiars yet", false, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, OPEN, "Open Grimoire", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, QUIT, "Quit Grimoire", true, Some("CmdOrCtrl+Q"))?)?;
    Ok(menu)
}

/// Called whenever a familiar starts or stops working.
pub fn agent_status(app: &AppHandle, agent_id: &str, busy: bool) {
    let Some(tray) = app.try_state::<Tray>() else { return };
    let in_front = !crate::notify::in_background(app);
    if let Ok(mut seen) = tray.seen.lock() {
        if busy {
            seen.busy.insert(agent_id.to_string());
        } else {
            seen.finished(agent_id, in_front);
        }
    }
    refresh(app);
}

/// The main window came to the front: the chat it shows has been seen.
pub fn main_focused(app: &AppHandle) {
    let Some(tray) = app.try_state::<Tray>() else { return };
    let changed = tray.seen.lock().is_ok_and(|mut s| match s.open.clone() {
        Some(open) => s.unread.remove(&open),
        None => false,
    });
    if changed {
        refresh(app);
    }
}

#[tauri::command]
pub fn set_open_agent(app: AppHandle, tray: State<Tray>, agent_id: Option<String>) {
    let changed = tray.seen.lock().is_ok_and(|mut s| {
        s.open = agent_id.clone();
        agent_id.is_some_and(|id| s.unread.remove(&id))
    });
    if changed {
        refresh(&app);
    }
}

#[tauri::command]
pub fn menu_bar_visible(tray: State<Tray>) -> bool {
    visible(&tray.db)
}

#[tauri::command]
pub fn set_menu_bar_visible(app: AppHandle, tray: State<Tray>, visible: bool) -> Result<bool, String> {
    tray.db.set_setting(KEY, if visible { "shown" } else { "hidden" })?;
    set_visible(&app, visible)?;
    Ok(visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_count() {
        assert_eq!(label("Scribe", Status::Working), "Scribe · Working");
        assert_eq!(label("Scribe", Status::NewReply), "Scribe · New reply");
        assert_eq!(title(0), None);
        assert_eq!(title(2).as_deref(), Some("2"));
        assert_eq!(tooltip(1), "Grimoire, 1 familiar working");
        assert_eq!(tooltip(3), "Grimoire, 3 familiars working");
    }

    #[test]
    fn a_reply_is_new_unless_its_chat_is_open_in_front() {
        let mut seen = Seen { open: Some("a1".into()), ..Seen::default() };
        seen.busy.insert("a1".into());
        assert_eq!(seen.status("a1"), Status::Working);
        seen.finished("a1", true);
        assert_eq!(seen.status("a1"), Status::Idle);
        seen.finished("a1", false);
        assert_eq!(seen.status("a1"), Status::NewReply, "the window was in the background");
        seen.finished("a2", true);
        assert_eq!(seen.status("a2"), Status::NewReply, "another familiar's chat was open");
    }

    #[test]
    fn hidden_only_when_turned_off() {
        let db = Db::open_in_memory().unwrap();
        assert!(visible(&db));
        db.set_setting(KEY, "hidden").unwrap();
        assert!(!visible(&db));
    }
}
