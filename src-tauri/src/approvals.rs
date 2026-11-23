//! Questions a familiar's action puts to the person: "Researcher wants to use the github.com
//! password". The request waits here until someone answers in a Grimoire window or it times out.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

pub const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Once,
    Always,
    Deny,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: String,
    pub agent_id: String,
    /// "vault", "command", "file", or "familiar".
    pub kind: &'static str,
    pub title: String,
    pub detail: String,
    /// Whether "Always allow" makes sense for this request.
    pub can_remember: bool,
}

#[derive(Default)]
pub struct Approvals {
    pending: Mutex<HashMap<String, (Request, oneshot::Sender<Decision>)>>,
}

impl Approvals {
    /// Asks every window and waits for an answer. No answer in time counts as a denial.
    pub async fn ask(&self, app: &AppHandle, request: Request) -> Decision {
        let (tx, rx) = oneshot::channel();
        let id = request.id.clone();
        self.pending.lock().unwrap().insert(id.clone(), (request.clone(), tx));
        let _ = app.emit("approval_requested", &request);
        crate::notify::approval_requested(app, &request);
        if let Some(main) = app.get_webview_window("main") {
            let _ = main.request_user_attention(Some(tauri::UserAttentionType::Informational));
        }
        let decision = tokio::time::timeout(TIMEOUT, rx).await.ok().and_then(Result::ok).unwrap_or(Decision::Deny);
        self.pending.lock().unwrap().remove(&id);
        let _ = app.emit("approval_closed", &id);
        crate::notify::approval_closed(&request);
        decision
    }

    pub fn answer(&self, id: &str, decision: Decision) -> Result<(), String> {
        let (_, tx) = self.pending.lock().unwrap().remove(id).ok_or("That request has already been answered or timed out.")?;
        let _ = tx.send(decision);
        Ok(())
    }

    /// Requests still waiting, for a window that opens after they were sent.
    pub fn waiting(&self) -> Vec<Request> {
        self.pending.lock().unwrap().values().map(|(request, _)| request.clone()).collect()
    }
}
