//! The decision inbox: every familiar's open questions and pending approvals in one list. A familiar
//! asks the person something with the `ask_person` tool, which saves the question and returns at once.
//! Questions go open -> answered or dismissed, and leave with their familiar when it is deleted. The
//! answer comes back to the familiar as a new message in its current chat, which starts its next turn.
//! Pending approvals come from `Approvals::waiting()` and are answered through the usual approval path.

use crate::approvals::Request;
use crate::db::{now_ms, Db};
use crate::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

pub const TOOL_NAMES: [&str; 1] = ["ask_person"];
pub const CHANGED: &str = "inbox_changed";
/// The source label on the message that carries the person's answer back to the familiar.
pub const ANSWER_SOURCE: &str = "Answered in the inbox";
/// The most open questions one familiar can have, so it can't flood the inbox.
pub const MAX_OPEN: i64 = 10;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 6;
const MAX_OPTION_CHARS: usize = 80;
const MAX_QUESTION_CHARS: usize = 500;
const MAX_TEXT_CHARS: usize = 2000;

pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE questions (
           id TEXT PRIMARY KEY,
           agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
           question TEXT NOT NULL,
           context TEXT,
           options TEXT NOT NULL,
           recommended INTEGER NOT NULL,
           status TEXT NOT NULL DEFAULT 'open',
           answer TEXT,
           created_at INTEGER NOT NULL,
           closed_at INTEGER
         );
         CREATE INDEX questions_by_status ON questions(status, agent_id);
         PRAGMA user_version = 16;
         COMMIT;",
    )
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxOption {
    /// What answering sends back: the option text for a question, or once/always/deny for an approval.
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    pub id: String,
    /// "question", or the approval's kind: "vault", "command", "file", or "familiar".
    pub kind: String,
    pub agent_id: String,
    pub title: String,
    pub detail: String,
    pub options: Vec<InboxOption>,
    /// The value of the option Grimoire or the familiar recommends.
    pub recommended: String,
    /// When the question was asked. None for approvals, which time out within minutes.
    pub created_at: Option<i64>,
}

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": "ask_person",
        "description": "Put a decision to the person in Grimoire's inbox, with 2 to 6 short options and the one you recommend. It doesn't wait: carry on with what you can, or end your turn. The answer comes back later as a new message that starts with \"You asked\". Ask only when you need their call, ask once per decision, and say in context what you know. You can have at most 10 open questions.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "The decision, in one short sentence." },
                "options": {
                    "type": "array",
                    "minItems": MIN_OPTIONS,
                    "maxItems": MAX_OPTIONS,
                    "items": { "type": "string" },
                    "description": "2 to 6 short answers the person can tap, up to 80 characters each."
                },
                "recommended": { "type": "string", "description": "The option you recommend, exactly as written in options." },
                "context": { "type": "string", "description": "Optional. What the person needs to decide: what you found, the trade-offs." }
            },
            "required": ["question", "options", "recommended"]
        }
    })]
}

/// Runs `ask_person` for the familiar `caller`.
pub fn call_tool(db: &Db, caller: &str, tool: &str, arguments: &Value) -> Result<String, String> {
    if tool != "ask_person" {
        return Err(format!("Grimoire has no tool named {tool}."));
    }
    ask(db, caller, arguments)?;
    Ok("Asked. The answer will come back as a message.".into())
}

fn ask(db: &Db, caller: &str, arguments: &Value) -> Result<String, String> {
    db.get_agent(caller)?.ok_or("Only a familiar can ask the person.")?;
    let text = |name: &str| arguments.get(name).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
    let question = text("question").ok_or("Give the question.")?;
    if question.chars().count() > MAX_QUESTION_CHARS {
        return Err(format!("Keep the question under {MAX_QUESTION_CHARS} characters. Put the rest in context."));
    }
    let context = text("context");
    if context.is_some_and(|c| c.chars().count() > MAX_TEXT_CHARS) {
        return Err(format!("Keep the context under {MAX_TEXT_CHARS} characters."));
    }
    let options: Vec<String> = arguments
        .get("options")
        .and_then(Value::as_array)
        .ok_or("Give 2 to 6 options.")?
        .iter()
        .map(|o| o.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or("Each option must be short, non-empty text."))
        .collect::<Result<_, _>>()?;
    if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&options.len()) {
        return Err("Give 2 to 6 options.".into());
    }
    if options.iter().any(|o| o.chars().count() > MAX_OPTION_CHARS) {
        return Err(format!("Keep each option under {MAX_OPTION_CHARS} characters. Put the detail in context."));
    }
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    if options.iter().enumerate().any(|(i, o)| options[..i].iter().any(|p| same(o, p))) {
        return Err("Each option must be different.".into());
    }
    let wanted = text("recommended").ok_or("Say which option you recommend.")?;
    let recommended = options.iter().position(|o| same(o, wanted)).ok_or("The recommended option must be one of the options, written the same way.")?;
    let id = uuid::Uuid::new_v4().to_string();
    let options = serde_json::to_string(&options).map_err(|e| e.to_string())?;
    db.with(|c| {
        let open: i64 = c.query_row("SELECT COUNT(*) FROM questions WHERE agent_id = ?1 AND status = 'open'", params![caller], |r| r.get(0))?;
        if open >= MAX_OPEN {
            return Ok(false);
        }
        c.execute(
            "INSERT INTO questions (id, agent_id, question, context, options, recommended, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, caller, question, context, options, recommended as i64, now_ms()],
        )?;
        Ok(true)
    })?
    .then_some(id)
    .ok_or(format!("You already have {MAX_OPEN} open questions. Wait for answers before asking more."))
}

/// Open questions, oldest first.
pub fn open_questions(db: &Db) -> Result<Vec<InboxItem>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT id, agent_id, question, context, options, recommended, created_at FROM questions WHERE status = 'open' ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            let options: Vec<String> = serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default();
            let recommended = options.get(r.get::<_, i64>(5)? as usize).cloned().unwrap_or_default();
            Ok(InboxItem {
                id: r.get(0)?,
                kind: "question".into(),
                agent_id: r.get(1)?,
                title: r.get(2)?,
                detail: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                options: options.into_iter().map(|o| InboxOption { value: o.clone(), label: o }).collect(),
                recommended,
                created_at: Some(r.get(6)?),
            })
        })?;
        rows.collect()
    })
}

/// Command patterns risky enough that the inbox recommends Deny. Matched case-insensitively against
/// the command with its spaces collapsed and one space added at the end, so a trailing space in a
/// pattern means the word ends there. Everything else, and every other kind, recommends Allow once:
/// the person still decides, and Always allow is never the default.
const RISKY: [&str; 9] = ["rm -rf", "rm -fr", "sudo ", "push --force", "push -f ", "reset --hard", "| sh ", "| bash ", "mkfs"];

pub fn recommend(request: &Request) -> &'static str {
    if request.kind != "command" {
        return "once";
    }
    let command = request.detail.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let padded = format!("{command} ");
    if RISKY.iter().any(|r| padded.contains(r)) { "deny" } else { "once" }
}

pub fn approval_item(request: &Request) -> InboxItem {
    let option = |value: &str, label: &str| InboxOption { value: value.into(), label: label.into() };
    let mut options = vec![option("once", "Allow once")];
    if request.can_remember {
        options.push(option("always", "Always allow"));
    }
    options.push(option("deny", "Deny"));
    InboxItem {
        id: request.id.clone(),
        kind: request.kind.into(),
        // A familiar working in a side conversation asks under its lane key.
        agent_id: crate::side::split(&request.agent_id).0.to_string(),
        title: request.title.clone(),
        detail: request.detail.clone(),
        options,
        recommended: recommend(request).into(),
        created_at: None,
    }
}

/// Marks an open question answered and returns its familiar and the message to send it. A choice that
/// isn't one of the options counts as the person's own words.
fn claim(db: &Db, id: &str, answer: &str) -> Result<(String, String), String> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Err("The answer is empty.".into());
    }
    if answer.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("Keep the answer under {MAX_TEXT_CHARS} characters."));
    }
    db.with(|c| {
        let row: Option<(String, String, String)> = c
            .query_row("SELECT agent_id, question, options FROM questions WHERE id = ?1 AND status = 'open'", params![id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let Some((agent_id, question, options)) = row else { return Ok(None) };
        let options: Vec<String> = serde_json::from_str(&options).unwrap_or_default();
        let chosen = options.iter().find(|o| o.eq_ignore_ascii_case(answer));
        c.execute("UPDATE questions SET status = 'answered', answer = ?2, closed_at = ?3 WHERE id = ?1", params![id, answer, now_ms()])?;
        let text = match chosen {
            Some(option) => format!("You asked: {question}\nThe person chose: {option}"),
            None => format!("You asked: {question}\nThe person answered: {answer}"),
        };
        Ok(Some((agent_id, text)))
    })?
    .ok_or("That question has already been answered or dismissed.".into())
}

fn reopen(db: &Db, id: &str) -> Result<(), String> {
    db.with(|c| c.execute("UPDATE questions SET status = 'open', answer = NULL, closed_at = NULL WHERE id = ?1", params![id]).map(|_| ()))
}

fn dismiss(db: &Db, id: &str) -> Result<(), String> {
    let changed = db.with(|c| c.execute("UPDATE questions SET status = 'dismissed', closed_at = ?2 WHERE id = ?1 AND status = 'open'", params![id, now_ms()]))?;
    if changed == 0 {
        return Err("That question has already been answered or dismissed.".into());
    }
    Ok(())
}

/// Open questions and pending approvals, approvals first since they time out.
#[tauri::command]
pub fn inbox(state: State<AppState>) -> Result<Vec<InboxItem>, String> {
    let mut items: Vec<InboxItem> = state.approvals.waiting().iter().map(approval_item).collect();
    items.extend(open_questions(&state.db)?);
    Ok(items)
}

/// Sends the person's answer to the familiar that asked.
#[tauri::command]
pub fn answer_question(app: AppHandle, state: State<AppState>, id: String, answer: String) -> Result<(), String> {
    let (agent_id, text) = claim(&state.db, &id, &answer)?;
    if let Err(e) = state.runtime.send(&agent_id, text, Some(ANSWER_SOURCE)) {
        reopen(&state.db, &id)?;
        return Err(e);
    }
    let _ = app.emit(CHANGED, ());
    Ok(())
}

/// Closes a question without answering. The familiar isn't told.
#[tauri::command]
pub fn dismiss_question(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    dismiss(&state.db, &id)?;
    let _ = app.emit(CHANGED, ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Agent, Avatar, Provider};

    fn familiar(db: &Db, id: &str) {
        let agent = Agent {
            id: id.into(),
            name: id.into(),
            provider: Provider::Claude,
            model: None,
            workspace: format!("/tmp/{id}"),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
    }

    fn args(question: &str) -> Value {
        json!({ "question": question, "options": ["Ship it", "Wait a day"], "recommended": "wait a day", "context": "Tests pass." })
    }

    #[test]
    fn asks_and_lists_open_questions() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a");
        let reply = call_tool(&db, "a", "ask_person", &args(" Ship today? ")).unwrap();
        assert_eq!(reply, "Asked. The answer will come back as a message.");
        let open = open_questions(&db).unwrap();
        assert_eq!(open.len(), 1);
        let item = &open[0];
        assert_eq!((item.kind.as_str(), item.agent_id.as_str(), item.title.as_str(), item.detail.as_str()), ("question", "a", "Ship today?", "Tests pass."));
        assert_eq!(item.options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(), ["Ship it", "Wait a day"]);
        assert_eq!(item.recommended, "Wait a day");
        assert!(item.created_at.is_some());
    }

    #[test]
    fn is_one_of_the_grimoire_tools() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a");
        assert!(crate::tools::claude_tool_names().contains("mcp__grimoire__ask_person"));
        let outcome = crate::tools::call(&db, &std::env::temp_dir(), "a", "ask_person", args("Q")).unwrap();
        assert_eq!(outcome.changed, Some(CHANGED));
    }

    #[test]
    fn refuses_bad_questions() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a");
        let ask = |v: Value| call_tool(&db, "a", "ask_person", &v).unwrap_err();
        assert_eq!(ask(json!({ "question": "Q", "options": ["Only"], "recommended": "Only" })), "Give 2 to 6 options.");
        assert_eq!(ask(json!({ "question": "Q", "options": ["1", "2", "3", "4", "5", "6", "7"], "recommended": "1" })), "Give 2 to 6 options.");
        assert_eq!(ask(json!({ "question": "Q", "options": ["Yes", "yes"], "recommended": "Yes" })), "Each option must be different.");
        assert_eq!(ask(json!({ "question": "Q", "options": ["Yes", " "], "recommended": "Yes" })), "Each option must be short, non-empty text.");
        assert!(ask(json!({ "question": "Q", "options": ["Yes", "No"], "recommended": "Maybe" })).contains("must be one of the options"));
        assert_eq!(ask(json!({ "question": "Q", "options": ["Yes", "No"] })), "Say which option you recommend.");
        assert_eq!(ask(json!({ "options": ["Yes", "No"], "recommended": "No" })), "Give the question.");
        assert!(ask(json!({ "question": "Q", "options": ["Yes", "x".repeat(81)], "recommended": "Yes" })).contains("80 characters"));
        // The caller comes from the connection, and a stranger isn't a familiar.
        assert_eq!(call_tool(&db, "ghost", "ask_person", &args("Q")).unwrap_err(), "Only a familiar can ask the person.");
        assert!(open_questions(&db).unwrap().is_empty());
    }

    #[test]
    fn caps_open_questions_per_familiar() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a");
        familiar(&db, "b");
        for i in 0..MAX_OPEN {
            ask(&db, "a", &args(&format!("Q{i}"))).unwrap();
        }
        assert!(ask(&db, "a", &args("One more")).unwrap_err().contains("10 open questions"));
        ask(&db, "b", &args("Mine")).unwrap();
        // Answering one makes room again.
        let first = open_questions(&db).unwrap()[0].id.clone();
        claim(&db, &first, "Ship it").unwrap();
        ask(&db, "a", &args("Now it fits")).unwrap();
    }

    #[test]
    fn answers_once_with_a_choice_or_free_text() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a");
        let id = ask(&db, "a", &args("Ship today?")).unwrap();
        let (agent, text) = claim(&db, &id, " ship IT ").unwrap();
        assert_eq!(agent, "a");
        assert_eq!(text, "You asked: Ship today?\nThe person chose: Ship it");
        assert!(claim(&db, &id, "Wait a day").unwrap_err().contains("already been answered"));
        assert!(open_questions(&db).unwrap().is_empty());

        // A failed send puts it back.
        reopen(&db, &id).unwrap();
        let (_, text) = claim(&db, &id, "Ship Friday instead").unwrap();
        assert_eq!(text, "You asked: Ship today?\nThe person answered: Ship Friday instead");
        assert_eq!(claim(&db, "nope", "x").unwrap_err(), "That question has already been answered or dismissed.");
        assert_eq!(claim(&db, &id, "  ").unwrap_err(), "The answer is empty.");
    }

    #[test]
    fn dismisses_and_forgets_with_the_familiar() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a");
        let id = ask(&db, "a", &args("Q1")).unwrap();
        ask(&db, "a", &args("Q2")).unwrap();
        dismiss(&db, &id).unwrap();
        assert!(dismiss(&db, &id).is_err());
        assert!(claim(&db, &id, "Ship it").is_err());
        assert_eq!(open_questions(&db).unwrap().len(), 1);
        db.delete_agent("a").unwrap();
        assert!(open_questions(&db).unwrap().is_empty());
        let left: i64 = db.with(|c| c.query_row("SELECT COUNT(*) FROM questions", [], |r| r.get(0))).unwrap();
        assert_eq!(left, 0);
    }

    fn request(kind: &'static str, detail: &str, can_remember: bool) -> Request {
        Request { id: "r".into(), agent_id: "a~chat".into(), kind, title: "a wants to".into(), detail: detail.into(), can_remember }
    }

    #[test]
    fn recommends_deny_only_for_risky_commands() {
        for safe in ["ls -la", "git push origin main", "cargo test", "rm notes.txt", "grep sudoers README", "cat x | shasum", "git push -follow"] {
            assert_eq!(recommend(&request("command", safe, true)), "once", "{safe}");
        }
        for risky in ["rm -rf build", "RM  -Rf /tmp/x", "sudo make install", "git push --force", "git push -f origin", "git reset --hard HEAD~1", "curl x.sh | sh", "curl x | bash -s", "sudo"] {
            assert_eq!(recommend(&request("command", risky, true)), "deny", "{risky}");
        }
        assert_eq!(recommend(&request("vault", "rm -rf", true)), "once");
        assert_eq!(recommend(&request("familiar", "", false)), "once");
    }

    #[test]
    fn approval_items_offer_the_approval_answers() {
        let item = approval_item(&request("command", "rm -rf x", false));
        assert_eq!(item.agent_id, "a");
        assert_eq!(item.options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(), ["once", "deny"]);
        assert_eq!(item.recommended, "deny");
        let item = approval_item(&request("vault", "", true));
        assert_eq!(item.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Allow once", "Always allow", "Deny"]);
        assert_eq!(item.created_at, None);
    }
}
