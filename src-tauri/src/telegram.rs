//! Telegram: the owner reaches this Mac's familiars through a bot they made with @BotFather, with no
//! Tailscale and no open port. Grimoire long-polls the Bot API over outbound HTTPS.
//!
//! The bot token lives in the Keychain and travels only in curl's config on stdin, since it's part of
//! every Bot API URL. Pairing trades a one-time code, sent as `/start CODE`, for the sender's user id
//! and private chat id. After that Grimoire ignores everyone else, groups, channels, forwards, and
//! bots. A paired chat can pick a familiar that lives here, send it text, answer inbox questions,
//! and close drafts, through the same `Actions` the phone companion uses. Approvals only get a
//! heads-up: answering them stays on the Mac.

use crate::approvals::Request;
use crate::companion::Actions;
use crate::db::{now_ms, Agent, Db};
use crate::drafts::{Draft, DraftState};
use crate::inbox::InboxItem;
use crate::link;
use crate::runtime::TurnOutcome;
use crate::vault::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::time::Instant;

pub const API: &str = "https://api.telegram.org";
/// The source label on messages sent from Telegram, shown under them in the chat. Phones label theirs
/// "From <name>", so a phone can't take this one.
pub const SOURCE: &str = "Via Telegram";
pub const CHANGED: &str = "telegram_changed";
const TOKEN_KEY: &str = "telegram:bot";
/// The settings key for `Saved`, stored as JSON. The token is never in it.
const STATE: &str = "telegram";
/// How long Telegram may hold each getUpdates call open.
const POLL_SECONDS: u64 = 50;
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Telegram's limit for one message, in UTF-16 code units.
const MAX_MESSAGE: usize = 4096;
const MAX_TEXT_CHARS: usize = 20_000;
/// Telegram allows 100 buttons on a message.
const MAX_BUTTONS: usize = 90;
const POISONED: &str = "Telegram state poisoned";
const STALE: &str = "That button is out of date.";
const NOT_HERE: &str = "That familiar isn't on this Mac anymore. Pick another with /familiars.";
const HELP: &str = "Send /familiars to pick a familiar, then write to it here. Its replies, its questions, drafts, and alerts from Grimoire show up in this chat.";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Saved {
    enabled: bool,
    /// The bot's username, from getMe.
    bot: Option<String>,
    owner: Option<Owner>,
    /// The next update to ask for, so a restart doesn't replay old ones.
    offset: i64,
    /// The familiar plain text goes to.
    current: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Owner {
    pub user_id: i64,
    pub chat_id: i64,
    pub name: String,
    pub paired_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramStatus {
    pub enabled: bool,
    pub has_token: bool,
    pub bot: Option<String>,
    pub owner: Option<Owner>,
    /// The name of the familiar plain text goes to.
    pub current: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramCode {
    pub code: String,
    /// Milliseconds since the epoch.
    pub expires_at: i64,
    /// A t.me link that opens the bot and sends `/start CODE`.
    pub link: Option<String>,
}

struct Pairing {
    code: String,
    expires: Instant,
    misses: u8,
}

pub struct Telegram {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    actions: Arc<dyn Actions>,
    /// The Bot API's base URL. Tests point it at a local server.
    api: String,
    token: Mutex<Option<String>>,
    pairing: Mutex<Option<Pairing>>,
    error: Mutex<Option<String>>,
    /// Held while the saved state is read, changed, and written.
    saved_lock: Mutex<()>,
    poller: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Bumped whenever the poll loop stops, so a loop that was mid-request drops what it got.
    generation: AtomicU64,
    /// For each familiar, the pairing (its `paired_at`) behind each Telegram turn not yet ended, in
    /// the order the runtime runs them. A reply goes out only to the pairing that asked for it.
    pending: Mutex<HashMap<String, VecDeque<i64>>>,
    /// Open questions and drafts already in the chat, so each goes out once.
    shown: Mutex<HashSet<String>>,
    /// One message at a time, so a long reply's parts arrive in order.
    sending: tokio::sync::Mutex<()>,
}

/// A bot token as @BotFather gives it: the bot's id, a colon, and the secret. Nothing else may reach
/// the URL path it's put in.
fn valid_token(token: &str) -> bool {
    let Some((id, secret)) = token.split_once(':') else { return false };
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && secret.len() >= 20 && secret.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A bot username as Telegram allows it, so it can go in a t.me link.
fn valid_username(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A command's name, without a `@bot` suffix and lowercased, and what follows it.
fn command(text: &str) -> Option<(String, &str)> {
    let rest = text.strip_prefix('/')?;
    let (head, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let name = head.split('@').next().unwrap_or_default().to_ascii_lowercase();
    Some((name, args.trim()))
}

/// Who sent a message, as (user id, chat id), when a person wrote it themselves in a private chat
/// with the bot. Groups, channels, bots, and forwarded or inline-bot messages give None.
fn private_sender(message: &Value) -> Option<(i64, i64)> {
    let relayed = ["forward_origin", "forward_from", "forward_from_chat", "forward_sender_name", "via_bot"].iter().any(|k| message.get(k).is_some());
    let person = message["from"]["is_bot"] == json!(false);
    if relayed || !person || message["chat"]["type"] != "private" {
        return None;
    }
    Some((message["from"]["id"].as_i64()?, message["chat"]["id"].as_i64()?))
}

/// How the owner shows in Settings: their first name and @username, without control characters.
fn owner_name(from: &Value) -> String {
    let first = from["first_name"].as_str().unwrap_or("").trim();
    let name = match from["username"].as_str() {
        Some(user) if first.is_empty() => format!("@{user}"),
        Some(user) => format!("{first} (@{user})"),
        None if first.is_empty() => "Telegram user".into(),
        None => first.to_string(),
    };
    name.chars().filter(|c| !c.is_control()).take(80).collect()
}

fn units(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Splits text into messages Telegram accepts, at a line break or a space when one is near the end.
fn split(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text.trim();
    while units(rest) > MAX_MESSAGE {
        let mut end = 0;
        let mut count = 0;
        for (at, c) in rest.char_indices() {
            if count + c.len_utf16() > MAX_MESSAGE {
                break;
            }
            count += c.len_utf16();
            end = at + c.len_utf8();
        }
        let head = &rest[..end];
        let cut = head.rfind('\n').or_else(|| head.rfind(' ')).filter(|&at| at > end / 2).unwrap_or(end);
        parts.push(rest[..cut].trim_end().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() || parts.is_empty() {
        parts.push(rest.to_string());
    }
    parts
}

/// The start of `text` that fits in one message.
fn clip(text: &str, limit: usize) -> String {
    let mut count = 0;
    text.chars().take_while(|c| {
        count += c.len_utf16();
        count <= limit
    })
    .collect()
}

fn keyboard(rows: Vec<Value>) -> Value {
    json!({ "inline_keyboard": rows })
}

fn button(text: &str, data: String) -> Value {
    json!({ "text": text, "callback_data": data })
}

fn question_message(name: &str, question: &InboxItem) -> (String, Value) {
    let mut text = format!("{name} asks: {}", question.title);
    if !question.detail.is_empty() {
        text.push_str(&format!("\n\n{}", question.detail));
    }
    let rows = question
        .options
        .iter()
        .enumerate()
        .map(|(i, option)| {
            let label = if option.value == question.recommended { format!("{} (recommended)", option.label) } else { option.label.clone() };
            json!([button(&label, format!("q:{}:{i}", question.id))])
        })
        .collect();
    (text, keyboard(rows))
}

fn draft_message(name: &str, draft: &Draft) -> (String, Value) {
    let mut text = format!("{name} wrote a draft for you to send.\n{}", crate::notify::capitalize(&crate::drafts::what(draft)));
    if let Some(subject) = &draft.subject {
        text.push_str(&format!("\nSubject: {subject}"));
    }
    text.push_str(&format!("\n\n{}", draft.body));
    if let Some(note) = &draft.note {
        text.push_str(&format!("\n\nWhy: {note}"));
    }
    let row = json!([button("Mark sent", format!("d:{}:s", draft.id)), button("Discard", format!("d:{}:x", draft.id))]);
    (text, keyboard(vec![row]))
}

fn reply_text(name: &str, outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Done(text) if text.trim().is_empty() => format!("{name} finished without a reply."),
        TurnOutcome::Done(text) => format!("{name}: {}", text.trim()),
        TurnOutcome::Failed(reason) => format!("{name}'s turn failed: {}", reason.trim()),
        TurnOutcome::Stopped => format!("{name} stopped."),
    }
}

impl Telegram {
    fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>, actions: Arc<dyn Actions>, api: String) -> Arc<Self> {
        let token = secrets.get(TOKEN_KEY).unwrap_or_else(|e| {
            eprintln!("Couldn't read the Telegram bot token from the Keychain: {e}");
            None
        });
        let telegram = Telegram {
            db,
            secrets,
            actions,
            api,
            token: Mutex::new(token),
            pairing: Mutex::new(None),
            error: Mutex::new(None),
            saved_lock: Mutex::new(()),
            poller: Mutex::new(None),
            generation: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            shown: Mutex::new(HashSet::new()),
            sending: tokio::sync::Mutex::new(()),
        };
        // What's already open stays on the Mac; only new questions and drafts go to the chat.
        telegram.unseen();
        Arc::new(telegram)
    }

    // ----- State -----

    fn saved(&self) -> Saved {
        self.db.setting(STATE).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    fn change(&self, edit: impl FnOnce(&mut Saved)) -> Result<Saved, String> {
        let _guard = self.saved_lock.lock().map_err(|_| POISONED)?;
        let mut saved = self.saved();
        edit(&mut saved);
        self.db.set_setting(STATE, &serde_json::to_string(&saved).map_err(|e| e.to_string())?)?;
        Ok(saved)
    }

    fn token(&self) -> Option<String> {
        self.token.lock().ok()?.clone()
    }

    fn set_error(&self, error: Option<String>) {
        let changed = self.error.lock().is_ok_and(|mut slot| std::mem::replace(&mut *slot, error.clone()) != error);
        if changed {
            self.actions.changed();
        }
    }

    /// The owner's chat while Telegram is on and has a token.
    fn chat(&self) -> Option<i64> {
        let saved = self.saved();
        if !saved.enabled || self.token().is_none() {
            return None;
        }
        saved.owner.map(|o| o.chat_id)
    }

    fn name(&self, agent_id: &str) -> String {
        self.db.get_agent(agent_id).ok().flatten().map_or_else(|| "A familiar".into(), |a| a.name)
    }

    /// A familiar that lives on this Mac. A linked Mac's `peer:` id or anything malformed is refused.
    fn familiar_here(&self, id: &str) -> Result<Agent, String> {
        if !link::valid_id(id) {
            return Err(NOT_HERE.into());
        }
        self.db.get_agent(id)?.ok_or_else(|| NOT_HERE.into())
    }

    pub fn status(&self) -> Result<TelegramStatus, String> {
        let saved = self.saved();
        Ok(TelegramStatus {
            enabled: saved.enabled,
            has_token: self.token().is_some(),
            bot: saved.bot,
            owner: saved.owner,
            current: saved.current.and_then(|id| self.familiar_here(&id).ok()).map(|a| a.name),
            error: self.error.lock().map_err(|_| POISONED)?.clone(),
        })
    }

    // ----- Settings -----

    pub async fn set_token(self: &Arc<Self>, token: &str) -> Result<TelegramStatus, String> {
        let token = token.trim();
        if !valid_token(token) {
            return Err("That doesn't look like a bot token. @BotFather gives one like 123456789:AAH…".into());
        }
        let me = self.call_with(token, "getMe", json!({})).await?;
        let bot = me["username"].as_str().filter(|n| valid_username(n)).map(String::from);
        self.secrets.set(TOKEN_KEY, token).map_err(|e| format!("Couldn't save the token in the Keychain: {e}"))?;
        // The old bot's loop stops before anything about the new one is saved.
        self.stop();
        let same_bot = self.token().is_some_and(|old| old.split(':').next() == token.split(':').next());
        *self.token.lock().map_err(|_| POISONED)? = Some(token.to_string());
        self.change(|s| {
            s.bot = bot;
            // A new bot has a new chat and its own update numbers.
            if !same_bot {
                s.owner = None;
                s.current = None;
                s.offset = 0;
            }
        })?;

        let commands = json!({ "commands": [{ "command": "familiars", "description": "Pick a familiar to talk to" }] });
        if let Err(e) = self.call("setMyCommands", commands).await {
            eprintln!("Couldn't set the Telegram bot's commands: {e}");
        }
        self.set_error(None);
        self.restart();
        self.actions.changed();
        self.status()
    }

    pub fn forget_token(self: &Arc<Self>) -> Result<TelegramStatus, String> {
        self.stop();
        *self.token.lock().map_err(|_| POISONED)? = None;
        *self.pairing.lock().map_err(|_| POISONED)? = None;
        let deleted = self.secrets.delete(TOKEN_KEY);
        self.change(|s| *s = Saved { enabled: s.enabled, ..Saved::default() })?;
        self.set_error(None);
        deleted.map_err(|e| format!("Couldn't delete the token from the Keychain: {e}"))?;
        self.actions.changed();
        self.status()
    }

    pub fn set_enabled(self: &Arc<Self>, enabled: bool) -> Result<TelegramStatus, String> {
        self.stop();
        self.change(|s| s.enabled = enabled)?;
        if !enabled {
            *self.pairing.lock().map_err(|_| POISONED)? = None;
            self.set_error(None);
        }
        self.restart();
        self.actions.changed();
        self.status()
    }

    pub fn unpair(&self) -> Result<TelegramStatus, String> {
        self.change(|s| {
            s.owner = None;
            s.current = None;
        })?;
        self.actions.changed();
        self.status()
    }

    /// Counts off a Telegram turn that ended for `agent_id`, and says whether its reply should go out:
    /// only while the pairing that sent it is still the one paired.
    fn reply_due(&self, agent_id: &str) -> bool {
        let asked = {
            let Ok(mut pending) = self.pending.lock() else { return false };
            let Some(queue) = pending.get_mut(agent_id) else { return false };
            let asked = queue.pop_front();
            if queue.is_empty() {
                pending.remove(agent_id);
            }
            asked
        };
        asked.is_some() && asked == self.saved().owner.map(|o| o.paired_at)
    }

    /// Stops the poll loop. One that is waiting on Telegram drops whatever comes back.
    fn stop(&self) {
        let Ok(mut poller) = self.poller.lock() else { return };
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(old) = poller.take() {
            old.abort();
        }
    }

    /// Stops the poll loop and starts a new one while Telegram is on and has a token. The loop keeps
    /// the token it started with, so it never asks one bot for another bot's updates.
    fn restart(self: &Arc<Self>) {
        let Ok(mut poller) = self.poller.lock() else { return };
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(old) = poller.take() {
            old.abort();
        }
        if let (true, Some(token)) = (self.saved().enabled, self.token()) {
            *poller = Some(tauri::async_runtime::spawn(self.clone().poll(generation, token)));
        }
    }

    // ----- Pairing -----

    pub fn start_pairing(&self) -> Result<TelegramCode, String> {
        let saved = self.saved();
        if self.token().is_none() {
            return Err("Add your bot's token first.".into());
        }
        if !saved.enabled {
            return Err("Turn on Telegram first.".into());
        }
        let code = link::new_code();
        *self.pairing.lock().map_err(|_| POISONED)? = Some(Pairing { code: code.clone(), expires: Instant::now() + link::CODE_TTL, misses: 0 });
        let link = saved.bot.map(|bot| format!("https://t.me/{bot}?start={code}"));
        Ok(TelegramCode { code, expires_at: now_ms() + link::CODE_TTL.as_millis() as i64, link })
    }

    fn pairing_live(&self, now: Instant) -> bool {
        self.pairing.lock().is_ok_and(|p| p.as_ref().is_some_and(|p| now < p.expires))
    }

    /// Whether `given` is the live code. A match uses it up, and so do too many misses.
    fn check_code(&self, given: &str, now: Instant) -> bool {
        let Ok(mut slot) = self.pairing.lock() else { return false };
        let Some(pairing) = slot.as_mut() else { return false };
        if now >= pairing.expires {
            *slot = None;
            return false;
        }
        if crate::http::same_secret(&pairing.code, &link::normalize_code(given)) {
            *slot = None;
            return true;
        }
        pairing.misses += 1;
        if pairing.misses >= link::CODE_TRIES {
            *slot = None;
        }
        false
    }

    async fn pair(&self, user: i64, chat: i64, from: &Value, code: &str) {
        if !self.check_code(code, Instant::now()) {
            if self.pairing_live(Instant::now()) {
                self.say(chat, "That code didn't work. Check it in Grimoire's Settings, Channels.", None).await;
            }
            return;
        }
        let owner = Owner { user_id: user, chat_id: chat, name: owner_name(from), paired_at: now_ms() };
        if let Err(e) = self.change(|s| {
            s.owner = Some(owner);
            s.current = None;
        }) {
            eprintln!("Couldn't save the Telegram pairing: {e}");
            return;
        }
        // What's open now stays on the Mac, including pushes a past pairing never got.
        self.unseen();
        self.actions.changed();
        self.say(chat, &format!("Paired with Grimoire. {HELP}"), None).await;
    }

    // ----- The Bot API -----

    async fn call(&self, method: &str, body: Value) -> Result<Value, String> {
        let token = self.token().ok_or("Add your bot's token first.")?;
        self.call_with(&token, method, body).await
    }

    async fn call_with(&self, token: &str, method: &str, body: Value) -> Result<Value, String> {
        let url = format!("{}/bot{token}/{method}", self.api);
        let headers = [("Content-Type", "application/json".to_string())];
        // Neither curl's error nor Telegram's description may carry the token back out.
        let redact = |e: String| e.replace(token, "<token>");
        let answer = crate::connectors::fetch("POST", &url, &headers, Some(&body.to_string())).await.map_err(redact)?;
        let reply: Value = serde_json::from_str(&answer.body).map_err(|_| format!("Telegram sent an unreadable reply (HTTP {}).", answer.status))?;
        if reply["ok"] == json!(true) {
            return Ok(reply["result"].clone());
        }
        Err(redact(match reply["error_code"].as_i64() {
            Some(401 | 404) => "Telegram didn't accept the bot token. Copy it again from @BotFather.".into(),
            Some(409) => "Something else is reading this bot's messages, like a webhook or another app. Stop it, or make a new bot for Grimoire.".into(),
            _ => format!("Telegram said: {}", reply["description"].as_str().unwrap_or("something went wrong")),
        }))
    }

    async fn poll(self: Arc<Self>, generation: u64, token: String) {
        let mut wait = Duration::from_secs(1);
        while self.generation.load(Ordering::SeqCst) == generation {
            match self.poll_as(generation, &token).await {
                Ok(()) => {
                    self.set_error(None);
                    wait = Duration::from_secs(1);
                }
                Err(e) => {
                    self.set_error(Some(e));
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    #[cfg(test)]
    async fn poll_once(&self) -> Result<(), String> {
        self.poll_as(self.generation.load(Ordering::SeqCst), &self.token().unwrap_or_default()).await
    }

    /// One getUpdates call for the poll loop of `generation`. A stopped loop acts on nothing more.
    async fn poll_as(&self, generation: u64, token: &str) -> Result<(), String> {
        let offset = self.saved().offset;
        let mut body = json!({ "timeout": POLL_SECONDS, "allowed_updates": ["message", "callback_query"] });
        if offset > 0 {
            body["offset"] = json!(offset);
        }
        let updates = self.call_with(token, "getUpdates", body).await?.as_array().cloned().unwrap_or_default();
        for update in &updates {
            let Some(id) = update["update_id"].as_i64() else { continue };
            // Saved before acting, so a crash mid-update skips it instead of doing it twice. Updates
            // after it stay unconfirmed and come back next time. The check runs under the state lock:
            // a bot change bumps the generation before it resets the offset, so a stopped loop can
            // neither act nor write over the reset.
            let mut live = false;
            self.change(|s| {
                live = self.generation.load(Ordering::SeqCst) == generation;
                if live {
                    s.offset = s.offset.max(id + 1);
                }
            })?;
            if !live {
                return Ok(());
            }
            if let Some(message) = update.get("message") {
                self.on_message(message).await;
            } else if let Some(press) = update.get("callback_query") {
                self.on_button(press).await;
            }
        }
        Ok(())
    }

    // ----- From the chat -----

    async fn on_message(&self, message: &Value) {
        let Some((user, chat)) = private_sender(message) else { return };
        let text = message["text"].as_str().unwrap_or("").trim();
        let saved = self.saved();
        let Some(owner) = saved.owner else {
            if let Some(("start", code)) = command(text).as_ref().map(|(n, a)| (n.as_str(), *a)) {
                if !code.is_empty() {
                    self.pair(user, chat, &message["from"], code).await;
                }
            }
            return;
        };
        if owner.user_id != user || owner.chat_id != chat {
            return;
        }
        let reply = match command(text) {
            Some((name, _)) if name == "familiars" => return self.show_familiars(chat, saved.current.as_deref()).await,
            Some((name, _)) if name == "start" || name == "help" => HELP.to_string(),
            Some(_) => "Grimoire knows /familiars and /help.".into(),
            None if text.is_empty() => "Grimoire only takes text messages for now.".into(),
            None => match self.send(&owner, saved.current.as_deref(), text) {
                Ok(()) => {
                    let _ = self.call("sendChatAction", json!({ "chat_id": chat, "action": "typing" })).await;
                    return;
                }
                Err(e) => e,
            },
        };
        self.say(chat, &reply, None).await;
    }

    fn send(&self, owner: &Owner, current: Option<&str>, text: &str) -> Result<(), String> {
        let agent = self.familiar_here(current.ok_or("Pick a familiar first with /familiars.")?)?;
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(format!("Keep a message under {MAX_TEXT_CHARS} characters."));
        }
        // Noted before sending, since a turn that can't start may end before send returns.
        let note = |add: bool| {
            if let Ok(mut pending) = self.pending.lock() {
                let queue = pending.entry(agent.id.clone()).or_default();
                if add {
                    queue.push_back(owner.paired_at);
                } else {
                    queue.pop_back();
                }
            }
        };
        note(true);
        self.actions.send(&agent.id, text.to_string(), SOURCE).inspect_err(|_| note(false))?;
        Ok(())
    }

    async fn show_familiars(&self, chat: i64, current: Option<&str>) {
        let agents: Vec<Agent> = self.db.list_agents().unwrap_or_default().into_iter().filter(|a| link::valid_id(&a.id)).take(MAX_BUTTONS).collect();
        if agents.is_empty() {
            self.say(chat, "There are no familiars on this Mac yet.", None).await;
            return;
        }
        let rows = agents
            .iter()
            .map(|a| {
                let label = if current == Some(a.id.as_str()) { format!("{} (current)", a.name) } else { a.name.clone() };
                json!([button(&label, format!("f:{}", a.id))])
            })
            .collect();
        self.say(chat, "Who do you want to talk to?", Some(keyboard(rows))).await;
    }

    async fn on_button(&self, press: &Value) {
        let Some(id) = press["id"].as_str() else { return };
        let message = &press["message"];
        let (Some(user), Some(chat), Some(message_id)) = (press["from"]["id"].as_i64(), message["chat"]["id"].as_i64(), message["message_id"].as_i64()) else {
            return;
        };
        let Some(owner) = self.saved().owner else { return };
        if owner.user_id != user || owner.chat_id != chat {
            return;
        }
        let result = self.press(press["data"].as_str().unwrap_or(""));
        let note = match &result {
            Ok(text) | Err(text) => text.clone(),
        };
        let _ = self.call("answerCallbackQuery", json!({ "callback_query_id": id, "text": clip(&note, 190), "show_alert": result.is_err() })).await;
        if result.is_ok() {
            // Says what was picked and takes the buttons away.
            let text = clip(&format!("{}\n\n{note}", message["text"].as_str().unwrap_or("")), MAX_MESSAGE);
            let _ = self.call("editMessageText", json!({ "chat_id": chat, "message_id": message_id, "text": text })).await;
        }
    }

    /// Does what a button asks. Each kind looks up its own item, so a button can't reach another kind.
    fn press(&self, data: &str) -> Result<String, String> {
        let (kind, rest) = data.split_once(':').ok_or(STALE)?;
        match kind {
            "f" => {
                let agent = self.familiar_here(rest)?;
                self.change(|s| s.current = Some(agent.id.clone()))?;
                self.actions.changed();
                Ok(format!("Talking to {}. Write your message.", agent.name))
            }
            "q" => {
                let (id, index) = rest.rsplit_once(':').ok_or(STALE)?;
                let index: usize = index.parse().map_err(|_| STALE)?;
                let question = crate::inbox::open_questions(&self.db)?.into_iter().find(|q| q.id == id).ok_or("That question was already answered or dismissed.")?;
                let option = question.options.get(index).ok_or(STALE)?.value.clone();
                self.actions.answer_question(id, &option)?;
                Ok(format!("You chose: {option}"))
            }
            "d" => {
                let (id, action) = rest.rsplit_once(':').ok_or(STALE)?;
                let sent = match action {
                    "s" => true,
                    "x" => false,
                    _ => return Err(STALE.into()),
                };
                self.actions.close_draft(id, sent, None)?;
                Ok(if sent { "Marked sent." } else { "Discarded." }.into())
            }
            _ => Err(STALE.into()),
        }
    }

    // ----- To the chat -----

    /// Sends text in as many messages as it takes, with the buttons on the last one. False when
    /// Telegram refused a part.
    async fn say(&self, chat: i64, text: &str, buttons: Option<Value>) -> bool {
        let _turn = self.sending.lock().await;
        let parts = split(text);
        let last = parts.len() - 1;
        for (i, part) in parts.into_iter().enumerate() {
            let mut body = json!({ "chat_id": chat, "text": part, "link_preview_options": { "is_disabled": true } });
            if let (true, Some(buttons)) = (i == last, &buttons) {
                body["reply_markup"] = buttons.clone();
            }
            if let Err(e) = self.call("sendMessage", body).await {
                eprintln!("Couldn't send a Telegram message: {e}");
                return false;
            }
        }
        true
    }

    async fn tell_owner(&self, text: &str, buttons: Option<Value>) -> bool {
        match self.chat() {
            Some(chat) => self.say(chat, text, buttons).await,
            None => false,
        }
    }

    /// Questions and drafts that opened since the last look, each claimed once.
    fn unseen(&self) -> (Vec<InboxItem>, Vec<Draft>) {
        let questions = crate::inbox::open_questions(&self.db).unwrap_or_default();
        let drafts: Vec<Draft> = crate::drafts::list(&self.db).unwrap_or_default().into_iter().filter(|d| d.state == DraftState::Draft).collect();
        let Ok(mut shown) = self.shown.lock() else { return (Vec::new(), Vec::new()) };
        let open: HashSet<String> = questions.iter().map(|q| format!("q:{}", q.id)).chain(drafts.iter().map(|d| format!("d:{}", d.id))).collect();
        shown.retain(|key| open.contains(key));
        let questions = questions.into_iter().filter(|q| shown.insert(format!("q:{}", q.id))).collect();
        let drafts = drafts.into_iter().filter(|d| shown.insert(format!("d:{}", d.id))).collect();
        (questions, drafts)
    }

    async fn push_inbox(&self) {
        let (questions, drafts) = self.unseen();
        if self.chat().is_none() {
            return;
        }
        let messages = questions
            .iter()
            .map(|q| (format!("q:{}", q.id), question_message(&self.name(&q.agent_id), q)))
            .chain(drafts.iter().map(|d| (format!("d:{}", d.id), draft_message(&self.name(&d.agent_id), d))));
        for (key, (text, buttons)) in messages.collect::<Vec<_>>() {
            // One that didn't get through goes again with the next change to the inbox or drafts.
            if !self.tell_owner(&text, Some(buttons)).await {
                if let Ok(mut shown) = self.shown.lock() {
                    shown.remove(&key);
                }
            }
        }
    }

    fn approval_text(&self, request: &Request) -> Option<String> {
        if self.chat().is_none() || !crate::notify::load(&self.db).approvals {
            return None;
        }
        let name = self.name(crate::side::split(&request.agent_id).0);
        let summary = crate::notify::approval_text(&name, request).1;
        Some(format!("{name} needs your OK: {}. Open Grimoire to answer.", summary.trim_end_matches('.')))
    }

    /// A ritual or spell alert, when Settings, Notifications lets those through.
    fn run_text(&self, title: String, body: String) -> Option<String> {
        if self.chat().is_none() || !crate::notify::load(&self.db).runs {
            return None;
        }
        Some(format!("{title}\n{body}"))
    }
}

// ---------- The app's side ----------

static TELEGRAM: OnceLock<Arc<Telegram>> = OnceLock::new();

fn running() -> Result<&'static Arc<Telegram>, String> {
    TELEGRAM.get().ok_or_else(|| "Telegram isn't running.".into())
}

fn tell_owner(text: Option<String>) {
    let (Some(telegram), Some(text)) = (TELEGRAM.get().cloned(), text) else { return };
    tauri::async_runtime::spawn(async move {
        telegram.tell_owner(&text, None).await;
    });
}

/// A turn ended. A turn Telegram started sends its reply back there.
pub fn turn_ended(agent_id: &str, source: Option<&str>, outcome: &TurnOutcome) {
    if source != Some(SOURCE) {
        return;
    }
    tell_owner(TELEGRAM.get().filter(|t| t.reply_due(agent_id)).map(|t| reply_text(&t.name(agent_id), outcome)));
}

pub fn approval_requested(request: &Request) {
    tell_owner(TELEGRAM.get().and_then(|t| t.approval_text(request)));
}

pub fn ritual_fired(ritual: &str, agent_id: &str) {
    tell_owner(TELEGRAM.get().and_then(|t| {
        let (title, body) = crate::notify::ritual_text(ritual, &t.name(agent_id));
        t.run_text(title, body)
    }));
}

pub fn spell_ended(spell: &str, state: &str, step: usize, total: usize, detail: Option<&str>) {
    tell_owner(TELEGRAM.get().and_then(|t| {
        let (title, body) = crate::notify::spell_text(spell, state, step, total, detail)?;
        t.run_text(title, body)
    }));
}

/// Starts polling when Telegram is on and has a token, and follows new questions and drafts.
pub fn install(app: &tauri::AppHandle, db: Arc<Db>, secrets: Arc<dyn SecretStore>, runtime: Arc<crate::runtime::Runtime>, approvals: Arc<crate::approvals::Approvals>) {
    use tauri::Listener;
    let actions = Arc::new(crate::companion::AppActions { app: app.clone(), db: db.clone(), runtime, approvals, changed: CHANGED });
    let telegram = Telegram::new(db, secrets, actions, API.into());
    if TELEGRAM.set(telegram.clone()).is_err() {
        return;
    }
    for event in [crate::inbox::CHANGED, crate::drafts::CHANGED] {
        let telegram = telegram.clone();
        app.listen_any(event, move |_| {
            let telegram = telegram.clone();
            tauri::async_runtime::spawn(async move { telegram.push_inbox().await });
        });
    }
    telegram.restart();
}

#[tauri::command]
pub fn telegram_status() -> Result<TelegramStatus, String> {
    running()?.status()
}

#[tauri::command]
pub async fn telegram_set_token(token: String) -> Result<TelegramStatus, String> {
    running()?.set_token(&token).await
}

#[tauri::command]
pub async fn telegram_forget_token() -> Result<TelegramStatus, String> {
    running()?.forget_token()
}

#[tauri::command]
pub async fn telegram_set_enabled(enabled: bool) -> Result<TelegramStatus, String> {
    running()?.set_enabled(enabled)
}

#[tauri::command]
pub fn telegram_start_pairing() -> Result<TelegramCode, String> {
    running()?.start_pairing()
}

#[tauri::command]
pub fn telegram_unpair() -> Result<TelegramStatus, String> {
    running()?.unpair()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approvals::Decision;
    use crate::db::{Access, Avatar, Message, Provider};
    use crate::drafts::Edits;
    use crate::vault::MemoryStore;
    use std::collections::VecDeque;
    use tokio::net::TcpListener;

    const TOKEN: &str = "123456:ABCdefGHIjklMNOpqrSTUvwxYZ01234567";
    const OWNER: i64 = 42;

    #[derive(Default)]
    struct FakeActions {
        sent: Mutex<Vec<(String, String, String)>>,
        answered: Mutex<Vec<(String, String)>>,
        closed: Mutex<Vec<(String, bool)>>,
    }

    impl Actions for FakeActions {
        fn busy(&self) -> Vec<String> {
            Vec::new()
        }
        fn send(&self, agent_id: &str, text: String, source: &str) -> Result<Message, String> {
            self.sent.lock().unwrap().push((agent_id.into(), text.clone(), source.into()));
            Ok(Message { id: 1, agent_id: agent_id.into(), role: "user".into(), text, status: "done".into(), source: Some(source.into()), created_at: 0, updated_at: None, attachments: Vec::new() })
        }
        fn approvals(&self) -> Vec<Request> {
            Vec::new()
        }
        fn answer_approval(&self, _id: &str, _decision: Decision) -> Result<(), String> {
            Err("Telegram never answers approvals.".into())
        }
        fn answer_question(&self, id: &str, answer: &str) -> Result<(), String> {
            self.answered.lock().unwrap().push((id.into(), answer.into()));
            Ok(())
        }
        fn dismiss_question(&self, _id: &str) -> Result<(), String> {
            Ok(())
        }
        fn close_draft(&self, id: &str, sent: bool, _edits: Option<Edits>) -> Result<Draft, String> {
            self.closed.lock().unwrap().push((id.into(), sent));
            Err("The fake keeps no drafts.".into())
        }
        fn changed(&self) {}
    }

    /// A local stand-in for the Bot API: it records each call and hands out queued updates.
    #[derive(Clone, Default)]
    struct FakeApi {
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        updates: Arc<Mutex<VecDeque<Value>>>,
        /// Refuses sendMessage while set.
        refuse_sends: Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeApi {
        async fn start(&self) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let api = self.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else { continue };
                    let api = api.clone();
                    tokio::spawn(async move {
                        let Ok(request) = crate::http::read_request(&mut stream, 1 << 20, |_| Ok(())).await else { return };
                        let path = request.head.path.clone();
                        let method = path.rsplit('/').next().unwrap_or_default().to_string();
                        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                        api.calls.lock().unwrap().push((method.clone(), body));
                        let reply = if !path.starts_with(&format!("/bot{TOKEN}/")) {
                            json!({ "ok": false, "error_code": 401, "description": "Unauthorized" })
                        } else {
                            match method.as_str() {
                                "getUpdates" => json!({ "ok": true, "result": api.updates.lock().unwrap().drain(..).collect::<Vec<_>>() }),
                                "getMe" => json!({ "ok": true, "result": { "id": 123456, "is_bot": true, "username": "grimoire_test_bot" } }),
                                "sendMessage" if api.refuse_sends.load(Ordering::SeqCst) => json!({ "ok": false, "error_code": 429, "description": "Too Many Requests" }),
                                // A server that echoes the path back, token and all.
                                "echo" => json!({ "ok": false, "error_code": 400, "description": format!("Bad request for {path}") }),
                                _ => json!({ "ok": true, "result": true }),
                            }
                        };
                        crate::http::reply(&mut stream, "200 OK", "application/json", reply.to_string().as_bytes()).await;
                    });
                }
            });
            address
        }

        fn calls(&self, method: &str) -> Vec<Value> {
            self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, body)| body.clone()).collect()
        }

        fn texts(&self) -> Vec<String> {
            self.calls("sendMessage").iter().map(|b| b["text"].as_str().unwrap_or_default().to_string()).collect()
        }

        fn queue(&self, updates: impl IntoIterator<Item = Value>) {
            self.updates.lock().unwrap().extend(updates);
        }

        fn clear(&self) {
            self.calls.lock().unwrap().clear();
        }
    }

    struct Setup {
        telegram: Arc<Telegram>,
        actions: Arc<FakeActions>,
        api: FakeApi,
        db: Arc<Db>,
        secrets: Arc<MemoryStore>,
        address: String,
    }

    async fn setup() -> Setup {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.set_setting(STATE, r#"{"enabled":true}"#).unwrap();
        let secrets = Arc::new(MemoryStore::new());
        secrets.set(TOKEN_KEY, TOKEN).unwrap();
        let actions = Arc::new(FakeActions::default());
        let api = FakeApi::default();
        let address = api.start().await;
        let telegram = Telegram::new(db.clone(), secrets.clone(), actions.clone(), address.clone());
        Setup { telegram, actions, api, db, secrets, address }
    }

    fn familiar(db: &Db, id: &str, name: &str) {
        db.insert_agent(&Agent {
            id: id.into(),
            name: name.into(),
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
        })
        .unwrap();
    }

    fn message(update_id: i64, user: i64, chat: Value, text: &str) -> Value {
        json!({ "update_id": update_id, "message": { "message_id": update_id, "from": { "id": user, "is_bot": false, "first_name": "Nik", "username": "nik" }, "chat": chat, "date": 0, "text": text } })
    }

    fn private(user: i64) -> Value {
        json!({ "id": user, "type": "private" })
    }

    fn press(update_id: i64, user: i64, data: &str) -> Value {
        json!({ "update_id": update_id, "callback_query": { "id": format!("cb{update_id}"), "from": { "id": user, "is_bot": false }, "data": data, "message": { "message_id": 7, "chat": private(OWNER), "text": "Pick one" } } })
    }

    async fn paired(s: &Setup) {
        let code = s.telegram.start_pairing().unwrap().code;
        s.api.queue([message(1, OWNER, private(OWNER), &format!("/start {}", code.to_lowercase()))]);
        s.telegram.poll_once().await.unwrap();
        assert_eq!(s.telegram.saved().owner.map(|o| o.user_id), Some(OWNER));
        s.api.clear();
    }

    #[tokio::test]
    async fn a_start_code_pairs_the_sender_and_only_once() {
        let s = setup().await;
        let code = s.telegram.start_pairing().unwrap();
        assert_eq!(code.link, None, "no link until getMe gave the bot's name");
        s.api.queue([message(1, OWNER, private(OWNER), &format!("/start@grimoire_test_bot {}", code.code))]);
        s.telegram.poll_once().await.unwrap();
        let owner = s.telegram.saved().owner.unwrap();
        assert_eq!((owner.user_id, owner.chat_id, owner.name.as_str()), (OWNER, OWNER, "Nik (@nik)"));
        assert!(s.api.texts()[0].starts_with("Paired with Grimoire."));

        // The code is used up, and once paired a stranger's /start is ignored.
        s.api.clear();
        s.api.queue([message(2, 7, private(7), &format!("/start {}", code.code))]);
        s.telegram.poll_once().await.unwrap();
        assert_eq!(s.telegram.saved().owner.unwrap().user_id, OWNER);
        assert!(s.api.texts().is_empty());
    }

    #[tokio::test]
    async fn wrong_and_expired_codes_and_five_misses_do_not_pair() {
        let s = setup().await;
        s.telegram.start_pairing().unwrap();
        s.api.queue([message(1, OWNER, private(OWNER), "/start WRONG234"), message(2, OWNER, private(OWNER), "/start")]);
        s.telegram.poll_once().await.unwrap();
        assert!(s.telegram.saved().owner.is_none());
        assert_eq!(s.api.texts(), ["That code didn't work. Check it in Grimoire's Settings, Channels."], "a bare /start spends no try and gets no reply");
        assert_eq!(s.telegram.pairing.lock().unwrap().as_ref().unwrap().misses, 1);

        let code = s.telegram.start_pairing().unwrap().code;
        assert!(!s.telegram.check_code(&code, Instant::now() + link::CODE_TTL), "expired");
        assert!(s.telegram.pairing.lock().unwrap().is_none());

        let code = s.telegram.start_pairing().unwrap().code;
        for _ in 0..link::CODE_TRIES {
            assert!(!s.telegram.check_code("WRONG234", Instant::now()));
        }
        assert!(!s.telegram.check_code(&code, Instant::now()), "five misses end the code");
    }

    #[tokio::test]
    async fn pairing_needs_the_channel_on() {
        let s = setup().await;
        s.telegram.set_enabled(false).unwrap();
        assert!(s.telegram.start_pairing().is_err());
        s.api.queue([message(1, OWNER, private(OWNER), "/start ABCD2345")]);
        s.telegram.poll_once().await.unwrap();
        assert!(s.telegram.saved().owner.is_none());
    }

    #[tokio::test]
    async fn only_the_owner_writing_in_the_private_chat_is_heard() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        s.telegram.change(|saved| saved.current = Some("ada".into())).unwrap();
        let mut forwarded = message(5, OWNER, private(OWNER), "forwarded");
        forwarded["message"]["forward_origin"] = json!({ "type": "user" });
        let mut from_bot = message(6, OWNER, private(OWNER), "bot");
        from_bot["message"]["from"]["is_bot"] = json!(true);
        s.api.queue([
            message(2, 7, private(7), "stranger"),
            message(3, OWNER, json!({ "id": -100, "type": "group" }), "group"),
            message(4, OWNER, json!({ "id": 7, "type": "private" }), "other chat"),
            forwarded,
            from_bot,
            json!({ "update_id": 7, "channel_post": { "chat": { "id": -200, "type": "channel" }, "text": "post" } }),
            press(8, 7, "f:ada"),
            message(9, OWNER, private(OWNER), "hello"),
        ]);
        s.telegram.poll_once().await.unwrap();
        assert_eq!(*s.actions.sent.lock().unwrap(), vec![("ada".to_string(), "hello".to_string(), SOURCE.to_string())]);
        assert!(s.api.texts().is_empty(), "strangers get no reply");
        assert!(s.api.calls("answerCallbackQuery").is_empty());
    }

    #[tokio::test]
    async fn text_goes_to_the_familiar_picked_with_a_button() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        s.api.queue([message(2, OWNER, private(OWNER), "hello"), message(3, OWNER, private(OWNER), "/familiars")]);
        s.telegram.poll_once().await.unwrap();
        assert_eq!(s.api.texts(), ["Pick a familiar first with /familiars.", "Who do you want to talk to?"]);
        let picker = s.api.calls("sendMessage")[1].clone();
        assert_eq!(picker["reply_markup"]["inline_keyboard"], json!([[{ "text": "Ada", "callback_data": "f:ada" }]]));

        s.api.queue([press(4, OWNER, "f:ada"), press(5, OWNER, "f:peer:mac:ada"), message(6, OWNER, private(OWNER), " hi Ada ")]);
        s.telegram.poll_once().await.unwrap();
        assert_eq!(s.telegram.status().unwrap().current.as_deref(), Some("Ada"));
        let answers = s.api.calls("answerCallbackQuery");
        assert_eq!(answers[0]["text"], "Talking to Ada. Write your message.");
        assert_eq!((answers[1]["text"].as_str().unwrap(), answers[1]["show_alert"].as_bool()), (NOT_HERE, Some(true)));
        assert_eq!(s.api.calls("editMessageText")[0]["text"], "Pick one\n\nTalking to Ada. Write your message.");
        assert_eq!(*s.actions.sent.lock().unwrap(), vec![("ada".to_string(), "hi Ada".to_string(), SOURCE.to_string())]);
    }

    #[tokio::test]
    async fn a_button_answers_an_inbox_question_and_drafts_close() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        crate::inbox::call_tool(&s.db, "ada", "ask_person", &json!({ "question": "Ship it?", "options": ["Ship", "Wait"], "recommended": "Ship" })).unwrap();
        s.telegram.push_inbox().await;
        s.telegram.push_inbox().await;
        let pushed = s.api.calls("sendMessage");
        assert_eq!(pushed.len(), 1, "each question goes out once");
        assert_eq!(pushed[0]["text"], "Ada asks: Ship it?");
        let id = crate::inbox::open_questions(&s.db).unwrap()[0].id.clone();
        let buttons = &pushed[0]["reply_markup"]["inline_keyboard"];
        assert_eq!(buttons[0][0], json!({ "text": "Ship (recommended)", "callback_data": format!("q:{id}:0") }));
        assert_eq!(buttons[1][0], json!({ "text": "Wait", "callback_data": format!("q:{id}:1") }));

        s.api.queue([press(2, 7, &format!("q:{id}:0")), press(3, OWNER, &format!("q:{id}:1")), press(4, OWNER, "d:d-1:s"), press(5, OWNER, "d:d-2:x")]);
        s.telegram.poll_once().await.unwrap();
        assert_eq!(*s.actions.answered.lock().unwrap(), vec![(id, "Wait".to_string())]);
        assert_eq!(*s.actions.closed.lock().unwrap(), vec![("d-1".to_string(), true), ("d-2".to_string(), false)]);
    }

    #[tokio::test]
    async fn questions_and_drafts_open_before_launch_or_pairing_stay_on_the_mac() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        crate::inbox::call_tool(&s.db, "ada", "ask_person", &json!({ "question": "Old?", "options": ["Yes", "No"], "recommended": "Yes" })).unwrap();
        let telegram = Telegram::new(s.db.clone(), s.secrets.clone(), s.actions.clone(), s.address.clone());
        crate::drafts::create(&s.db, "ada", &json!({ "channel": "email", "to": ["a@example.com"], "subject": "Hi", "body": "Before pairing" })).unwrap();
        telegram.push_inbox().await;
        let code = telegram.start_pairing().unwrap().code;
        s.api.queue([message(1, OWNER, private(OWNER), &format!("/start {code}"))]);
        telegram.poll_once().await.unwrap();
        s.api.clear();
        crate::drafts::create(&s.db, "ada", &json!({ "channel": "email", "to": ["b@example.com"], "subject": "Plan", "body": "After pairing", "note": "You asked" })).unwrap();
        telegram.push_inbox().await;
        let pushed = s.api.calls("sendMessage");
        assert_eq!(pushed.len(), 1);
        assert_eq!(pushed[0]["text"], "Ada wrote a draft for you to send.\nEmail draft to b@example.com\nSubject: Plan\n\nAfter pairing\n\nWhy: You asked");
        let row = &pushed[0]["reply_markup"]["inline_keyboard"][0];
        assert_eq!((row[0]["text"].as_str(), row[1]["text"].as_str()), (Some("Mark sent"), Some("Discard")));
    }

    #[test]
    fn long_replies_split_under_the_limit() {
        let lines = format!("{}\n{}", "a".repeat(3000), "b".repeat(2000));
        assert_eq!(split(&lines), vec!["a".repeat(3000), "b".repeat(2000)]);
        let solid = "c".repeat(9000);
        let parts = split(&solid);
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [4096, 4096, 808]);
        // Each emoji is two UTF-16 units, which is what Telegram counts.
        let emoji = "🦉".repeat(3000);
        let parts = split(&emoji);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| units(p) <= MAX_MESSAGE));
        assert_eq!(parts.concat(), emoji);
        assert_eq!(split("short"), ["short"]);
        assert_eq!(reply_text("Ada", &TurnOutcome::Done(" Done. ".into())), "Ada: Done.");
        assert_eq!(reply_text("Ada", &TurnOutcome::Failed("No sign-in".into())), "Ada's turn failed: No sign-in");
    }

    #[tokio::test]
    async fn a_long_reply_reaches_the_owner_in_parts() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        let reply = reply_text(&s.telegram.name("ada"), &TurnOutcome::Done(format!("{}\n{}", "x".repeat(4000), "y".repeat(100))));
        s.telegram.tell_owner(&reply, None).await;
        assert_eq!(s.api.texts(), [format!("Ada: {}", "x".repeat(4000)), "y".repeat(100)]);
        assert!(s.api.calls("sendMessage").iter().all(|b| b["chat_id"] == json!(OWNER)));
    }

    #[tokio::test]
    async fn the_offset_survives_a_restart() {
        let s = setup().await;
        s.api.queue([message(10, 7, private(7), "a"), message(11, 7, private(7), "b")]);
        s.telegram.poll_once().await.unwrap();
        assert_eq!(s.telegram.saved().offset, 12);
        let again = Telegram::new(s.db.clone(), s.secrets.clone(), s.actions.clone(), s.address.clone());
        again.poll_once().await.unwrap();
        let polls = s.api.calls("getUpdates");
        assert_eq!(polls[0].get("offset"), None);
        assert_eq!(polls[1]["offset"], 12);
        assert_eq!(polls[1]["timeout"], POLL_SECONDS);
    }

    #[tokio::test]
    async fn the_token_lives_in_the_secret_store_and_a_bad_one_is_refused() {
        let s = setup().await;
        s.telegram.set_enabled(false).unwrap();
        s.telegram.forget_token().unwrap();
        for bad in ["", "123456", "abc:ABCdefGHIjklMNOpqrSTUvwxYZ", "123456:short", "123456:ABCdefGHIjklMNOpqrSTU/../x"] {
            assert!(s.telegram.set_token(bad).await.is_err(), "{bad}");
        }
        assert!(s.api.calls("getMe").is_empty(), "malformed tokens never reach Telegram");
        let wrong = "999999:ABCdefGHIjklMNOpqrSTUvwxYZ01234567";
        let error = s.telegram.set_token(wrong).await.unwrap_err();
        assert!(error.contains("didn't accept") && !error.contains(wrong), "{error}");
        assert!(s.secrets.get(TOKEN_KEY).unwrap().is_none());

        let status = s.telegram.set_token(TOKEN).await.unwrap();
        assert_eq!((status.has_token, status.bot.as_deref()), (true, Some("grimoire_test_bot")));
        assert_eq!(s.secrets.get(TOKEN_KEY).unwrap().as_deref(), Some(TOKEN));
        assert!(!s.db.setting(STATE).unwrap().unwrap().contains("ABCdef"), "the token stays out of the database");
        assert_eq!(s.api.calls("setMyCommands").len(), 1);
        s.telegram.set_enabled(true).unwrap();
        let code = s.telegram.start_pairing().unwrap();
        assert_eq!(code.link, Some(format!("https://t.me/grimoire_test_bot?start={}", code.code)));
        s.telegram.set_enabled(false).unwrap();
    }

    #[tokio::test]
    async fn approvals_get_a_heads_up_without_buttons_and_runs_follow_the_notification_settings() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        let request = Request { id: "r1".into(), agent_id: "ada".into(), kind: "command", title: "Ada wants to run a command".into(), detail: "git push".into(), can_remember: true };
        assert_eq!(s.telegram.approval_text(&request), None, "nothing goes out before pairing");
        paired(&s).await;
        assert_eq!(s.telegram.approval_text(&request).as_deref(), Some("Ada needs your OK: git push. Open Grimoire to answer."));
        assert_eq!(s.telegram.run_text("Morning ran".into(), "Ada is working on it.".into()).as_deref(), Some("Morning ran\nAda is working on it."));
        s.db.set_setting("notifications", r#"{"runs":false,"approvals":false}"#).unwrap();
        assert_eq!(s.telegram.run_text("Morning ran".into(), "x".into()), None);
        assert_eq!(s.telegram.approval_text(&request), None);
        // Answering an approval has no button to press.
        assert_eq!(s.telegram.press("a:r1:once"), Err(STALE.into()));
    }

    #[tokio::test]
    async fn unpairing_stops_listening_to_the_old_owner() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        s.telegram.change(|saved| saved.current = Some("ada".into())).unwrap();
        s.telegram.unpair().unwrap();
        s.api.queue([message(2, OWNER, private(OWNER), "hello"), press(3, OWNER, "f:ada")]);
        s.telegram.poll_once().await.unwrap();
        assert!(s.actions.sent.lock().unwrap().is_empty());
        assert!(s.api.texts().is_empty());
        assert_eq!(s.telegram.chat(), None);
    }

    #[tokio::test]
    async fn a_reply_reaches_only_the_owner_who_asked() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        s.telegram.change(|saved| saved.current = Some("ada".into())).unwrap();
        s.api.queue([message(2, OWNER, private(OWNER), "one"), message(3, OWNER, private(OWNER), "two")]);
        s.telegram.poll_once().await.unwrap();
        assert!(s.telegram.reply_due("ada") && s.telegram.reply_due("ada"));
        assert!(!s.telegram.reply_due("ada"), "a turn that didn't come from Telegram stays on the Mac");

        // A turn from the old pairing still running when the owner pairs again doesn't reach the new
        // pairing, and doesn't use up the new pairing's turn.
        s.api.queue([message(4, OWNER, private(OWNER), "old")]);
        s.telegram.poll_once().await.unwrap();
        s.telegram.unpair().unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        let code = s.telegram.start_pairing().unwrap().code;
        s.api.queue([message(5, OWNER, private(OWNER), &format!("/start {code}"))]);
        s.telegram.poll_once().await.unwrap();
        s.telegram.change(|saved| saved.current = Some("ada".into())).unwrap();
        s.api.queue([message(6, OWNER, private(OWNER), "new")]);
        s.telegram.poll_once().await.unwrap();
        assert!(!s.telegram.reply_due("ada"), "the old turn ends first and stays quiet");
        assert!(s.telegram.reply_due("ada"), "the new turn's reply goes out");
    }

    #[tokio::test]
    async fn a_stopped_poll_loop_acts_on_nothing_it_was_waiting_for() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        s.telegram.change(|saved| saved.current = Some("ada".into())).unwrap();
        let old = s.telegram.generation.load(Ordering::SeqCst);
        s.telegram.stop();
        s.api.queue([message(5, OWNER, private(OWNER), "late")]);
        s.telegram.poll_as(old, TOKEN).await.unwrap();
        assert!(s.actions.sent.lock().unwrap().is_empty());
        assert_eq!(s.telegram.saved().offset, 2, "the update stays unconfirmed for the next loop");
    }

    #[tokio::test]
    async fn a_push_telegram_refused_goes_again_with_the_next_change() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        crate::inbox::call_tool(&s.db, "ada", "ask_person", &json!({ "question": "Ship it?", "options": ["Ship", "Wait"], "recommended": "Ship" })).unwrap();
        s.api.refuse_sends.store(true, Ordering::SeqCst);
        s.telegram.push_inbox().await;
        s.api.refuse_sends.store(false, Ordering::SeqCst);
        s.telegram.push_inbox().await;
        s.telegram.push_inbox().await;
        assert_eq!(s.api.texts(), ["Ada asks: Ship it?", "Ada asks: Ship it?"], "refused once, then sent once");
    }

    #[tokio::test]
    async fn a_refused_push_never_reaches_a_later_pairing() {
        let s = setup().await;
        familiar(&s.db, "ada", "Ada");
        paired(&s).await;
        crate::inbox::call_tool(&s.db, "ada", "ask_person", &json!({ "question": "Ship it?", "options": ["Ship", "Wait"], "recommended": "Ship" })).unwrap();
        s.api.refuse_sends.store(true, Ordering::SeqCst);
        s.telegram.push_inbox().await;
        s.api.refuse_sends.store(false, Ordering::SeqCst);
        s.telegram.unpair().unwrap();
        let code = s.telegram.start_pairing().unwrap().code;
        s.api.queue([message(2, 99, private(99), &format!("/start {code}"))]);
        s.telegram.poll_once().await.unwrap();
        s.api.clear();
        s.telegram.push_inbox().await;
        assert!(s.api.texts().is_empty());
    }

    #[tokio::test]
    async fn errors_never_carry_the_token() {
        let s = setup().await;
        let error = s.telegram.call("echo", json!({})).await.unwrap_err();
        assert!(error.contains("<token>") && !error.contains(TOKEN), "{error}");
    }
}
