use crate::db::Db;
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "dock";
const MARGIN: f64 = 6.0;
/// The familiars left out of the dock, as a JSON list of ids. New familiars show until you hide them.
const HIDDEN: &str = "dock_hidden";

pub fn hidden(db: &Db) -> Vec<String> {
    db.setting(HIDDEN).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// Saves which familiars the dock leaves out, once each and only ones that still exist.
pub fn save_hidden(db: &Db, ids: &[String]) -> Result<Vec<String>, String> {
    let agents = db.list_agents()?;
    let mut kept: Vec<String> = Vec::new();
    for id in ids {
        if agents.iter().any(|a| &a.id == id) && !kept.contains(id) {
            kept.push(id.clone());
        }
    }
    db.set_setting(HIDDEN, &serde_json::to_string(&kept).map_err(|e| e.to_string())?)?;
    Ok(kept)
}

#[tauri::command]
pub fn dock_hidden(state: State<AppState>) -> Vec<String> {
    hidden(&state.db)
}

#[tauri::command]
pub fn set_dock_hidden(app: AppHandle, state: State<AppState>, ids: Vec<String>) -> Result<Vec<String>, String> {
    let saved = save_hidden(&state.db, &ids)?;
    let _ = app.emit("dock_hidden", &saved);
    Ok(saved)
}

/// Takes the dock off the screen while it has no familiars to show. Its next resize brings it back.
#[tauri::command]
pub fn dock_hide(app: AppHandle) -> Result<(), String> {
    match app.get_webview_window(LABEL) {
        Some(window) => window.hide().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
}

impl Edge {
    pub fn parse(value: &str) -> Option<Edge> {
        match value {
            "left" => Some(Edge::Left),
            "right" => Some(Edge::Right),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn place(edge: Edge, area: Area, width: f64, height: f64) -> (f64, f64, f64, f64) {
    let width = width.min(area.width - MARGIN * 2.0).max(1.0);
    let height = height.min(area.height - MARGIN * 2.0).max(1.0);
    let x = match edge {
        Edge::Left => area.x + MARGIN,
        Edge::Right => area.x + area.width - width - MARGIN,
    };
    let y = area.y + (area.height - height) / 2.0;
    (x, y, width, height)
}

#[derive(Default)]
pub struct Dock {
    edge: Mutex<Option<Edge>>,
}

impl Dock {
    pub fn edge(&self) -> Option<Edge> {
        self.edge.lock().ok().and_then(|e| *e)
    }

    pub fn set_edge(&self, app: &AppHandle, edge: Option<Edge>) -> Result<(), String> {
        if let Ok(mut current) = self.edge.lock() {
            *current = edge;
        }
        let existing = app.get_webview_window(LABEL);
        match (edge, existing) {
            (None, Some(window)) => window.close().map_err(|e| e.to_string()),
            (None, None) => Ok(()),
            (Some(edge), Some(window)) => window.emit("dock_edge", edge).map_err(|e| e.to_string()),
            (Some(_), None) => WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
                .title("Grimoire dock")
                .inner_size(76.0, 160.0)
                .transparent(true)
                .decorations(false)
                .shadow(false)
                .resizable(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .visible_on_all_workspaces(true)
                .focused(false)
                .accept_first_mouse(true)
                // Let the chat take dropped files itself.
                .disable_drag_drop_handler()
                .visible(false)
                .build()
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }

    pub fn resize(&self, app: &AppHandle, width: f64, height: f64, focus: bool) -> Result<(), String> {
        let edge = self.edge().ok_or("The dock is off.")?;
        let window = app.get_webview_window(LABEL).ok_or("The dock window is not open.")?;
        let area = work_area(app, &window)?;
        let (x, y, width, height) = place(edge, area, width, height);
        window.set_size(LogicalSize::new(width, height)).map_err(|e| e.to_string())?;
        window.set_position(LogicalPosition::new(x, y)).map_err(|e| e.to_string())?;
        if !window.is_visible().unwrap_or(false) {
            window.show().map_err(|e| e.to_string())?;
        }
        if focus {
            window.set_focus().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn work_area(app: &AppHandle, dock: &WebviewWindow) -> Result<Area, String> {
    let main = app.get_webview_window("main").filter(|w| w.is_visible().unwrap_or(false));
    let monitor = main
        .and_then(|w| w.current_monitor().ok().flatten())
        .or_else(|| dock.primary_monitor().ok().flatten())
        .ok_or("No display found for the dock.")?;
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    Ok(Area {
        x: f64::from(work.position.x) / scale,
        y: f64::from(work.position.y) / scale,
        width: f64::from(work.size.width) / scale,
        height: f64::from(work.size.height) / scale,
    })
}

/// Whether the main window's page is listening for `open_agent`, and the familiar to open once it is.
struct MainPage {
    ready: bool,
    pending: Option<String>,
}

static MAIN_PAGE: Mutex<MainPage> = Mutex::new(MainPage { ready: false, pending: None });

pub fn show_main(app: &AppHandle, agent_id: Option<String>) -> Result<(), String> {
    let main = match app.get_webview_window("main") {
        Some(main) => main,
        // Gone after a close with nothing to keep it, while a hidden dock or Quick Ask kept Grimoire running.
        None => {
            if let Ok(mut page) = MAIN_PAGE.lock() {
                page.ready = false;
            }
            let config = app.config().app.windows.iter().find(|w| w.label == "main").ok_or("The Grimoire window is missing.")?;
            WebviewWindowBuilder::from_config(app, config).and_then(|b| b.build()).map_err(|e| e.to_string())?
        }
    };
    crate::background::window_shown(app);
    main.show().map_err(|e| e.to_string())?;
    let _ = main.unminimize();
    main.set_focus().map_err(|e| e.to_string())?;
    restore_dock(app);
    if let Some(agent_id) = agent_id {
        // A page still loading would miss the event, so it picks the familiar up from `main_ready`.
        let mut page = MAIN_PAGE.lock().map_err(|_| "window state poisoned")?;
        if page.ready {
            main.emit("open_agent", agent_id).map_err(|e| e.to_string())?;
        } else {
            page.pending = Some(agent_id);
        }
    }
    Ok(())
}

/// Puts a saved dock back on the screen edge, the first time the main window shows.
fn restore_dock(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else { return };
    if state.dock.edge().is_some() {
        return;
    }
    let saved = state.db.setting(crate::DOCK_EDGE).ok().flatten().and_then(|e| Edge::parse(&e));
    if saved.is_some() {
        if let Err(e) = state.dock.set_edge(app, saved) {
            eprintln!("Couldn't bring the dock back: {e}");
        }
    }
}

/// The main window's page is listening for `open_agent`. Returns the familiar it should open, if a
/// click came in while it was loading.
#[tauri::command]
pub fn main_ready() -> Option<String> {
    let mut page = MAIN_PAGE.lock().ok()?;
    page.ready = true;
    page.pending.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Area = Area { x: 0.0, y: 25.0, width: 1512.0, height: 920.0 };

    #[test]
    fn hugs_the_chosen_edge_and_centers_vertically() {
        assert_eq!(place(Edge::Left, SCREEN, 76.0, 200.0), (6.0, 385.0, 76.0, 200.0));
        assert_eq!(place(Edge::Right, SCREEN, 76.0, 200.0), (1430.0, 385.0, 76.0, 200.0));
    }

    #[test]
    fn keeps_the_rail_in_place_when_the_panel_opens() {
        let (closed_x, closed_y, closed_w, closed_h) = place(Edge::Right, SCREEN, 76.0, 200.0);
        let (open_x, open_y, open_w, open_h) = place(Edge::Right, SCREEN, 480.0, 600.0);
        assert_eq!(closed_x + closed_w, open_x + open_w);
        assert_eq!(closed_y + closed_h / 2.0, open_y + open_h / 2.0);
    }

    #[test]
    fn remembers_hidden_familiars_once_and_only_real_ones() {
        let db = Db::open_in_memory().unwrap();
        for id in ["a", "b"] {
            let agent = crate::db::Agent {
                id: id.into(),
                name: id.into(),
                provider: crate::db::Provider::Claude,
                model: None,
                workspace: format!("/tmp/{id}"),
                access: crate::db::Access::Workspace,
                session_id: None,
                avatar: crate::db::Avatar::for_seed(id),
                effort: None,
                active_model: None,
                coven_id: None,
                host: None,
                created_at: 0,
            };
            db.insert_agent(&agent).unwrap();
        }
        assert!(hidden(&db).is_empty(), "every familiar shows at first");
        let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(save_hidden(&db, &ids(&["b", "gone", "b", "a"])).unwrap(), ids(&["b", "a"]));
        assert_eq!(hidden(&db), ids(&["b", "a"]));
        assert!(save_hidden(&db, &[]).unwrap().is_empty());
    }

    #[test]
    fn never_grows_past_the_screen() {
        let small = Area { x: 1512.0, y: 0.0, width: 400.0, height: 300.0 };
        let (x, y, w, h) = place(Edge::Left, small, 480.0, 600.0);
        assert_eq!((w, h), (388.0, 288.0));
        assert_eq!((x, y), (1518.0, 6.0));
    }
}
