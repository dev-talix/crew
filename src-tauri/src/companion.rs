//! The phone companion. Grimoire serves a small web app to the owner's phone over their Tailscale
//! tailnet: the inbox, drafts, and chat with this Mac's familiars. See docs/phone-companion.md.
//!
//! The server listens on port 47621 on the addresses Tailscale reports for this Mac, only while a
//! phone is paired or a pairing code is live. It drops connections from anything Tailscale doesn't
//! list as a peer or as this Mac, the same check linked Macs use. Pairing trades a one-time code for
//! a token; tokens live in the Keychain and every API call carries one. The phone never picks who
//! acts: chat reaches only familiars that live here, and inbox items and drafts name their familiar.

use crate::approvals::{Decision, Request};
use crate::db::{now_ms, Db, Message};
use crate::drafts::{Draft, Edits};
use crate::http;
use crate::link::{self, BoxFuture, EventLog};
use crate::vault::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;
use tokio::time::Instant;

pub const PORT: u16 = 47621;
const BODY_LIMIT: usize = 128 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const BIND_RETRY: Duration = Duration::from_secs(60);
/// Each phone holds a long poll and makes a request now and then.
const MAX_CONNECTIONS: usize = 32;
const MAX_PHONES: usize = 10;
const MAX_TEXT_CHARS: usize = 20_000;
/// The setting that lists paired phones. Their tokens are in the Keychain.
const PHONES: &str = "companion_phones";
/// How stale a phone's last-seen time may get before it's saved again.
const SEEN_EVERY_MS: i64 = 60_000;
const CHANGED: &str = "companion_changed";
const NO_CLI: &str = "Grimoire can't find Tailscale's command-line tool, so the phone companion is off. Make sure the Tailscale app is installed.";
const CSP: &str = "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// What a paired phone may do here. The app runs it through the runtime and the approval gate.
pub trait Actions: Send + Sync {
    fn busy(&self) -> Vec<String>;
    fn send(&self, agent_id: &str, text: String, source: &str) -> Result<Message, String>;
    fn approvals(&self) -> Vec<Request>;
    fn answer_approval(&self, id: &str, decision: Decision) -> Result<(), String>;
    /// Answers an open question. The question names the familiar that asked.
    fn answer_question(&self, id: &str, answer: &str) -> Result<(), String>;
    fn dismiss_question(&self, id: &str) -> Result<(), String>;
    /// Marks a draft sent, with edits, or discarded. The draft names the familiar that wrote it.
    fn close_draft(&self, id: &str, sent: bool, edits: Option<Edits>) -> Result<Draft, String>;
    /// The phone list or the server's state changed, for the settings page.
    fn changed(&self);
}

/// Where the server may listen and whom it lets in: Tailscale through the link, a fake in tests.
pub trait Network: Send + Sync {
    /// This Mac's tailnet addresses, or None when Tailscale's CLI isn't found.
    fn addresses(&self) -> BoxFuture<'_, Option<Vec<IpAddr>>>;
    /// Whether `ip` is a peer on the tailnet.
    fn trusts(&self, ip: IpAddr) -> BoxFuture<'_, bool>;
}

/// The built phone app: Tauri's embedded assets, or a map in tests.
pub trait Assets: Send + Sync {
    /// The bytes and media type of a file in the built frontend, like "phone.html".
    fn get(&self, path: &str) -> Option<(Vec<u8>, String)>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phone {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub last_seen: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    pub code: String,
    /// Milliseconds since the epoch.
    pub expires_at: i64,
    /// Addresses to open on the phone, like http://100.101.102.103:47621.
    pub urls: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionStatus {
    pub listening: bool,
    pub urls: Vec<String>,
    pub error: Option<String>,
    pub phones: Vec<Phone>,
}

struct Pairing {
    code: String,
    expires: Instant,
    misses: u8,
}

#[derive(Default)]
struct Server {
    urls: Vec<String>,
    error: Option<String>,
    /// This Mac's own tailnet addresses, which may also open the app.
    own: Vec<IpAddr>,
}

pub struct Companion {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    actions: Arc<dyn Actions>,
    network: Arc<dyn Network>,
    assets: Arc<dyn Assets>,
    mac: String,
    port: u16,
    events: EventLog,
    pairing: Mutex<Option<Pairing>>,
    pair_tries: Mutex<VecDeque<Instant>>,
    /// Each phone's token, by phone id.
    tokens: Mutex<HashMap<String, String>>,
    /// Held while the phone list is read, changed, and saved, so two changes can't undo each other.
    phones_lock: Mutex<()>,
    server: Mutex<Server>,
    /// Each address's server and the URL it answers at.
    bound: tokio::sync::Mutex<HashMap<IpAddr, (AbortHandle, String)>>,
    connections: Arc<Semaphore>,
}

fn token_key(phone: &str) -> String {
    format!("companion:{phone}")
}

fn error(text: &str) -> Value {
    json!({ "error": text })
}

fn status_line(status: u16) -> &'static str {
    match status {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        429 => "429 Too Many Requests",
        _ => "500 Internal Server Error",
    }
}

fn url(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v4) => format!("http://{v4}:{port}"),
        IpAddr::V6(v6) => format!("http://[{v6}]:{port}"),
    }
}

/// The built file a path asks for. Only the app page and Vite's hashed assets, so a path can't
/// reach anything else in the frontend's folder.
fn asset_path(path: &str) -> Option<String> {
    if path == "/" || path == "/phone.html" {
        return Some("phone.html".into());
    }
    let file = path.strip_prefix("/assets/")?;
    let plain = !file.is_empty() && !file.starts_with('.') && file.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    plain.then(|| format!("assets/{file}"))
}

/// Whether a request's Host names this Mac the way a phone reaches it: a Tailscale address, a
/// MagicDNS name, or a short name. A web page that points its own domain at this Mac (DNS
/// rebinding) sends its domain, which has a dot and isn't under ts.net, so the API refuses it.
fn trusted_host(host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let name = match host.strip_prefix('[') {
        Some(v6) => return v6.split_once(']').is_some_and(|(ip, _)| ip.parse::<std::net::Ipv6Addr>().is_ok()),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name.parse::<std::net::Ipv4Addr>().is_ok() || name.ends_with(".ts.net") || (!name.is_empty() && !name.contains('.'))
}

/// A familiar as the phone lists it: no workspace, access, or session.
fn familiar_value(agent: &crate::db::Agent) -> Value {
    json!({
        "id": agent.id,
        "name": agent.name,
        "provider": agent.provider,
        "model": agent.active_model.as_ref().or(agent.model.as_ref()),
        "avatar": agent.avatar,
    })
}

/// An inbox item as the phone shows it. Always allow is a lasting grant, so it stays on the Mac.
fn phone_item(mut item: crate::inbox::InboxItem) -> crate::inbox::InboxItem {
    item.options.retain(|o| o.value != "always");
    item
}

impl Companion {
    #[allow(clippy::too_many_arguments)]
    fn new(
        db: Arc<Db>,
        secrets: Arc<dyn SecretStore>,
        actions: Arc<dyn Actions>,
        network: Arc<dyn Network>,
        assets: Arc<dyn Assets>,
        mac: String,
        port: u16,
    ) -> Result<Arc<Self>, String> {
        let mut tokens = HashMap::new();
        let companion = Companion {
            db,
            secrets,
            actions,
            network,
            assets,
            mac,
            port,
            events: EventLog::new(),
            pairing: Mutex::new(None),
            pair_tries: Mutex::new(VecDeque::new()),
            tokens: Mutex::new(HashMap::new()),
            phones_lock: Mutex::new(()),
            server: Mutex::new(Server::default()),
            bound: tokio::sync::Mutex::new(HashMap::new()),
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        };
        for phone in companion.phones()? {
            match companion.secrets.get(&token_key(&phone.id)) {
                Ok(Some(token)) => {
                    tokens.insert(phone.id, token);
                }
                _ => eprintln!("The Keychain has no token for the phone {}. Pair it again.", phone.name),
            }
        }
        *companion.tokens.lock().map_err(|_| "companion state poisoned")? = tokens;
        Ok(Arc::new(companion))
    }

    // ----- Phones -----

    pub fn phones(&self) -> Result<Vec<Phone>, String> {
        Ok(self.db.setting(PHONES)?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default())
    }

    fn save_phones(&self, phones: &[Phone]) -> Result<(), String> {
        self.db.set_setting(PHONES, &serde_json::to_string(phones).map_err(|e| e.to_string())?)
    }

    fn has_phones(&self) -> bool {
        self.tokens.lock().is_ok_and(|t| !t.is_empty())
    }

    /// The phone a request's token belongs to.
    fn phone_for(&self, authorization: Option<&str>) -> Option<String> {
        let token = authorization?.strip_prefix("Bearer ")?.trim();
        let tokens = self.tokens.lock().ok()?;
        // Compare with every token so the time doesn't say which one matched.
        let mut found = None;
        for (id, expect) in tokens.iter() {
            if http::same_secret(expect, token) {
                found = Some(id.clone());
            }
        }
        found
    }

    fn mark_seen(&self, phone: &str) -> Option<String> {
        let _guard = self.phones_lock.lock().ok()?;
        let mut phones = self.phones().ok()?;
        let entry = phones.iter_mut().find(|p| p.id == phone)?;
        let name = entry.name.clone();
        let now = now_ms();
        if entry.last_seen.is_none_or(|seen| now - seen >= SEEN_EVERY_MS) {
            entry.last_seen = Some(now);
            let _ = self.save_phones(&phones);
        }
        Some(name)
    }

    /// Forgets a phone and its token. Its next request gets 401.
    pub async fn remove(self: &Arc<Self>, phone: &str) -> Result<(), String> {
        {
            let _guard = self.phones_lock.lock().map_err(|_| "companion state poisoned")?;
            // The token goes first, so the phone is locked out even if saving the list fails.
            self.tokens.lock().map_err(|_| "companion state poisoned")?.remove(phone);
            let saved = self.phones().and_then(|phones| self.save_phones(&phones.into_iter().filter(|p| p.id != phone).collect::<Vec<_>>()));
            // Deleted either way: a row left by a failed save can't load a token that's gone.
            let deleted = self.secrets.delete(&token_key(phone));
            saved?;
            if let Err(e) = deleted {
                eprintln!("Couldn't delete a removed phone's token from the Keychain: {e}");
            }
        }
        self.actions.changed();
        self.rebind().await;
        Ok(())
    }

    // ----- Pairing -----

    /// Starts a pairing code, replacing any earlier one, and opens the port if it was closed.
    pub async fn start_pairing(self: &Arc<Self>) -> Result<PairingCode, String> {
        let code = link::new_code();
        let expires_at = now_ms() + link::CODE_TTL.as_millis() as i64;
        *self.pairing.lock().map_err(|_| "companion state poisoned")? =
            Some(Pairing { code: code.clone(), expires: Instant::now() + link::CODE_TTL, misses: 0 });
        self.rebind().await;
        // Close the port once the code runs out, unless a phone paired or a newer code took over.
        let companion = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(link::CODE_TTL).await;
            companion.rebind().await;
        });
        let status = self.status()?;
        Ok(PairingCode { code, expires_at, urls: status.urls, error: status.error })
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
        if http::same_secret(&pairing.code, &link::normalize_code(given)) {
            *slot = None;
            return true;
        }
        pairing.misses += 1;
        if pairing.misses >= link::CODE_TRIES {
            *slot = None;
        }
        false
    }

    fn pair_allowed(&self, now: Instant) -> bool {
        let Ok(mut tries) = self.pair_tries.lock() else { return false };
        while tries.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60)) {
            tries.pop_front();
        }
        if tries.len() >= link::PAIR_PER_MINUTE {
            return false;
        }
        tries.push_back(now);
        true
    }

    fn pair(&self, body: &[u8], now: Instant) -> (u16, Value) {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Pair {
            code: String,
            name: String,
        }
        if !self.pair_allowed(now) {
            return (429, error("Too many pairing attempts. Wait a minute, then try again."));
        }
        let Ok(request) = serde_json::from_slice::<Pair>(body) else { return (400, error("Bad pairing request.")) };
        // Checked before the code, so a fixable mistake doesn't use it up.
        let Some(name) = link::clean_mac_name(&request.name) else { return (400, error("Give this phone a name.")) };
        let Ok(_guard) = self.phones_lock.lock() else { return (500, error("companion state poisoned")) };
        let mut phones = match self.phones() {
            Ok(phones) => phones,
            Err(e) => return (500, error(&e)),
        };
        if phones.len() >= MAX_PHONES {
            return (400, error("Grimoire already has 10 phones paired. Remove one in Settings first."));
        }
        if !self.check_code(&request.code, now) {
            return (403, error("That code didn't work. Start pairing again on your Mac if it expired."));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let token = link::new_token();
        if let Err(e) = self.secrets.set(&token_key(&id), &token) {
            return (500, error(&format!("Couldn't save the phone's token in the Keychain: {e}")));
        }
        phones.push(Phone { id: id.clone(), name, created_at: now_ms(), last_seen: Some(now_ms()) });
        if let Err(e) = self.save_phones(&phones) {
            let _ = self.secrets.delete(&token_key(&id));
            return (500, error(&e));
        }
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(id.clone(), token.clone());
        }
        self.actions.changed();
        (200, json!({ "token": token, "deviceId": id, "mac": self.mac }))
    }

    // ----- Events -----

    /// Notes an event for paired phones' long polls. Nothing is kept while no phone is paired.
    fn record(&self, kind: &str, payload: Value) {
        if self.has_phones() {
            self.events.push(kind, payload);
        }
    }

    async fn events(&self, query: &str) -> Value {
        let Some(after) = http::query_param(query, "after").and_then(|v| v.parse::<u64>().ok()) else {
            return json!({ "seq": self.events.head(), "epoch": self.events.epoch, "events": [], "reset": false });
        };
        let wait = http::query_param(query, "wait").and_then(|v| v.parse::<u64>().ok()).map_or(link::LONG_POLL, Duration::from_secs).min(link::LONG_POLL);
        let batch = self.events.wait(after, wait).await;
        json!({ "seq": batch.seq, "epoch": self.events.epoch, "events": batch.events, "reset": batch.reset })
    }

    // ----- Serving -----

    pub fn status(&self) -> Result<CompanionStatus, String> {
        let server = self.server.lock().map_err(|_| "companion state poisoned")?;
        Ok(CompanionStatus { listening: !server.urls.is_empty(), urls: server.urls.clone(), error: server.error.clone(), phones: self.phones()? })
    }

    async fn trusts(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        let own = self.server.lock().is_ok_and(|s| s.own.contains(&ip));
        own || self.network.trusts(ip).await
    }

    /// Binds the port on this Mac's tailnet addresses while a phone is paired or a code is live, and
    /// lets go of everything otherwise.
    /// Boxed, since serving a phone's unpair comes back here.
    fn rebind(self: &Arc<Self>) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut bound = self.bound.lock().await;
            let wanted = self.has_phones() || self.pairing_live(Instant::now());
            let checked = if wanted { self.network.addresses().await } else { Some(Vec::new()) };
            let found = checked.clone().unwrap_or_default();
            bound.retain(|ip, (server, _)| {
                let keep = found.contains(ip);
                if !keep {
                    server.abort();
                }
                keep
            });
            let mut failures = Vec::new();
            for ip in &found {
                if bound.contains_key(ip) {
                    continue;
                }
                match TcpListener::bind((*ip, self.port)).await {
                    Ok(listener) => {
                        // Tests bind port 0 and get whatever the system picks.
                        let port = listener.local_addr().map_or(self.port, |a| a.port());
                        bound.insert(*ip, (tokio::spawn(self.clone().serve(listener)).abort_handle(), url(*ip, port)));
                    }
                    Err(e) => failures.push(format!("{ip}: {e}")),
                }
            }
            // IPv4 first: it's the one people type.
            let mut urls: Vec<String> = bound.values().map(|(_, u)| u.clone()).collect();
            urls.sort_by_key(|u| (u.contains('['), u.clone()));
            let error = if !wanted {
                None
            } else if checked.is_none() {
                Some(NO_CLI.to_string())
            } else if found.is_empty() {
                Some("Tailscale isn't connected on this Mac, so your phone can't reach it. Grimoire checks again every minute.".to_string())
            } else if !failures.is_empty() {
                Some(format!("Grimoire couldn't listen on port {} at {}.", self.port, failures.join(", ")))
            } else {
                None
            };
            let changed = match self.server.lock() {
                Ok(mut server) => {
                    let changed = server.urls != urls || server.error != error;
                    server.urls = urls;
                    server.error = error;
                    server.own = found;
                    changed
                }
                Err(_) => false,
            };
            if changed {
                self.actions.changed();
            }
        })
    }

    async fn listen(self: Arc<Self>) {
        loop {
            self.rebind().await;
            tokio::time::sleep(BIND_RETRY).await;
        }
    }

    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            let (stream, from) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let Ok(permit) = self.connections.clone().try_acquire_owned() else { continue };
            let companion = self.clone();
            tokio::spawn(async move {
                if companion.trusts(from.ip()).await {
                    companion.answer(stream).await;
                }
                drop(permit);
            });
        }
    }

    async fn answer(self: &Arc<Self>, mut stream: TcpStream) {
        // The page and pairing are open to the tailnet; the rest of the API needs a token before its
        // body is read.
        let gate = |head: &http::Head| {
            if head.path.starts_with("/api/") && !trusted_host(head.host.as_deref()) {
                return Err(("403 Forbidden", "Open Grimoire at your Mac's Tailscale address."));
            }
            let open = !head.path.starts_with("/api/") || head.path == "/api/pair";
            if open || self.phone_for(head.authorization.as_deref()).is_some() {
                Ok(())
            } else {
                Err(("401 Unauthorized", "This phone isn't paired. Pair it again from Grimoire's settings on your Mac."))
            }
        };
        let read = tokio::time::timeout(READ_TIMEOUT, http::read_request(&mut stream, BODY_LIMIT, gate)).await;
        let request = match read {
            Ok(Ok(request)) => request,
            Ok(Err(Some((status, text)))) => {
                let body = error(text).to_string();
                let _ = tokio::time::timeout(WRITE_TIMEOUT, http::reply_with(&mut stream, status, "application/json", &json_headers(), body.as_bytes())).await;
                return;
            }
            Ok(Err(None)) | Err(_) => return,
        };
        let reply = self.route(request).await;
        let _ = tokio::time::timeout(WRITE_TIMEOUT, async {
            match reply {
                Reply::Json(status, value) => {
                    http::reply_with(&mut stream, status_line(status), "application/json", &json_headers(), value.to_string().as_bytes()).await
                }
                Reply::File(bytes, mime) => {
                    let page = mime.starts_with("text/html");
                    let mut headers = vec![("X-Content-Type-Options", "nosniff"), ("Referrer-Policy", "no-referrer")];
                    if page {
                        headers.extend([("Content-Security-Policy", CSP), ("X-Frame-Options", "DENY"), ("Cache-Control", "no-cache")]);
                    } else {
                        // Vite names assets by their content, so a file never changes under its name.
                        headers.push(("Cache-Control", "public, max-age=31536000, immutable"));
                    }
                    http::reply_with(&mut stream, "200 OK", &mime, &headers, &bytes).await
                }
            }
        })
        .await;
    }

    async fn route(self: &Arc<Self>, request: http::Request) -> Reply {
        let head = &request.head;
        if !head.path.starts_with("/api/") {
            return match (head.method.as_str(), asset_path(&head.path).and_then(|p| self.assets.get(&p))) {
                ("GET", Some((bytes, mime))) => Reply::File(bytes, mime),
                _ => Reply::Json(404, error("Not found.")),
            };
        }
        if head.path == "/api/pair" {
            if head.method != "POST" {
                return Reply::Json(404, error("Not found."));
            }
            // JSON makes a browser ask first before another site can post here.
            if !head.content_type.as_deref().is_some_and(|t| t.starts_with("application/json")) {
                return Reply::Json(400, error("Bad pairing request."));
            }
            let (status, value) = self.pair(&request.body, Instant::now());
            // A new phone keeps the port open; too many misses end the code and may close it.
            self.rebind().await;
            return Reply::Json(status, value);
        }
        let Some(phone) = self.phone_for(head.authorization.as_deref()) else {
            return Reply::Json(401, error("This phone isn't paired. Pair it again from Grimoire's settings on your Mac."));
        };
        let name = self.mark_seen(&phone).unwrap_or_else(|| "your phone".into());
        let parts: Vec<&str> = head.path["/api/".len()..].split('/').collect();
        let result = match (head.method.as_str(), parts.as_slice()) {
            ("GET", ["hello"]) => Ok(json!({ "mac": self.mac, "device": name, "version": env!("CARGO_PKG_VERSION") })),
            ("POST", ["unpair"]) => self.remove(&phone).await.map(|_| json!({})),
            ("GET", ["inbox"]) => self.inbox(),
            ("POST", ["inbox", "answer"]) => self.answer_item(&request.body),
            ("POST", ["inbox", "dismiss"]) => self.dismiss_item(&request.body),
            ("GET", ["drafts"]) => crate::drafts::list(&self.db).and_then(to_value),
            ("POST", ["drafts", id, "send"]) => self.send_draft(id, &request.body),
            ("POST", ["drafts", id, "discard"]) => self.known_id(id).and_then(|_| self.actions.close_draft(id, false, None)).and_then(to_value),
            ("GET", ["familiars"]) => self.familiars(),
            ("GET", ["familiars", id, "messages"]) => self.messages(id, &head.query),
            ("POST", ["familiars", id, "messages"]) => self.send(id, &name, &request.body),
            ("GET", ["events"]) => {
                let batch = self.events(&head.query).await;
                // The phone may have been removed while it waited.
                if !self.tokens.lock().is_ok_and(|t| t.contains_key(&phone)) {
                    return Reply::Json(401, error("This phone isn't paired. Pair it again from Grimoire's settings on your Mac."));
                }
                Ok(batch)
            }
            _ => return Reply::Json(404, error("Not found.")),
        };
        match result {
            Ok(value) => Reply::Json(200, value),
            Err(e) => Reply::Json(400, error(&e)),
        }
    }

    // ----- What a phone can do -----

    fn known_id(&self, id: &str) -> Result<(), String> {
        link::valid_id(id).then_some(()).ok_or_else(|| "That item no longer exists.".into())
    }

    /// A familiar that lives on this Mac. A linked Mac's `peer:` id, an unknown id, or anything
    /// malformed is refused.
    fn familiar_here(&self, id: &str) -> Result<(), String> {
        match link::valid_id(id) && self.db.get_agent(id)?.is_some() {
            true => Ok(()),
            false => Err("That familiar isn't on this Mac.".into()),
        }
    }

    fn inbox(&self) -> Result<Value, String> {
        let mut items: Vec<_> = self.actions.approvals().iter().map(crate::inbox::approval_item).map(phone_item).collect();
        items.extend(crate::inbox::open_questions(&self.db)?);
        to_value(items)
    }

    /// Answers an item. Approval ids come from the CLIs and may hold any character, so ids travel in
    /// the body, and `kind` says which list to look in, so a question can't reach an approval.
    fn answer_item(&self, body: &[u8]) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Answer {
            id: String,
            kind: String,
            answer: String,
        }
        let Answer { id, kind, answer } = serde_json::from_slice(body).map_err(|_| "Bad answer.")?;
        if kind == "question" {
            self.actions.answer_question(&id, &answer)?;
            return Ok(json!({}));
        }
        if !self.actions.approvals().iter().any(|r| r.id == id && r.kind == kind) {
            return Err("That request has already been answered or timed out.".into());
        }
        let decision = match answer.as_str() {
            "once" => Decision::Once,
            "deny" => Decision::Deny,
            "always" => return Err("Always allow is only on your Mac. Allow it once from here.".into()),
            _ => return Err("Answer with once or deny.".into()),
        };
        self.actions.answer_approval(&id, decision)?;
        Ok(json!({}))
    }

    fn dismiss_item(&self, body: &[u8]) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Dismiss {
            id: String,
        }
        let Dismiss { id } = serde_json::from_slice(body).map_err(|_| "Bad request.")?;
        self.actions.dismiss_question(&id).map(|_| json!({}))
    }

    fn send_draft(&self, id: &str, body: &[u8]) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Send {
            to: Vec<String>,
            #[serde(default)]
            subject: Option<String>,
            body: String,
        }
        self.known_id(id)?;
        let send = serde_json::from_slice::<Send>(body).map_err(|_| "Bad draft.")?;
        let edits = Edits { to: send.to, subject: send.subject, body: send.body };
        to_value(self.actions.close_draft(id, true, Some(edits))?)
    }

    fn familiars(&self) -> Result<Value, String> {
        let familiars: Vec<Value> = self.db.list_agents()?.iter().map(familiar_value).collect();
        let here: Vec<String> = self.actions.busy().into_iter().filter(|id| !link::is_remote(id)).collect();
        Ok(json!({ "familiars": familiars, "busy": here }))
    }

    fn messages(&self, id: &str, query: &str) -> Result<Value, String> {
        self.familiar_here(id)?;
        let number = |key| http::query_param(query, key).and_then(|v| v.parse::<i64>().ok());
        let messages = self.db.list_messages(id, number("before"), number("limit").unwrap_or(50).clamp(1, 200))?;
        Ok(Value::Array(messages.iter().map(link::message_value).collect()))
    }

    fn send(&self, id: &str, phone: &str, body: &[u8]) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Send {
            text: String,
        }
        self.familiar_here(id)?;
        let text = serde_json::from_slice::<Send>(body).map_err(|_| "Bad message.")?.text.trim().to_string();
        if text.is_empty() {
            return Err("The message is empty.".into());
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(format!("Keep a message under {MAX_TEXT_CHARS} characters."));
        }
        self.actions.send(id, text, &format!("From {phone}")).map(|m| link::message_value(&m))
    }
}

enum Reply {
    Json(u16, Value),
    File(Vec<u8>, String),
}

fn json_headers() -> [(&'static str, &'static str); 3] {
    [("Cache-Control", "no-store"), ("X-Content-Type-Options", "nosniff"), ("Referrer-Policy", "no-referrer")]
}

fn to_value(value: impl Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

// ---------- The app's side ----------

struct AppActions {
    app: tauri::AppHandle,
    db: Arc<Db>,
    runtime: Arc<crate::runtime::Runtime>,
    approvals: Arc<crate::approvals::Approvals>,
}

impl Actions for AppActions {
    fn busy(&self) -> Vec<String> {
        self.runtime.busy_agents()
    }

    fn send(&self, agent_id: &str, text: String, source: &str) -> Result<Message, String> {
        self.runtime.send(agent_id, text, Some(source))
    }

    fn approvals(&self) -> Vec<Request> {
        self.approvals.waiting()
    }

    fn answer_approval(&self, id: &str, decision: Decision) -> Result<(), String> {
        self.approvals.answer(id, decision)
    }

    fn answer_question(&self, id: &str, answer: &str) -> Result<(), String> {
        crate::inbox::answer(&self.app, &self.db, &self.runtime, id, answer)
    }

    fn dismiss_question(&self, id: &str) -> Result<(), String> {
        crate::inbox::close_unanswered(&self.app, &self.db, id)
    }

    fn close_draft(&self, id: &str, sent: bool, edits: Option<Edits>) -> Result<Draft, String> {
        crate::drafts::finish(&self.app, &self.db, &self.runtime, id, sent, edits)
    }

    fn changed(&self) {
        let _ = tauri::Emitter::emit(&self.app, CHANGED, ());
    }
}

/// Tailscale, as the link already checks it.
struct LinkNetwork;

impl Network for LinkNetwork {
    fn addresses(&self) -> BoxFuture<'_, Option<Vec<IpAddr>>> {
        Box::pin(async {
            match link::current() {
                Some(link) => link.check_tailnet().await,
                None => None,
            }
        })
    }

    fn trusts(&self, ip: IpAddr) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            match link::current() {
                Some(link) => link.trusts(ip).await,
                None => false,
            }
        })
    }
}

struct AppAssets(tauri::AppHandle);

impl Assets for AppAssets {
    fn get(&self, path: &str) -> Option<(Vec<u8>, String)> {
        self.0.asset_resolver().get(path.to_string()).map(|asset| (asset.bytes, asset.mime_type))
    }
}

static COMPANION: std::sync::OnceLock<Arc<Companion>> = std::sync::OnceLock::new();

fn running() -> Result<&'static Arc<Companion>, String> {
    COMPANION.get().ok_or_else(|| "The phone companion isn't running.".into())
}

/// The window events a phone follows, under the names the phone knows them by.
const RELAYED: [(&str, &str); 9] = [
    ("message", "message"),
    ("delta", "delta"),
    ("agent_status", "status"),
    ("inbox_changed", "inbox"),
    ("approval_requested", "inbox"),
    ("approval_closed", "inbox"),
    ("drafts_changed", "drafts"),
    ("agents_changed", "familiars"),
    ("chats_changed", "chat"),
];

/// What a window event becomes for a phone, or None when it's about a linked Mac's familiar.
fn relayed(kind: &str, payload: &str) -> Option<Value> {
    let mut value: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    match kind {
        "message" | "delta" | "status" => {
            if value["agentId"].as_str().is_none_or(link::is_remote) {
                return None;
            }
            if let Some(fields) = value.as_object_mut() {
                fields.remove("attachments");
            }
            Some(value)
        }
        // Relayed changes from linked Macs carry a payload; this Mac's own have none.
        "familiars" if !value.is_null() => None,
        // The familiar's current chat changed, so a phone showing it reloads.
        "chat" => match value["agentId"].as_str() {
            Some(agent) if value["switched"] == json!(true) && !link::is_remote(agent) => Some(json!({ "agentId": agent })),
            _ => None,
        },
        _ => Some(Value::Null),
    }
}

/// Starts the companion: follows the window events phones care about and opens the port once a
/// phone is paired or a pairing code is live.
pub fn install(
    app: &tauri::AppHandle,
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    runtime: Arc<crate::runtime::Runtime>,
    approvals: Arc<crate::approvals::Approvals>,
    mac: String,
) -> Result<(), String> {
    use tauri::Listener;
    let actions = Arc::new(AppActions { app: app.clone(), db: db.clone(), runtime, approvals });
    let companion = Companion::new(db, secrets, actions, Arc::new(LinkNetwork), Arc::new(AppAssets(app.clone())), mac, PORT)?;
    if COMPANION.set(companion.clone()).is_err() {
        return Ok(());
    }
    for (event, kind) in RELAYED {
        let companion = companion.clone();
        app.listen_any(event, move |e| {
            if let Some(payload) = relayed(kind, e.payload()) {
                companion.record(kind, payload);
            }
        });
    }
    tauri::async_runtime::spawn(companion.listen());
    Ok(())
}

#[tauri::command]
pub fn companion_status() -> Result<CompanionStatus, String> {
    running()?.status()
}

#[tauri::command]
pub async fn companion_start_pairing() -> Result<PairingCode, String> {
    running()?.start_pairing().await
}

#[tauri::command]
pub async fn companion_remove(phone_id: String) -> Result<(), String> {
    running()?.remove(&phone_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Agent, Avatar, Provider};
    use crate::drafts::DraftState;
    use crate::vault::MemoryStore;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct FakeActions {
        sent: Mutex<Vec<(String, String, String)>>,
        approvals: Mutex<Vec<Request>>,
        decided: Mutex<Vec<(String, Decision)>>,
        answered: Mutex<Vec<(String, String)>>,
        closed: Mutex<Vec<(String, bool, Option<Edits>)>>,
    }

    impl Actions for FakeActions {
        fn busy(&self) -> Vec<String> {
            vec!["ada".into(), "peer:mac:remote".into()]
        }
        fn send(&self, agent_id: &str, text: String, source: &str) -> Result<Message, String> {
            self.sent.lock().unwrap().push((agent_id.into(), text.clone(), source.into()));
            Ok(Message { id: 1, agent_id: agent_id.into(), role: "user".into(), text, status: "done".into(), source: Some(source.into()), created_at: 0, updated_at: None, attachments: Vec::new() })
        }
        fn approvals(&self) -> Vec<Request> {
            self.approvals.lock().unwrap().clone()
        }
        fn answer_approval(&self, id: &str, decision: Decision) -> Result<(), String> {
            self.decided.lock().unwrap().push((id.into(), decision));
            Ok(())
        }
        fn answer_question(&self, id: &str, answer: &str) -> Result<(), String> {
            self.answered.lock().unwrap().push((id.into(), answer.into()));
            Ok(())
        }
        fn dismiss_question(&self, _id: &str) -> Result<(), String> {
            Ok(())
        }
        fn close_draft(&self, id: &str, sent: bool, edits: Option<Edits>) -> Result<Draft, String> {
            self.closed.lock().unwrap().push((id.into(), sent, edits));
            Ok(Draft {
                id: id.into(),
                agent_id: "ada".into(),
                channel: crate::drafts::Channel::Email,
                to: vec![],
                subject: None,
                body: String::new(),
                note: None,
                state: if sent { DraftState::Sent } else { DraftState::Discarded },
                created_at: 0,
                closed_at: Some(0),
            })
        }
        fn changed(&self) {}
    }

    /// Loopback stands in for the tailnet.
    struct Loopback;

    impl Network for Loopback {
        fn addresses(&self) -> BoxFuture<'_, Option<Vec<IpAddr>>> {
            Box::pin(async { Some(vec![IpAddr::from([127, 0, 0, 1])]) })
        }
        fn trusts(&self, ip: IpAddr) -> BoxFuture<'_, bool> {
            Box::pin(async move { ip.is_loopback() })
        }
    }

    struct Page;

    impl Assets for Page {
        fn get(&self, path: &str) -> Option<(Vec<u8>, String)> {
            match path {
                "phone.html" => Some((b"<!doctype html>phone".to_vec(), "text/html".into())),
                "assets/app-1.js" => Some((b"js".to_vec(), "text/javascript".into())),
                _ => None,
            }
        }
    }

    struct Setup {
        companion: Arc<Companion>,
        actions: Arc<FakeActions>,
        secrets: Arc<MemoryStore>,
        db: Arc<Db>,
    }

    fn setup() -> Setup {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets = Arc::new(MemoryStore::new());
        let actions = Arc::new(FakeActions::default());
        let companion = Companion::new(db.clone(), secrets.clone(), actions.clone(), Arc::new(Loopback), Arc::new(Page), "Studio".into(), 0).unwrap();
        Setup { companion, actions, secrets, db }
    }

    fn familiar(db: &Db, id: &str) {
        db.insert_agent(&Agent {
            id: id.into(),
            name: id.into(),
            provider: Provider::Claude,
            model: None,
            workspace: format!("/tmp/{id}"),
            access: Access::Workspace,
            session_id: Some("secret-session".into()),
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: now_ms(),
        })
        .unwrap();
    }

    async fn start(companion: &Arc<Companion>) -> String {
        companion.start_pairing().await.unwrap();
        companion.status().unwrap().urls[0].trim_start_matches("http://").to_string()
    }

    /// Sends one request and returns the status code and body.
    async fn call(address: &str, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, String) {
        call_as(address, "127.0.0.1", method, path, token, body).await
    }

    async fn call_as(address: &str, host: &str, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, String) {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let json = if body.is_some() { "Content-Type: application/json\r\n" } else { "" };
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        let request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n{auth}{json}Content-Length: {}\r\n\r\n{body}", body.len());
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let status = response[9..12].parse().unwrap();
        let body = response.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        (status, body)
    }

    async fn paired(s: &Setup) -> (String, String) {
        let address = start(&s.companion).await;
        let code = s.companion.pairing.lock().unwrap().as_ref().unwrap().code.clone();
        let (status, body) = call(&address, "POST", "/api/pair", None, Some(json!({ "code": code.to_lowercase(), "name": "Pocket" }))).await;
        assert_eq!(status, 200, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        (address, reply["token"].as_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn nothing_listens_until_pairing_starts_and_the_port_closes_without_phones() {
        let s = setup();
        s.companion.rebind().await;
        assert!(!s.companion.status().unwrap().listening);
        let (_, token) = paired(&s).await;
        assert!(s.companion.status().unwrap().listening);
        let phone = s.companion.phone_for(Some(&format!("Bearer {token}"))).unwrap();
        // The code was used up when the phone paired, so removing the only phone closes the port.
        s.companion.remove(&phone).await.unwrap();
        assert!(!s.companion.status().unwrap().listening);
    }

    #[tokio::test]
    async fn a_code_pairs_one_phone_once_and_its_token_lives_in_the_secret_store() {
        let s = setup();
        let (address, token) = paired(&s).await;
        let phones = s.companion.phones().unwrap();
        assert_eq!(phones.len(), 1);
        assert_eq!(phones[0].name, "Pocket");
        assert_eq!(s.secrets.get(&token_key(&phones[0].id)).unwrap().as_deref(), Some(token.as_str()));
        assert!(!s.db.setting(PHONES).unwrap().unwrap().contains(&token), "the token stays out of the database");

        let (status, body) = call(&address, "GET", "/api/hello", Some(&token), None).await;
        assert_eq!(status, 200);
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["device"], "Pocket");

        // The code is used up.
        let again = call(&address, "POST", "/api/pair", None, Some(json!({ "code": "ABCD2345", "name": "Other" }))).await;
        assert_eq!(again.0, 403);
    }

    #[tokio::test]
    async fn expired_codes_and_five_misses_end_a_code() {
        let s = setup();
        s.companion.start_pairing().await.unwrap();
        let code = s.companion.pairing.lock().unwrap().as_ref().unwrap().code.clone();
        assert!(!s.companion.check_code(&code, Instant::now() + link::CODE_TTL));
        assert!(s.companion.pairing.lock().unwrap().is_none());

        s.companion.start_pairing().await.unwrap();
        let code = s.companion.pairing.lock().unwrap().as_ref().unwrap().code.clone();
        for _ in 0..link::CODE_TRIES {
            assert!(!s.companion.check_code("WRONG234", Instant::now()));
        }
        assert!(!s.companion.check_code(&code, Instant::now()));
    }

    #[tokio::test]
    async fn pairing_is_rate_limited() {
        let s = setup();
        let now = Instant::now();
        for _ in 0..link::PAIR_PER_MINUTE {
            assert_ne!(s.companion.pair(b"{}", now).0, 429);
        }
        assert_eq!(s.companion.pair(b"{}", now).0, 429);
        assert_ne!(s.companion.pair(b"{}", now + Duration::from_secs(61)).0, 429);
    }

    #[tokio::test]
    async fn the_api_needs_a_token_but_the_page_does_not() {
        let s = setup();
        let (address, token) = paired(&s).await;
        assert_eq!(call(&address, "GET", "/api/inbox", None, None).await.0, 401);
        assert_eq!(call(&address, "GET", "/api/inbox", Some(&"x".repeat(64)), None).await.0, 401);
        assert_eq!(call(&address, "GET", "/api/inbox", Some(&token), None).await.0, 200);
        let (status, body) = call(&address, "GET", "/", None, None).await;
        assert_eq!((status, body.as_str()), (200, "<!doctype html>phone"));
        assert_eq!(call(&address, "GET", "/assets/app-1.js", None, None).await.0, 200);
        for path in ["/assets/../grimoire.db", "/assets/.env", "/index.html", "/assets/"] {
            assert_eq!(call(&address, "GET", path, None, None).await.0, 404, "{path}");
        }
    }

    #[tokio::test]
    async fn removing_a_phone_revokes_its_token_at_once() {
        let s = setup();
        let (address, token) = paired(&s).await;
        let phone = s.companion.phones().unwrap()[0].id.clone();
        s.companion.start_pairing().await.unwrap(); // keeps the port open after the phone goes
        s.companion.remove(&phone).await.unwrap();
        assert_eq!(call(&address, "GET", "/api/hello", Some(&token), None).await.0, 401);
        assert!(s.secrets.get(&token_key(&phone)).unwrap().is_none());
        assert!(s.companion.phones().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_phone_can_unpair_itself() {
        let s = setup();
        let (address, token) = paired(&s).await;
        s.companion.start_pairing().await.unwrap();
        assert_eq!(call(&address, "POST", "/api/unpair", Some(&token), None).await.0, 200);
        assert_eq!(call(&address, "GET", "/api/hello", Some(&token), None).await.0, 401);
    }

    #[tokio::test]
    async fn chat_reaches_only_familiars_on_this_mac() {
        let s = setup();
        familiar(&s.db, "ada");
        let (address, token) = paired(&s).await;
        let send = |id: &'static str, body: Value| {
            let (address, token) = (address.clone(), token.clone());
            async move { call(&address, "POST", &format!("/api/familiars/{id}/messages"), Some(&token), Some(body)).await }
        };
        for id in ["nobody", "peer:mac:ada", "..", "ada%2F.."] {
            assert_eq!(send(id, json!({ "text": "hi" })).await.0, 400, "{id}");
        }
        // The phone can't name who the message is from or for beyond the path.
        assert_eq!(send("ada", json!({ "text": "hi", "agentId": "other" })).await.0, 400);
        assert_eq!(send("ada", json!({ "text": "   " })).await.0, 400);
        assert_eq!(send("ada", json!({ "text": " hello " })).await.0, 200);
        assert_eq!(*s.actions.sent.lock().unwrap(), vec![("ada".into(), "hello".into(), "From Pocket".into())]);

        let (status, body) = call(&address, "GET", "/api/familiars", Some(&token), None).await;
        assert_eq!(status, 200);
        let list: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(list["familiars"][0]["id"], "ada");
        assert!(list["familiars"][0].get("workspace").is_none() && list["familiars"][0].get("sessionId").is_none());
        assert_eq!(list["busy"], json!(["ada"]));
        assert_eq!(call(&address, "GET", "/api/familiars/peer:mac:ada/messages", Some(&token), None).await.0, 400);
        assert_eq!(call(&address, "GET", "/api/familiars/ada/messages", Some(&token), None).await.0, 200);
    }

    #[tokio::test]
    async fn approvals_answer_once_or_deny_and_questions_send_only_their_id() {
        let s = setup();
        // Codex names its approvals after the CLI's request id, quotes and all.
        let approval = r#"codex-1-"0""#;
        s.actions.approvals.lock().unwrap().push(Request {
            id: approval.into(),
            agent_id: "ada".into(),
            kind: "command",
            title: "Ada wants to run a command".into(),
            detail: "ls".into(),
            can_remember: true,
        });
        let (address, token) = paired(&s).await;
        let (_, body) = call(&address, "GET", "/api/inbox", Some(&token), None).await;
        let items: Value = serde_json::from_str(&body).unwrap();
        let values: Vec<&str> = items[0]["options"].as_array().unwrap().iter().map(|o| o["value"].as_str().unwrap()).collect();
        assert_eq!(values, ["once", "deny"]);

        let answer = |body: Value| {
            let (address, token) = (address.clone(), token.clone());
            async move { call(&address, "POST", "/api/inbox/answer", Some(&token), Some(body)).await.0 }
        };
        assert_eq!(answer(json!({ "id": approval, "kind": "command", "answer": "always" })).await, 400);
        assert_eq!(answer(json!({ "id": approval, "kind": "vault", "answer": "once" })).await, 400, "the kind has to match");
        assert_eq!(answer(json!({ "id": approval, "kind": "command", "answer": "once" })).await, 200);
        assert_eq!(*s.actions.decided.lock().unwrap(), vec![(approval.to_string(), Decision::Once)]);

        // A question answered "once" stays a question, even with an approval's id.
        assert_eq!(answer(json!({ "id": approval, "kind": "question", "answer": "once" })).await, 200);
        assert_eq!(answer(json!({ "id": "q-1", "kind": "question", "answer": "Ship it", "agentId": "other" })).await, 400);
        assert_eq!(answer(json!({ "id": "q-1", "kind": "question", "answer": "Ship it" })).await, 200);
        assert_eq!(*s.actions.answered.lock().unwrap(), vec![(approval.to_string(), "once".to_string()), ("q-1".to_string(), "Ship it".to_string())]);
        assert_eq!(s.actions.decided.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_api_refuses_foreign_hosts_and_pairing_needs_json() {
        let s = setup();
        let (address, token) = paired(&s).await;
        for host in ["evil.example.com", "evil.example.com:47621", "", "100.64.0.1.evil.com"] {
            assert_eq!(call_as(&address, host, "GET", "/api/hello", Some(&token), None).await.0, 403, "{host}");
        }
        for host in ["100.101.102.103:47621", "[fd7a:115c:a1e0::1]:47621", "studio.tail1234.ts.net:47621", "studio:47621"] {
            assert_eq!(call_as(&address, host, "GET", "/api/hello", Some(&token), None).await.0, 200, "{host}");
        }
        s.companion.start_pairing().await.unwrap();
        let mut stream = TcpStream::connect(&address).await.unwrap();
        let body = r#"{"code":"ABCD2345","name":"x"}"#;
        let request = format!("POST /api/pair HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert_eq!(s.companion.pairing.lock().unwrap().as_ref().unwrap().misses, 0, "a refused request doesn't spend a try");
    }

    #[tokio::test]
    async fn a_poll_waiting_when_its_phone_is_removed_gets_401() {
        let s = setup();
        let (address, token) = paired(&s).await;
        s.companion.start_pairing().await.unwrap();
        let (_, body) = call(&address, "GET", "/api/events", Some(&token), None).await;
        let seq = serde_json::from_str::<Value>(&body).unwrap()["seq"].as_u64().unwrap();
        let poll = tokio::spawn({
            let (address, token) = (address.clone(), token.clone());
            async move { call(&address, "GET", &format!("/api/events?after={seq}&wait=1"), Some(&token), None).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let phone = s.companion.phones().unwrap()[0].id.clone();
        s.companion.remove(&phone).await.unwrap();
        assert_eq!(poll.await.unwrap().0, 401);
    }

    #[tokio::test]
    async fn the_port_closes_when_a_code_runs_out_of_tries() {
        let s = setup();
        let address = start(&s.companion).await;
        for _ in 0..link::CODE_TRIES {
            call(&address, "POST", "/api/pair", None, Some(json!({ "code": "WRONG234", "name": "x" }))).await;
        }
        assert!(!s.companion.status().unwrap().listening);
    }

    #[tokio::test(start_paused = true)]
    async fn the_port_closes_when_a_code_expires() {
        let s = setup();
        s.companion.start_pairing().await.unwrap();
        assert!(s.companion.status().unwrap().listening);
        tokio::time::sleep(link::CODE_TTL + Duration::from_secs(1)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!s.companion.status().unwrap().listening);
    }

    #[tokio::test]
    async fn drafts_send_with_edits_or_discard() {
        let s = setup();
        let (address, token) = paired(&s).await;
        let body = json!({ "to": ["a@example.com"], "subject": "Hi", "body": "Edited" });
        assert_eq!(call(&address, "POST", "/api/drafts/d-1/send", Some(&token), Some(body)).await.0, 200);
        assert_eq!(call(&address, "POST", "/api/drafts/d-2/discard", Some(&token), None).await.0, 200);
        let extra = json!({ "to": [], "body": "x", "agentId": "other" });
        assert_eq!(call(&address, "POST", "/api/drafts/d-3/send", Some(&token), Some(extra)).await.0, 400);
        let closed = s.actions.closed.lock().unwrap();
        assert_eq!(closed.len(), 2);
        assert_eq!(closed[0].2, Some(Edits { to: vec!["a@example.com".into()], subject: Some("Hi".into()), body: "Edited".into() }));
        assert_eq!((closed[1].1, &closed[1].2), (false, &None));
    }

    #[tokio::test]
    async fn events_reach_a_long_poll_only_once_a_phone_is_paired() {
        let s = setup();
        s.companion.record("inbox", Value::Null);
        assert_eq!(s.companion.events.head(), 0);
        let (address, token) = paired(&s).await;
        let (_, body) = call(&address, "GET", "/api/events", Some(&token), None).await;
        let seq = serde_json::from_str::<Value>(&body).unwrap()["seq"].as_u64().unwrap();
        let poll = tokio::spawn({
            let (address, token) = (address.clone(), token.clone());
            async move { call(&address, "GET", &format!("/api/events?after={seq}&wait=5"), Some(&token), None).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.companion.record("drafts", Value::Null);
        let (_, body) = poll.await.unwrap();
        let batch: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(batch["events"], json!([{ "kind": "drafts", "payload": null }]));
    }

    #[test]
    fn linked_macs_familiars_and_attachments_stay_off_the_phone() {
        assert_eq!(relayed("message", r#"{"agentId":"peer:m:a","text":"x"}"#), None);
        assert_eq!(relayed("message", r#"{"agentId":"ada","text":"x","attachments":[{"path":"/x"}]}"#), Some(json!({ "agentId": "ada", "text": "x" })));
        assert_eq!(relayed("familiars", r#"{"peerId":"m"}"#), None);
        assert_eq!(relayed("familiars", "null"), Some(Value::Null));
        assert_eq!(relayed("inbox", r#"{"id":"r"}"#), Some(Value::Null));
        assert_eq!(relayed("chat", r#"{"agentId":"ada","switched":true}"#), Some(json!({ "agentId": "ada" })));
        assert_eq!(relayed("chat", r#"{"agentId":"ada","switched":false}"#), None);
        assert_eq!(relayed("chat", r#"{"agentId":"peer:m:a","switched":true}"#), None);
    }
}
