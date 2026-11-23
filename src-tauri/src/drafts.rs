//! Draft-first outbound: a familiar never sends email, Slack messages, or posts itself. It calls
//! draft_message, the draft shows up as a card, and the person sends it with their own Mail or Slack.
//! Grimoire never sends anything. When the person marks a draft sent or discards it, the familiar
//! gets a short message saying so, with any edits they made.

use crate::db::{now_ms, Agent, Db};
use crate::AppState;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;

pub const TOOL: &str = "draft_message";
pub const CHANGED: &str = "drafts_changed";
const BODY_LIMIT: usize = 10_000;
const SUBJECT_LIMIT: usize = 200;
const NOTE_LIMIT: usize = 500;
const MAX_RECIPIENTS: usize = 20;
const MAX_PENDING: i64 = 20;
const RECENT: i64 = 30;
/// The only link a Slack draft may open: the app itself, since a channel link needs workspace ids.
pub const SLACK_LINK: &str = "slack://open";

/// Schema version 15: drafts, deleted with their familiar.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE drafts (
           id TEXT PRIMARY KEY,
           agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
           channel TEXT NOT NULL,
           recipients TEXT NOT NULL,
           subject TEXT,
           body TEXT NOT NULL,
           note TEXT,
           state TEXT NOT NULL DEFAULT 'draft',
           created_at INTEGER NOT NULL,
           closed_at INTEGER
         );
         CREATE INDEX drafts_by_state ON drafts(state, created_at);
         PRAGMA user_version = 19;
         COMMIT;",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Email,
    Slack,
    Post,
}

impl Channel {
    fn parse(text: &str) -> Result<Channel, String> {
        match text {
            "email" => Ok(Channel::Email),
            "slack" => Ok(Channel::Slack),
            "post" => Ok(Channel::Post),
            other => Err(format!("Unknown channel \"{other}\". Use email, slack, or post.")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Channel::Email => "email",
            Channel::Slack => "slack",
            Channel::Post => "post",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Channel::Email => "email",
            Channel::Slack => "Slack message",
            Channel::Post => "post",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftState {
    Draft,
    Sent,
    Discarded,
}

impl DraftState {
    fn as_str(self) -> &'static str {
        match self {
            DraftState::Draft => "draft",
            DraftState::Sent => "sent",
            DraftState::Discarded => "discarded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: String,
    pub agent_id: String,
    pub channel: Channel,
    /// Email addresses, Slack #channels or @handles, or where a post goes.
    pub to: Vec<String>,
    pub subject: Option<String>,
    pub body: String,
    /// Why the familiar wrote it.
    pub note: Option<String>,
    pub state: DraftState,
    pub created_at: i64,
    pub closed_at: Option<i64>,
}

/// The parts of a draft the person can change before sending.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Edits {
    pub to: Vec<String>,
    pub subject: Option<String>,
    pub body: String,
}

fn row_to_draft(row: &Row) -> rusqlite::Result<Draft> {
    let text = |i: usize| -> rusqlite::Result<String> { row.get(i) };
    let bad = |i: usize, e: String| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, e.into());
    let channel = Channel::parse(&text(2)?).map_err(|e| bad(2, e))?;
    let to: Vec<String> = serde_json::from_str(&text(3)?).map_err(|e| bad(3, e.to_string()))?;
    let state = match text(7)?.as_str() {
        "sent" => DraftState::Sent,
        "discarded" => DraftState::Discarded,
        _ => DraftState::Draft,
    };
    Ok(Draft {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        channel,
        to,
        subject: row.get(4)?,
        body: row.get(5)?,
        note: row.get(6)?,
        state,
        created_at: row.get(8)?,
        closed_at: row.get(9)?,
    })
}

const COLUMNS: &str = "id, agent_id, channel, recipients, subject, body, note, state, created_at, closed_at";

fn valid_email(address: &str) -> bool {
    // Plain addresses only: anything that could add a header or a second recipient to a mailto: link is out.
    let banned = |c: char| c.is_whitespace() || c.is_control() || ",;:<>\"()[]\\?&#%/".contains(c);
    let Some((local, domain)) = address.rsplit_once('@') else { return false };
    address.len() <= 254
        && !address.chars().any(banned)
        && !local.is_empty()
        && !local.contains('@')
        && domain.contains('.')
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-'])
        && !domain.contains("..")
}

fn valid_slack(target: &str) -> bool {
    let name = target.strip_prefix(['#', '@']).unwrap_or("");
    (1..=80).contains(&name.chars().count()) && name.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c))
}

fn single_line(text: &str, limit: usize) -> bool {
    text.chars().count() <= limit && !text.chars().any(char::is_control)
}

/// Checks a draft's fields for its channel and returns them trimmed.
fn clean(channel: Channel, to: Vec<String>, subject: Option<String>, body: &str) -> Result<(Vec<String>, Option<String>, String), String> {
    let mut list: Vec<String> = to.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    let mut seen = std::collections::HashSet::new();
    list.retain(|t| seen.insert(t.clone()));
    if list.len() > MAX_RECIPIENTS {
        return Err(format!("Keep it to {MAX_RECIPIENTS} recipients."));
    }
    match channel {
        Channel::Email => {
            if list.is_empty() {
                return Err("Give at least one email address in to.".into());
            }
            if let Some(bad) = list.iter().find(|a| !valid_email(a)) {
                return Err(format!("\"{bad}\" isn't a plain email address, like name@example.com."));
            }
        }
        Channel::Slack => {
            if list.is_empty() {
                return Err("Give at least one Slack #channel or @handle in to.".into());
            }
            if let Some(bad) = list.iter().find(|t| !valid_slack(t)) {
                return Err(format!("\"{bad}\" isn't a Slack #channel or @handle."));
            }
        }
        Channel::Post => {
            if let Some(bad) = list.iter().find(|t| !single_line(t, 80)) {
                return Err(format!("\"{bad}\" is too long. Name where the post goes in up to 80 characters, like LinkedIn."));
            }
        }
    }
    let subject = subject.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match (&subject, channel) {
        (Some(_), Channel::Slack | Channel::Post) => return Err("Only an email has a subject. Put it in the body instead.".into()),
        (Some(s), Channel::Email) if !single_line(s, SUBJECT_LIMIT) => {
            return Err(format!("Keep the subject to one line of up to {SUBJECT_LIMIT} characters."))
        }
        _ => {}
    }
    let body = body.trim();
    if body.is_empty() {
        return Err("The body is empty.".into());
    }
    if body.chars().count() > BODY_LIMIT {
        return Err(format!("Keep the body to {BODY_LIMIT} characters."));
    }
    Ok((list, subject, body.to_string()))
}

/// `to` as a list, or one string with commas or semicolons between recipients.
fn recipients_arg(value: Option<&Value>) -> Result<Vec<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(text.split([',', ';']).map(String::from).collect()),
        Some(Value::Array(items)) => items.iter().map(|v| v.as_str().map(String::from).ok_or("Each recipient in to must be a string.".to_string())).collect(),
        Some(_) => Err("Give to as a list of strings.".into()),
    }
}

/// Saves a draft from the familiar `caller`. The familiar comes from the tool connection, never an argument.
pub fn create(db: &Db, caller: &str, arguments: &Value) -> Result<Draft, String> {
    let agent: Agent = db.get_agent(caller)?.ok_or("Only a familiar can draft messages.")?;
    let text = |name: &str| arguments.get(name).and_then(Value::as_str).map(String::from);
    let channel = Channel::parse(&text("channel").ok_or("Give the channel: email, slack, or post.")?)?;
    let (to, subject, body) = clean(channel, recipients_arg(arguments.get("to"))?, text("subject"), &text("body").unwrap_or_default())?;
    let note = text("note").map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if note.as_ref().is_some_and(|n| n.chars().count() > NOTE_LIMIT) {
        return Err(format!("Keep the note to {NOTE_LIMIT} characters."));
    }
    let draft = Draft {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent.id,
        channel,
        to,
        subject,
        body,
        note,
        state: DraftState::Draft,
        created_at: now_ms(),
        closed_at: None,
    };
    let recipients = serde_json::to_string(&draft.to).map_err(|e| e.to_string())?;
    db.with(|c| {
        let pending: i64 = c.query_row("SELECT COUNT(*) FROM drafts WHERE agent_id = ?1 AND state = 'draft'", params![draft.agent_id], |r| r.get(0))?;
        if pending >= MAX_PENDING {
            return Ok(false);
        }
        c.execute(
            &format!("INSERT INTO drafts ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'draft', ?8, NULL)"),
            params![draft.id, draft.agent_id, channel.as_str(), recipients, draft.subject, draft.body, draft.note, draft.created_at],
        )?;
        Ok(true)
    })?
    .then_some(())
    .ok_or(format!("You already have {MAX_PENDING} drafts waiting for the person. Wait until they send or discard some."))?;
    Ok(draft)
}

pub fn get(db: &Db, id: &str) -> Result<Option<Draft>, String> {
    db.with(|c| c.query_row(&format!("SELECT {COLUMNS} FROM drafts WHERE id = ?1"), params![id], row_to_draft).optional())
}

/// Every draft still waiting, newest first, then the most recent sent or discarded ones.
pub fn list(db: &Db) -> Result<Vec<Draft>, String> {
    db.with(|c| {
        let mut pending = c.prepare_cached(&format!("SELECT {COLUMNS} FROM drafts WHERE state = 'draft' ORDER BY created_at DESC"))?;
        let mut drafts = pending.query_map([], row_to_draft)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut recent = c.prepare_cached(&format!("SELECT {COLUMNS} FROM drafts WHERE state != 'draft' ORDER BY closed_at DESC LIMIT ?1"))?;
        drafts.extend(recent.query_map(params![RECENT], row_to_draft)?.collect::<rusqlite::Result<Vec<_>>>()?);
        Ok(drafts)
    })
}

fn what(draft: &Draft) -> String {
    let noun = draft.channel.noun();
    match (draft.channel, draft.to.is_empty()) {
        (Channel::Post, true) => format!("{noun} draft"),
        (Channel::Post, false) => format!("{noun} draft for {}", draft.to.join(", ")),
        _ => format!("{noun} draft to {}", draft.to.join(", ")),
    }
}

/// What the familiar hears once the person acts. `after` is the draft as it was closed.
fn outcome_text(before: &Draft, after: &Draft) -> String {
    if after.state == DraftState::Discarded {
        return format!("The person discarded your {}. Don't send it some other way.", what(before));
    }
    let mut text = format!("The person marked your {} as sent.", what(before));
    let mut changes = Vec::new();
    if after.to != before.to {
        changes.push(format!("To: {}", after.to.join(", ")));
    }
    if after.subject != before.subject {
        changes.push(format!("Subject: {}", after.subject.as_deref().unwrap_or("(none)")));
    }
    if after.body != before.body {
        changes.push(format!("Body:\n{}", after.body));
    }
    if !changes.is_empty() {
        text.push_str(" They edited it first. What went out:\n");
        text.push_str(&changes.join("\n"));
    }
    text
}

/// The source label on the message the familiar gets.
fn source(state: DraftState) -> &'static str {
    match state {
        DraftState::Sent => "Draft sent",
        _ => "Draft discarded",
    }
}

/// Marks a waiting draft sent, with the person's edits, or discarded. Returns the closed draft and the
/// message for the familiar.
pub fn close(db: &Db, id: &str, state: DraftState, edits: Option<Edits>) -> Result<(Draft, String), String> {
    let before = get(db, id)?.ok_or("That draft no longer exists.")?;
    if before.state != DraftState::Draft || state == DraftState::Draft {
        return Err("That draft was already sent or discarded.".into());
    }
    let mut after = before.clone();
    if let (DraftState::Sent, Some(edits)) = (state, edits) {
        (after.to, after.subject, after.body) = clean(before.channel, edits.to, edits.subject, &edits.body)?;
    }
    after.state = state;
    after.closed_at = Some(now_ms());
    let recipients = serde_json::to_string(&after.to).map_err(|e| e.to_string())?;
    let changed = db.with(|c| {
        c.execute(
            "UPDATE drafts SET state = ?2, closed_at = ?3, recipients = ?4, subject = ?5, body = ?6 WHERE id = ?1 AND state = 'draft'",
            params![id, state.as_str(), after.closed_at, recipients, after.subject, after.body],
        )
    })?;
    if changed == 0 {
        return Err("That draft was already sent or discarded.".into());
    }
    let text = outcome_text(&before, &after);
    Ok((after, text))
}

/// Whether a draft may open `url`: a mailto: link for an email, the Slack app for a Slack message.
fn link_allowed(channel: Channel, url: &str) -> bool {
    match channel {
        Channel::Email => url.starts_with("mailto:") && !url.chars().any(char::is_control),
        Channel::Slack => url == SLACK_LINK,
        Channel::Post => false,
    }
}

/// The chat-start instruction, for a familiar that has the Grimoire tools.
pub fn session_prompt(agent: &Agent) -> Option<String> {
    prompt(crate::tools::available_for(agent))
}

fn prompt(tools: bool) -> Option<String> {
    tools.then(|| {
        "Outbound messages: never send email, Slack messages, or posts to people yourself. That means no sendmail, mail, or \
         osascript sending from Mail, no curl to Slack webhooks or chat.postMessage, and no gh commands that post comments. \
         Call draft_message instead. The person reviews the draft, sends it themselves, and Grimoire tells you when they \
         send or discard it."
            .to_string()
    })
}

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": TOOL,
        "description": "Draft an email, Slack message, or post for the person to send. Grimoire never sends anything: the person sees the draft, can edit it, and sends it with their own Mail or Slack. Use this for every outbound message to people, never sendmail, osascript, Slack webhooks, or gh comments. Returns at once; you get a message later when they send or discard it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "channel": { "type": "string", "enum": ["email", "slack", "post"] },
                "to": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "email: addresses like name@example.com. slack: #channel or @handle. post: where it goes, like LinkedIn (optional)."
                },
                "subject": { "type": "string", "description": "Email only." },
                "body": { "type": "string", "description": "Plain text, up to 10,000 characters." },
                "note": { "type": "string", "description": "Optional: why you wrote it, in a sentence, for the person." }
            },
            "required": ["channel", "body"]
        }
    })]
}

/// Runs draft_message for `caller`: saves the draft and lets the person know.
pub fn run(app: &AppHandle, db: &Db, caller: &str, arguments: &Value) -> Result<String, String> {
    let draft = create(db, caller, arguments)?;
    let _ = app.emit(CHANGED, ());
    crate::notify::draft_ready(app, &draft.agent_id, &what(&draft), &draft.body);
    Ok("Drafted. The person will review and send it.".into())
}

#[tauri::command]
pub fn list_drafts(state: State<AppState>) -> Result<Vec<Draft>, String> {
    list(&state.db)
}

#[tauri::command]
pub fn close_draft(app: AppHandle, state: State<AppState>, id: String, sent: bool, edits: Option<Edits>) -> Result<Draft, String> {
    let outcome = if sent { DraftState::Sent } else { DraftState::Discarded };
    let (draft, text) = close(&state.db, &id, outcome, edits)?;
    let _ = app.emit(CHANGED, ());
    // The familiar may be gone or busy; the draft is closed either way, and a busy familiar gets it next.
    let _ = state.runtime.send(&draft.agent_id, text, Some(source(outcome)));
    Ok(draft)
}

/// Opens a waiting draft's mailto: link, built by the window with the person's edits, or the Slack app.
#[tauri::command]
pub fn open_draft_link(app: AppHandle, state: State<AppState>, id: String, url: String) -> Result<(), String> {
    let draft = get(&state.db, &id)?.ok_or("That draft no longer exists.")?;
    if draft.state != DraftState::Draft || !link_allowed(draft.channel, &url) {
        return Err("Grimoire can't open that link for this draft.".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Avatar, Provider};

    fn setup() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&Agent {
            id: "scribe".into(),
            name: "Scribe".into(),
            provider: Provider::Claude,
            model: None,
            workspace: "/tmp/scribe".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed("scribe"),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: now_ms(),
        })
        .unwrap();
        db
    }

    #[test]
    fn validates_recipients_per_channel() {
        let db = setup();
        let email = |to: Value| create(&db, "scribe", &json!({ "channel": "email", "to": to, "subject": "Hi", "body": "Hello" }));
        assert_eq!(email(json!("a@example.com; b+tag@mail.example.org")).unwrap().to, ["a@example.com", "b+tag@mail.example.org"]);
        for bad in ["nobody", "a@b", "a@example.com?bcc=x@evil.com", "a b@example.com", "a@.com", "<a@example.com>", "a@example.com&cc=x@y.z"] {
            assert!(email(json!([bad])).is_err(), "{bad} should be refused");
        }
        assert!(email(json!([])).unwrap_err().contains("at least one"));

        let slack = |to: Value, subject: Option<&str>| create(&db, "scribe", &json!({ "channel": "slack", "to": to, "subject": subject, "body": "Ship it" }));
        assert_eq!(slack(json!(["#eng-launch", "@nik.k"]), None).unwrap().to, ["#eng-launch", "@nik.k"]);
        assert!(slack(json!(["general"]), None).is_err(), "needs # or @");
        assert!(slack(json!(["#a b"]), None).is_err());
        assert!(slack(json!(["#eng"]), Some("Topic")).unwrap_err().contains("Only an email"));

        let post = create(&db, "scribe", &json!({ "channel": "post", "body": "We launched." })).unwrap();
        assert!(post.to.is_empty() && post.subject.is_none());
        assert!(create(&db, "scribe", &json!({ "channel": "fax", "body": "x" })).is_err());
        assert!(create(&db, "scribe", &json!({ "channel": "post", "body": "  " })).unwrap_err().contains("empty"));
        assert!(create(&db, "scribe", &json!({ "channel": "post", "body": "x".repeat(BODY_LIMIT + 1) })).is_err());
        assert!(create(&db, "ghost", &json!({ "channel": "post", "body": "x" })).unwrap_err().contains("Only a familiar"));
    }

    #[test]
    fn closes_once_and_tells_the_familiar_what_changed() {
        let db = setup();
        let draft = create(&db, "scribe", &json!({ "channel": "email", "to": ["a@example.com"], "subject": "Plan", "body": "Draft one", "note": "You asked me to follow up." })).unwrap();
        assert_eq!(draft.note.as_deref(), Some("You asked me to follow up."));
        assert_eq!(list(&db).unwrap(), std::slice::from_ref(&draft));

        let edits = Edits { to: vec!["a@example.com".into(), "b@example.com".into()], subject: Some("Plan".into()), body: "Final text".into() };
        let (sent, text) = close(&db, &draft.id, DraftState::Sent, Some(edits)).unwrap();
        assert_eq!((sent.state, sent.body.as_str()), (DraftState::Sent, "Final text"));
        assert!(sent.closed_at.is_some());
        assert!(text.starts_with("The person marked your email draft to a@example.com as sent. They edited it first."), "{text}");
        assert!(text.contains("To: a@example.com, b@example.com") && text.contains("Body:\nFinal text") && !text.contains("Subject:"), "{text}");
        assert_eq!(get(&db, &draft.id).unwrap().unwrap(), sent);
        assert!(close(&db, &draft.id, DraftState::Discarded, None).unwrap_err().contains("already"));

        let slack = create(&db, "scribe", &json!({ "channel": "slack", "to": "#eng", "body": "Heads up" })).unwrap();
        let bad = Edits { to: vec!["eng".into()], subject: None, body: "Heads up".into() };
        assert!(close(&db, &slack.id, DraftState::Sent, Some(bad)).is_err(), "edits are checked too");
        let (_, text) = close(&db, &slack.id, DraftState::Sent, None).unwrap();
        assert_eq!(text, "The person marked your Slack message draft to #eng as sent.");

        let post = create(&db, "scribe", &json!({ "channel": "post", "to": ["LinkedIn"], "body": "News" })).unwrap();
        let (gone, text) = close(&db, &post.id, DraftState::Discarded, None).unwrap();
        assert_eq!(gone.state, DraftState::Discarded);
        assert_eq!(text, "The person discarded your post draft for LinkedIn. Don't send it some other way.");
        assert_eq!((source(DraftState::Sent), source(DraftState::Discarded)), ("Draft sent", "Draft discarded"));

        let pending = create(&db, "scribe", &json!({ "channel": "post", "body": "Waiting" })).unwrap();
        let all = list(&db).unwrap();
        assert_eq!(all[0], pending, "waiting drafts come first");
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn caps_waiting_drafts_and_goes_with_the_familiar() {
        let db = setup();
        for _ in 0..MAX_PENDING {
            create(&db, "scribe", &json!({ "channel": "post", "body": "x" })).unwrap();
        }
        assert!(create(&db, "scribe", &json!({ "channel": "post", "body": "x" })).unwrap_err().contains("already have 20"));
        db.delete_agent("scribe").unwrap();
        assert!(list(&db).unwrap().is_empty());
    }

    #[test]
    fn opens_only_the_link_for_its_channel() {
        assert!(link_allowed(Channel::Email, "mailto:a%40example.com?subject=Hi"));
        assert!(!link_allowed(Channel::Email, "https://example.com"));
        assert!(!link_allowed(Channel::Email, "mailto:a@example.com\n"));
        assert!(link_allowed(Channel::Slack, SLACK_LINK));
        assert!(!link_allowed(Channel::Slack, "slack://channel?id=C1"));
        assert!(!link_allowed(Channel::Post, "https://linkedin.com"));
    }

    #[test]
    fn instructs_only_familiars_with_tools() {
        assert_eq!(prompt(false), None);
        let text = prompt(true).unwrap();
        assert!(text.contains("draft_message") && text.contains("sendmail") && text.contains("Slack webhooks"));
        let names: Vec<String> = crate::tools::definitions().as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
        assert!(names.iter().any(|n| n == TOOL));
        assert!(crate::tools::claude_tool_names().contains("mcp__grimoire__draft_message"));
    }
}
