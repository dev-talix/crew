use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "dock";
const MARGIN: f64 = 6.0;

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

pub fn show_main(app: &AppHandle, agent_id: Option<String>) -> Result<(), String> {
    let main = app.get_webview_window("main").ok_or("The Grimoire window is missing.")?;
    main.show().map_err(|e| e.to_string())?;
    let _ = main.unminimize();
    main.set_focus().map_err(|e| e.to_string())?;
    if let Some(agent_id) = agent_id {
        main.emit("open_agent", agent_id).map_err(|e| e.to_string())?;
    }
    Ok(())
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
    fn never_grows_past_the_screen() {
        let small = Area { x: 1512.0, y: 0.0, width: 400.0, height: 300.0 };
        let (x, y, w, h) = place(Edge::Left, small, 480.0, 600.0);
        assert_eq!((w, h), (388.0, 288.0));
        assert_eq!((x, y), (1518.0, 6.0));
    }
}
