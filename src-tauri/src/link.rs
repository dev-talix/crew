//! Linked Macs. Two Macs running Grimoire pair over the owner's Tailscale tailnet and show each
//! other's familiars. A familiar lives only on its home Mac; other Macs list it, read its chat, send
//! it messages, and stop its turns through this Mac's link server. Nothing else crosses: no vault,
//! approvals, settings, memory, or tools.
//!
//! The server listens on port 47620 on this Mac's Tailscale addresses only, and drops connections
//! from outside the tailnet. WireGuard encrypts the traffic, so the protocol is plain HTTP/1.1 JSON.
//! Pairing trades a one-time code for a token each way; the tokens live in the Keychain.

use crate::db::{Access, Avatar, Db, Message, Provider};
use crate::http;
use crate::vault::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore};
use tokio::task::AbortHandle;
use tokio::time::Instant;

pub const PORT: u16 = 47620;
pub const REMOTE_PREFIX: &str = "peer:";
const BODY_LIMIT: usize = 1024 * 1024;
const RESPONSE_LIMIT: u64 = 32 * 1024 * 1024;
const RING: usize = 1000;
const LONG_POLL: Duration = Duration::from_secs(25);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
const BIND_RETRY: Duration = Duration::from_secs(60);
const BACKOFF_MIN: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const CODE_TTL: Duration = Duration::from_secs(10 * 60);
const CODE_TRIES: u8 = 5;
const CODE_LEN: usize = 8;
/// No 0, O, 1, or I, so a code read aloud can't be misheard.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const PAIR_PER_MINUTE: usize = 20;
const MAX_CONNECTIONS: usize = 64;
const INSTALL_ID: &str = "link_install_id";

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a linked Mac may do with this Mac's familiars. The app runs it through the runtime.
pub trait Local: Send + Sync {
    fn send(&self, agent_id: &str, text: String, source: &str) -> Result<Message, String>;
    fn stop<'a>(&'a self, agent_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
    fn busy(&self) -> Vec<String>;
    /// Makes a familiar here and returns it as `list_agents` shows it.
    fn create(&self, input: NewFamiliar) -> BoxFuture<'_, Result<Value, String>>;
}

/// Where relayed events go: the app's windows, or a list in tests.
pub trait Ui: Send + Sync {
    fn event(&self, name: &str, payload: Value);
}

impl Ui for tauri::AppHandle {
    fn event(&self, name: &str, payload: Value) {
        let _ = tauri::Emitter::emit(self, name, payload);
    }
}

/// A familiar a linked Mac asks this Mac to make. It has no SSH host field, so a peer can only make
/// familiars that run here.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewFamiliar {
    pub name: String,
    pub provider: Provider,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    pub access: Access,
    pub avatar: Avatar,
    #[serde(default)]
    pub workspace: Option<String>,
}

// ---------- Ids and addresses ----------

/// An install id or a familiar id: a uuid, or anything as plain, so it can't reach into a path.
fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn is_remote(id: &str) -> bool {
    id.starts_with(REMOTE_PREFIX)
}

pub fn remote_id(peer: &str, agent: &str) -> String {
    format!("{REMOTE_PREFIX}{peer}:{agent}")
}

/// The Mac and the familiar in `peer:<install id>:<familiar id>`.
pub fn parse_remote(id: &str) -> Option<(&str, &str)> {
    let (peer, agent) = id.strip_prefix(REMOTE_PREFIX)?.split_once(':')?;
    (valid_id(peer) && valid_id(agent)).then_some((peer, agent))
}

/// Tailscale gives each device an address in 100.64.0.0/10 and one in fd7a:115c:a1e0::/48.
pub fn is_tailnet(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (b & 0xc0) == 64
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// This Mac's addresses that `allow` accepts, IPv4 first.
fn own_addresses(allow: fn(IpAddr) -> bool) -> Vec<IpAddr> {
    let mut found = Vec::new();
    // SAFETY: getifaddrs fills a list that stays valid until freeifaddrs. Each sockaddr is read as the
    // type its family says.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return found;
        }
        let mut at = head;
        while !at.is_null() {
            let entry = &*at;
            if !entry.ifa_addr.is_null() {
                let ip = match i32::from((*entry.ifa_addr).sa_family) {
                    libc::AF_INET => {
                        let sin = &*(entry.ifa_addr as *const libc::sockaddr_in);
                        Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))))
                    }
                    libc::AF_INET6 => {
                        let sin6 = &*(entry.ifa_addr as *const libc::sockaddr_in6);
                        Some(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)))
                    }
                    _ => None,
                };
                if let Some(ip) = ip.filter(|ip| allow(*ip) && !found.contains(ip)) {
                    found.push(ip);
                }
            }
            at = entry.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    found.sort_by_key(|ip| ip.is_ipv6());
    found
}

/// What the person typed for the other Mac: a Tailscale IP or MagicDNS name, maybe with a port.
fn clean_address(address: &str) -> Result<String, String> {
    let address = address.trim();
    let ok = (1..=255).contains(&address.len())
        && !address.starts_with('-')
        && address.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]_".contains(c));
    if ok {
        Ok(address.to_string())
    } else {
        Err("Write the other Mac's Tailscale name or address, like studio or 100.101.102.103.".into())
    }
}

fn clean_mac_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= 80 && !name.chars().any(char::is_control)).then(|| name.to_string())
}

fn valid_token(token: &str) -> bool {
    (32..=128).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// 244 random bits from the system's generator.
fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn new_code() -> String {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    // Bytes 6 and 8 carry the uuid's version and variant; the rest are random.
    [0, 1, 2, 3, 4, 5, 10, 11].iter().map(|&i| CODE_ALPHABET[(bytes[i] % 32) as usize] as char).collect()
}

/// A code as typed, without spaces or dashes and in capitals.
fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// The Mac's name as Sharing settings shows it, or its host name.
fn mac_name() -> String {
    let scutil = std::process::Command::new("/usr/sbin/scutil").args(["--get", "ComputerName"]).output();
    if let Some(name) = scutil.ok().filter(|o| o.status.success()).and_then(|o| clean_mac_name(&String::from_utf8_lossy(&o.stdout))) {
        return name;
    }
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes into `buf`.
    let host = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) == 0 }
        .then(|| String::from_utf8_lossy(buf.split(|&b| b == 0).next().unwrap_or_default()).into_owned());
    host.and_then(|h| clean_mac_name(&h)).unwrap_or_else(|| "This Mac".into())
}

fn send_key(peer: &str) -> String {
    format!("link:send:{peer}")
}

fn expect_key(peer: &str) -> String {
    format!("link:expect:{peer}")
}

/// A message as a linked Mac sees it. Attachments stay behind: their files are on this Mac.
pub fn message_value(message: &Message) -> Value {
    let mut value = serde_json::to_value(message).unwrap_or_default();
    if let Some(fields) = value.as_object_mut() {
        fields.remove("attachments");
    }
    value
}

// ---------- Peers table ----------

struct PeerRow {
    id: String,
    name: String,
    address: String,
    last_seen: Option<i64>,
    version: Option<String>,
    familiars: Vec<Value>,
    created_at: i64,
}

fn peer_rows(db: &Db) -> Result<Vec<PeerRow>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached("SELECT id, name, address, last_seen, version, familiars, created_at FROM link_peers ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| {
            Ok(PeerRow {
                id: r.get(0)?,
                name: r.get(1)?,
                address: r.get(2)?,
                last_seen: r.get(3)?,
                version: r.get(4)?,
                familiars: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                created_at: r.get(6)?,
            })
        })?;
        rows.collect()
    })
}

fn peer_row(db: &Db, id: &str) -> Result<Option<PeerRow>, String> {
    Ok(peer_rows(db)?.into_iter().find(|p| p.id == id))
}

fn save_peer(db: &Db, id: &str, name: &str, address: &str) -> Result<(), String> {
    db.with(|c| {
        c.execute(
            "INSERT INTO link_peers (id, name, address, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = ?2, address = ?3, familiars = '[]'",
            rusqlite::params![id, name, address, crate::db::now_ms()],
        )
        .map(|_| ())
    })
}

fn mark_seen(db: &Db, id: &str, name: Option<&str>, version: Option<&str>) {
    let _ = db.with(|c| {
        c.execute(
            "UPDATE link_peers SET last_seen = ?2, name = COALESCE(?3, name), version = COALESCE(?4, version) WHERE id = ?1",
            rusqlite::params![id, crate::db::now_ms(), name, version],
        )
    });
}

fn cache_familiars(db: &Db, id: &str, familiars: &[Value]) {
    let text = serde_json::to_string(familiars).unwrap_or_else(|_| "[]".into());
    let _ = db.with(|c| c.execute("UPDATE link_peers SET familiars = ?2 WHERE id = ?1", rusqlite::params![id, text]));
}

fn delete_peer(db: &Db, id: &str) -> Result<(), String> {
    db.with(|c| c.execute("DELETE FROM link_peers WHERE id = ?1", rusqlite::params![id]).map(|_| ()))
}

fn install_id(db: &Db) -> Result<String, String> {
    if let Some(id) = db.setting(INSTALL_ID)?.filter(|id| valid_id(id)) {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    db.set_setting(INSTALL_ID, &id)?;
    Ok(id)
}

// ---------- Event log ----------

/// Recent events about this Mac's familiars, numbered, for linked Macs to long-poll.
struct EventLog {
    epoch: String,
    ring: Mutex<(u64, VecDeque<(u64, Value)>)>,
    notify: Notify,
}

struct Batch {
    seq: u64,
    events: Vec<Value>,
    /// The caller missed events, or asked about a sequence this log never had.
    reset: bool,
}

impl EventLog {
    fn new() -> Self {
        EventLog { epoch: uuid::Uuid::new_v4().simple().to_string(), ring: Mutex::new((0, VecDeque::new())), notify: Notify::new() }
    }

    fn push(&self, kind: &str, payload: Value) {
        if let Ok(mut ring) = self.ring.lock() {
            ring.0 += 1;
            let seq = ring.0;
            ring.1.push_back((seq, json!({ "kind": kind, "payload": payload })));
            if ring.1.len() > RING {
                ring.1.pop_front();
            }
        }
        self.notify.notify_waiters();
    }

    fn head(&self) -> u64 {
        self.ring.lock().map(|r| r.0).unwrap_or(0)
    }

    /// The events after `after`, or None when there are none yet.
    fn after(&self, after: u64) -> Option<Batch> {
        let ring = self.ring.lock().ok()?;
        let (head, items) = (&ring.0, &ring.1);
        if after > *head {
            return Some(Batch { seq: *head, events: Vec::new(), reset: true });
        }
        if after == *head {
            return None;
        }
        let oldest = items.front().map_or(*head + 1, |(seq, _)| *seq);
        let events = items.iter().filter(|(seq, _)| *seq > after).map(|(_, e)| e.clone()).collect();
        Some(Batch { seq: *head, events, reset: after + 1 < oldest })
    }

    /// Waits up to `wait` for events after `after`.
    async fn wait(&self, after: u64, wait: Duration) -> Batch {
        let deadline = Instant::now() + wait;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(batch) = self.after(after) {
                return batch;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Batch { seq: after, events: Vec::new(), reset: false };
            }
        }
    }
}

// ---------- The link ----------

struct Pairing {
    code: String,
    expires: Instant,
    wrong: u8,
}

struct Tokens {
    /// What this Mac sends to the peer.
    send: String,
    /// What the peer must send to this Mac.
    expect: String,
}

#[derive(Default)]
struct Live {
    online: bool,
    busy: HashSet<String>,
    relay: Option<AbortHandle>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThisMac {
    pub install_id: String,
    pub name: String,
    pub addresses: Vec<String>,
    pub listening: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub address: String,
    pub online: bool,
    pub last_seen: Option<i64>,
    pub version: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    pub this_mac: ThisMac,
    pub peers: Vec<Peer>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    pub code: String,
    pub expires_at: i64,
    pub name: String,
    pub addresses: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    code: String,
    install_id: String,
    name: String,
    address: String,
    token_for_you: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairReply {
    install_id: String,
    name: String,
    token_for_me: String,
}

#[derive(Debug)]
enum CallError {
    /// The other Mac didn't answer.
    Offline,
    /// It answered with an error.
    Refused(u16, String),
}

pub struct Link {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    local: Arc<dyn Local>,
    ui: Arc<dyn Ui>,
    /// Which addresses may connect, and which this Mac connects to. Tailscale's in the app.
    allow: fn(IpAddr) -> bool,
    install_id: String,
    name: String,
    events: EventLog,
    pairing: Mutex<Option<Pairing>>,
    pair_tries: Mutex<VecDeque<Instant>>,
    tokens: Mutex<HashMap<String, Tokens>>,
    live: Mutex<HashMap<String, Live>>,
    server: Mutex<ThisMac>,
    connections: Arc<Semaphore>,
}

static LINK: OnceLock<Arc<Link>> = OnceLock::new();

fn running() -> Result<&'static Arc<Link>, String> {
    LINK.get().ok_or_else(|| "Linked Macs aren't running.".into())
}

impl Link {
    fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>, local: Arc<dyn Local>, ui: Arc<dyn Ui>, allow: fn(IpAddr) -> bool, name: String) -> Result<Arc<Self>, String> {
        let install_id = install_id(&db)?;
        let mut tokens = HashMap::new();
        for peer in peer_rows(&db)? {
            match (secrets.get(&send_key(&peer.id)), secrets.get(&expect_key(&peer.id))) {
                (Ok(Some(send)), Ok(Some(expect))) => {
                    tokens.insert(peer.id, Tokens { send, expect });
                }
                _ => eprintln!("The Keychain has no link tokens for {}. Pair it again.", peer.name),
            }
        }
        let server = ThisMac { install_id: install_id.clone(), name: name.clone(), ..Default::default() };
        Ok(Arc::new(Link {
            db,
            secrets,
            local,
            ui,
            allow,
            install_id,
            name,
            events: EventLog::new(),
            pairing: Mutex::new(None),
            pair_tries: Mutex::new(VecDeque::new()),
            tokens: Mutex::new(tokens),
            live: Mutex::new(HashMap::new()),
            server: Mutex::new(server),
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }))
    }

    fn has_peers(&self) -> bool {
        self.tokens.lock().is_ok_and(|t| !t.is_empty())
    }

    fn online(&self, peer: &str) -> bool {
        self.live.lock().is_ok_and(|l| l.get(peer).is_some_and(|p| p.online))
    }

    fn remote_changed(&self, peer: &str) {
        // A payload marks it as relayed, so this Mac doesn't pass it back to its own peers.
        self.ui.event("agents_changed", json!({ "peerId": peer }));
    }

    pub fn status(&self) -> Result<LinkStatus, String> {
        let this_mac = self.server.lock().map_err(|_| "link state poisoned")?.clone();
        let peers = peer_rows(&self.db)?.into_iter().map(|row| self.peer_view(row)).collect();
        Ok(LinkStatus { this_mac, peers })
    }

    fn peer_view(&self, row: PeerRow) -> Peer {
        Peer { online: self.online(&row.id), id: row.id, name: row.name, address: row.address, last_seen: row.last_seen, version: row.version }
    }

    // ----- Pairing -----

    pub fn start_pairing(&self) -> Result<PairingCode, String> {
        let server = self.server.lock().map_err(|_| "link state poisoned")?.clone();
        if !server.listening {
            return Err(server.error.unwrap_or_else(|| "This Mac isn't on a tailnet yet. Connect Tailscale, then try again.".into()));
        }
        let code = new_code();
        let expires_at = crate::db::now_ms() + CODE_TTL.as_millis() as i64;
        *self.pairing.lock().map_err(|_| "link state poisoned")? = Some(Pairing { code: code.clone(), expires: Instant::now() + CODE_TTL, wrong: 0 });
        Ok(PairingCode { code, expires_at, name: self.name.clone(), addresses: server.addresses })
    }

    /// Whether `given` is the active code. A match uses it up, and so do five misses.
    fn check_code(&self, given: &str, now: Instant) -> bool {
        let Ok(mut slot) = self.pairing.lock() else { return false };
        let Some(pairing) = slot.as_mut() else { return false };
        if now >= pairing.expires {
            *slot = None;
            return false;
        }
        if http::same_secret(&pairing.code, &normalize_code(given)) {
            *slot = None;
            return true;
        }
        pairing.wrong += 1;
        if pairing.wrong >= CODE_TRIES {
            *slot = None;
        }
        false
    }

    /// At most PAIR_PER_MINUTE pairing requests a minute, from anyone.
    fn pair_allowed(&self, now: Instant) -> bool {
        let Ok(mut tries) = self.pair_tries.lock() else { return false };
        while tries.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60)) {
            tries.pop_front();
        }
        if tries.len() >= PAIR_PER_MINUTE {
            return false;
        }
        tries.push_back(now);
        true
    }

    fn add_peer(self: &Arc<Self>, id: &str, name: &str, address: &str, tokens: Tokens) -> Result<Peer, String> {
        self.secrets.set(&send_key(id), &tokens.send)?;
        self.secrets.set(&expect_key(id), &tokens.expect)?;
        save_peer(&self.db, id, name, address)?;
        self.tokens.lock().map_err(|_| "link state poisoned")?.insert(id.to_string(), tokens);
        self.start_relay(id);
        self.ui.event("links_changed", Value::Null);
        let row = peer_row(&self.db, id)?.ok_or("The link wasn't saved.")?;
        Ok(self.peer_view(row))
    }

    fn answer_pair(self: &Arc<Self>, body: &[u8], now: Instant) -> (u16, Value) {
        if !self.pair_allowed(now) {
            return (429, error("Too many pairing tries. Wait a minute and try again."));
        }
        let Ok(request) = serde_json::from_slice::<PairRequest>(body) else {
            return (400, error("Bad pairing request."));
        };
        let name = clean_mac_name(&request.name);
        let address = clean_address(&request.address).ok();
        let (Some(name), Some(address)) = (name, address) else {
            return (400, error("Bad pairing request."));
        };
        if !valid_id(&request.install_id) || request.install_id == self.install_id || !valid_token(&request.token_for_you) {
            return (400, error("Bad pairing request."));
        }
        if !self.check_code(&request.code, now) {
            return (403, error("That code is wrong or has expired. Make a new one on the other Mac."));
        }
        let token_for_me = new_token();
        let tokens = Tokens { send: request.token_for_you, expect: token_for_me.clone() };
        match self.add_peer(&request.install_id, &name, &address, tokens) {
            Ok(_) => (200, json!(PairReply { install_id: self.install_id.clone(), name: self.name.clone(), token_for_me })),
            Err(e) => (500, error(&e)),
        }
    }

    pub async fn pair(self: &Arc<Self>, address: &str, code: &str) -> Result<Peer, String> {
        let address = clean_address(address)?;
        let code = normalize_code(code);
        if code.len() != CODE_LEN || !code.bytes().all(|b| CODE_ALPHABET.contains(&b)) {
            return Err("A pairing code is 8 letters and digits, like K7QM3XRP.".into());
        }
        let mine = self.server.lock().map_err(|_| "link state poisoned")?.addresses.first().cloned();
        let mine = mine.ok_or("This Mac isn't on a tailnet yet. Connect Tailscale, then try again.")?;
        let token_for_you = new_token();
        let body = json!({ "code": code, "installId": self.install_id, "name": self.name, "address": mine, "tokenForYou": token_for_you });
        let reply = match self.call(&address, None, "POST", "/v1/pair", Some(&body), CALL_TIMEOUT).await {
            Ok(reply) => reply,
            Err(CallError::Offline) => return Err(format!("Couldn't reach Grimoire at {address}. Check that it's open there and on the same tailnet.")),
            Err(CallError::Refused(_, why)) => return Err(why),
        };
        let reply: PairReply = serde_json::from_value(reply).map_err(|_| "The other Mac sent an odd answer.")?;
        let name = clean_mac_name(&reply.name);
        let ok = valid_id(&reply.install_id) && reply.install_id != self.install_id && valid_token(&reply.token_for_me);
        let Some(name) = name.filter(|_| ok) else { return Err("The other Mac sent an odd answer.".into()) };
        self.add_peer(&reply.install_id, &name, &address, Tokens { send: reply.token_for_me, expect: token_for_you })
    }

    pub async fn unlink(&self, peer: &str) -> Result<(), String> {
        if !valid_id(peer) {
            return Err("Unknown Mac.".into());
        }
        // Best effort: an offline Mac drops its side when it next finds the token refused.
        let _ = self.peer_call(peer, "POST", "/v1/unlink", None, Duration::from_secs(5)).await;
        self.forget(peer)
    }

    /// Drops a peer: its row, its tokens, its relay, and its familiars from the list.
    fn forget(&self, peer: &str) -> Result<(), String> {
        if let Some(relay) = self.live.lock().ok().and_then(|mut l| l.remove(peer)).and_then(|l| l.relay) {
            relay.abort();
        }
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.remove(peer);
        }
        let deleted = delete_peer(&self.db, peer);
        let keychain = self.secrets.delete(&send_key(peer)).and(self.secrets.delete(&expect_key(peer)));
        self.ui.event("links_changed", Value::Null);
        self.remote_changed(peer);
        deleted.and(keychain)
    }

    // ----- Server -----

    /// The peer whose token this is.
    fn peer_for(&self, authorization: Option<&str>) -> Option<String> {
        let token = authorization?.strip_prefix("Bearer ")?.trim();
        let tokens = self.tokens.lock().ok()?;
        // Compare with every peer so the time doesn't say which one matched.
        tokens.iter().fold(None, |found, (id, t)| if http::same_secret(&t.expect, token) { Some(id.clone()) } else { found })
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
            if !(self.allow)(from.ip()) {
                continue;
            }
            let Ok(permit) = self.connections.clone().try_acquire_owned() else { continue };
            let link = self.clone();
            tokio::spawn(async move {
                link.answer(stream).await;
                drop(permit);
            });
        }
    }

    async fn answer(self: &Arc<Self>, mut stream: TcpStream) {
        // Only pairing is open; anything else needs a token before its body is read.
        let gate = |head: &http::Head| {
            if head.path == "/v1/pair" || self.peer_for(head.authorization.as_deref()).is_some() {
                Ok(())
            } else {
                Err(("401 Unauthorized", "This Mac isn't linked. Pair it again."))
            }
        };
        let read = tokio::time::timeout(READ_TIMEOUT, http::read_request(&mut stream, BODY_LIMIT, gate)).await;
        let (status, value) = match read {
            Ok(Ok(request)) => self.route(request).await,
            Ok(Err(Some((status, text)))) => {
                return http::reply(&mut stream, status, "application/json", error(text).to_string().as_bytes()).await;
            }
            Ok(Err(None)) | Err(_) => return,
        };
        http::reply(&mut stream, status_line(status), "application/json", value.to_string().as_bytes()).await;
    }

    async fn route(self: &Arc<Self>, request: http::Request) -> (u16, Value) {
        let head = &request.head;
        if head.path == "/v1/pair" {
            return if head.method == "POST" { self.answer_pair(&request.body, Instant::now()) } else { (404, error("Not found.")) };
        }
        let Some(peer) = self.peer_for(head.authorization.as_deref()) else {
            return (401, error("This Mac isn't linked. Pair it again."));
        };
        let Some(rest) = head.path.strip_prefix("/v1/") else { return (404, error("Not found.")) };
        let parts: Vec<&str> = rest.split('/').collect();
        let result = match (head.method.as_str(), parts.as_slice()) {
            ("GET", ["hello"]) => Ok(json!({ "installId": self.install_id, "name": self.name, "version": env!("CARGO_PKG_VERSION") })),
            ("POST", ["unlink"]) => self.forget(&peer).map(|_| json!({})),
            ("GET", ["familiars"]) => self.familiars_here(),
            ("POST", ["familiars"]) => match serde_json::from_slice::<NewFamiliar>(&request.body) {
                Ok(input) => self.local.create(input).await,
                Err(e) => Err(format!("Bad familiar: {e}")),
            },
            ("GET", ["familiars", id, "messages"]) => self.messages_here(id, &head.query),
            ("POST", ["familiars", id, "messages"]) => self.send_here(&peer, id, &request.body),
            ("POST", ["familiars", id, "stop"]) => match self.familiar_here(id) {
                Ok(()) => self.local.stop(id).await.map(|_| json!({})),
                Err(e) => Err(e),
            },
            ("GET", ["events"]) => Ok(self.events_here(&head.query).await),
            _ => return (404, error("Not found.")),
        };
        match result {
            Ok(value) => (200, value),
            Err(e) => (400, error(&e)),
        }
    }

    fn familiar_here(&self, id: &str) -> Result<(), String> {
        match valid_id(id) && self.db.get_agent(id)?.is_some() {
            true => Ok(()),
            false => Err("That familiar no longer exists.".into()),
        }
    }

    /// This Mac's familiars, SSH-hosted ones included since they live here. Never a peer's.
    fn familiars_here(&self) -> Result<Value, String> {
        let mut familiars = serde_json::to_value(crate::remote::with_status(&self.db, self.db.list_agents()?)?).map_err(|e| e.to_string())?;
        // The CLI's session id is this Mac's business.
        for familiar in familiars.as_array_mut().into_iter().flatten() {
            familiar["sessionId"] = Value::Null;
        }
        Ok(json!({ "familiars": familiars, "busy": self.local.busy() }))
    }

    fn messages_here(&self, id: &str, query: &str) -> Result<Value, String> {
        self.familiar_here(id)?;
        let number = |key| http::query_param(query, key).and_then(|v| v.parse::<i64>().ok());
        let messages = self.db.list_messages(id, number("before"), number("limit").unwrap_or(100).clamp(1, 500))?;
        Ok(Value::Array(messages.iter().map(message_value).collect()))
    }

    fn send_here(&self, peer: &str, id: &str, body: &[u8]) -> Result<Value, String> {
        self.familiar_here(id)?;
        #[derive(Deserialize)]
        struct Send {
            text: String,
        }
        let text = serde_json::from_slice::<Send>(body).map_err(|_| "Bad message.")?.text.trim().to_string();
        if text.is_empty() {
            return Err("The message is empty.".into());
        }
        let from = peer_row(&self.db, peer)?.map_or_else(|| "a linked Mac".into(), |p| p.name);
        self.local.send(id, text, &format!("From {from}")).map(|m| message_value(&m))
    }

    async fn events_here(&self, query: &str) -> Value {
        let Some(after) = http::query_param(query, "after").and_then(|v| v.parse::<u64>().ok()) else {
            return json!({ "seq": self.events.head(), "epoch": self.events.epoch, "events": [], "reset": false });
        };
        let wait = http::query_param(query, "wait").and_then(|v| v.parse::<u64>().ok()).map_or(LONG_POLL, Duration::from_secs).min(LONG_POLL);
        let batch = self.events.wait(after, wait).await;
        json!({ "seq": batch.seq, "epoch": self.events.epoch, "events": batch.events, "reset": batch.reset })
    }

    /// Binds the link port on this Mac's tailnet addresses, and checks again every minute.
    async fn listen(self: Arc<Self>) {
        let mut bound: HashMap<IpAddr, AbortHandle> = HashMap::new();
        loop {
            let found = own_addresses(self.allow);
            bound.retain(|ip, server| {
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
                match TcpListener::bind((*ip, PORT)).await {
                    Ok(listener) => {
                        bound.insert(*ip, tokio::spawn(self.clone().serve(listener)).abort_handle());
                    }
                    Err(e) => failures.push(format!("{ip}: {e}")),
                }
            }
            let error = if found.is_empty() {
                Some("Tailscale isn't connected on this Mac, so other Macs can't reach it. Grimoire checks again every minute.".to_string())
            } else if !failures.is_empty() {
                Some(format!("Grimoire couldn't listen on port {PORT} at {}.", failures.join(", ")))
            } else {
                None
            };
            let addresses: Vec<String> = found.iter().filter(|ip| bound.contains_key(ip)).map(IpAddr::to_string).collect();
            let changed = match self.server.lock() {
                Ok(mut server) => {
                    let changed = server.addresses != addresses || server.error != error;
                    server.listening = !addresses.is_empty();
                    server.addresses = addresses;
                    server.error = error;
                    changed
                }
                Err(_) => false,
            };
            if changed {
                self.ui.event("links_changed", Value::Null);
            }
            tokio::time::sleep(BIND_RETRY).await;
        }
    }

    // ----- Client -----

    async fn call(&self, address: &str, token: Option<&str>, method: &str, target: &str, body: Option<&Value>, timeout: Duration) -> Result<Value, CallError> {
        tokio::time::timeout(timeout, self.request(address, token, method, target, body)).await.unwrap_or(Err(CallError::Offline))
    }

    async fn request(&self, address: &str, token: Option<&str>, method: &str, target: &str, body: Option<&Value>) -> Result<Value, CallError> {
        let to = resolve(address, self.allow).await.ok_or(CallError::Offline)?;
        let mut stream = TcpStream::connect(to).await.map_err(|_| CallError::Offline)?;
        let body = body.map(Value::to_string).unwrap_or_default();
        let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        let head = format!(
            "{method} {target} HTTP/1.1\r\nHost: {address}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.map_err(|_| CallError::Offline)?;
        stream.write_all(body.as_bytes()).await.map_err(|_| CallError::Offline)?;
        let mut response = Vec::new();
        (&mut stream).take(RESPONSE_LIMIT).read_to_end(&mut response).await.map_err(|_| CallError::Offline)?;
        let end = response.windows(4).position(|w| w == b"\r\n\r\n").ok_or(CallError::Offline)?;
        let status = std::str::from_utf8(&response[..end])
            .ok()
            .and_then(|h| h.split(' ').nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or(CallError::Offline)?;
        let value: Value = serde_json::from_slice(&response[end + 4..]).unwrap_or_default();
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            let why = value["error"].as_str().unwrap_or("The other Mac refused the request.").to_string();
            Err(CallError::Refused(status, why))
        }
    }

    async fn peer_call(&self, peer: &str, method: &str, target: &str, body: Option<&Value>, timeout: Duration) -> Result<Value, CallError> {
        let address = peer_row(&self.db, peer).ok().flatten().map(|p| p.address);
        let token = self.tokens.lock().ok().and_then(|t| t.get(peer).map(|t| t.send.clone()));
        let (Some(address), Some(token)) = (address, token) else {
            return Err(CallError::Refused(401, "That Mac isn't linked anymore.".into()));
        };
        self.call(&address, Some(&token), method, target, body, timeout).await
    }

    /// What the person sees when a call to a linked Mac fails.
    fn explain(&self, peer: &str, error: CallError) -> String {
        match error {
            CallError::Offline => {
                let name = peer_row(&self.db, peer).ok().flatten().map_or_else(|| "That Mac".into(), |p| p.name);
                format!("{name} is offline. Try again when it's back.")
            }
            CallError::Refused(_, why) => why,
        }
    }

    fn set_online(&self, peer: &str, online: bool) {
        let (changed, stopped) = match self.live.lock() {
            Ok(mut live) => {
                let state = live.entry(peer.to_string()).or_default();
                let changed = state.online != online;
                state.online = online;
                let stopped: Vec<String> = if online { Vec::new() } else { state.busy.drain().collect() };
                (changed, stopped)
            }
            Err(_) => return,
        };
        for agent in stopped {
            self.ui.event("agent_status", json!({ "agentId": remote_id(peer, &agent), "busy": false }));
        }
        if changed {
            self.ui.event("links_changed", Value::Null);
            self.remote_changed(peer);
        }
    }

    /// Fetches a peer's familiars and which are busy, and keeps them for when it's offline.
    async fn refresh(&self, peer: &str) -> Result<(), CallError> {
        let reply = self.peer_call(peer, "GET", "/v1/familiars", None, CALL_TIMEOUT).await?;
        let familiars: Vec<Value> = reply["familiars"]
            .as_array()
            .map(|all| all.iter().filter(|f| f["id"].as_str().is_some_and(valid_id)).cloned().collect())
            .unwrap_or_default();
        let busy: HashSet<String> = reply["busy"].as_array().map(|b| b.iter().filter_map(|id| id.as_str().map(String::from)).collect()).unwrap_or_default();
        cache_familiars(&self.db, peer, &familiars);
        if let Ok(mut live) = self.live.lock() {
            live.entry(peer.to_string()).or_default().busy = busy;
        }
        self.remote_changed(peer);
        Ok(())
    }

    fn start_relay(self: &Arc<Self>, peer: &str) {
        let link = self.clone();
        let id = peer.to_string();
        let relay = tokio::spawn(async move { link.relay(id).await }).abort_handle();
        if let Ok(mut live) = self.live.lock() {
            if let Some(old) = live.entry(peer.to_string()).or_default().relay.replace(relay) {
                old.abort();
            }
        }
    }

    /// Follows a peer's events for as long as it stays linked, reconnecting with backoff.
    async fn relay(self: Arc<Self>, peer: String) {
        let mut backoff = BACKOFF_MIN;
        while self.tokens.lock().is_ok_and(|t| t.contains_key(&peer)) {
            if let CallError::Refused(401, _) = self.follow(&peer, &mut backoff).await {
                // It unlinked this Mac while this Mac couldn't hear, so drop this side too. Right after
                // pairing, the other Mac may not have saved its tokens yet, so a new link gets a minute.
                let settled = peer_row(&self.db, &peer).ok().flatten().is_some_and(|p| crate::db::now_ms() - p.created_at > 60_000);
                if settled {
                    let _ = self.forget(&peer);
                    return;
                }
            }
            self.set_online(&peer, false);
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// Connects to a peer and relays its events until a call fails.
    async fn follow(&self, peer: &str, backoff: &mut Duration) -> CallError {
        let hello = match self.peer_call(peer, "GET", "/v1/hello", None, CALL_TIMEOUT).await {
            Ok(hello) => hello,
            Err(e) => return e,
        };
        let start = match self.peer_call(peer, "GET", "/v1/events", None, CALL_TIMEOUT).await {
            Ok(start) => start,
            Err(e) => return e,
        };
        let (mut seq, mut epoch) = (start["seq"].as_u64().unwrap_or(0), start["epoch"].clone());
        if let Err(e) = self.refresh(peer).await {
            return e;
        }
        let name = hello["name"].as_str().and_then(clean_mac_name);
        mark_seen(&self.db, peer, name.as_deref(), hello["version"].as_str().filter(|v| v.len() <= 40));
        self.set_online(peer, true);
        *backoff = BACKOFF_MIN;
        loop {
            let target = format!("/v1/events?after={seq}&wait={}", LONG_POLL.as_secs());
            let batch = match self.peer_call(peer, "GET", &target, None, LONG_POLL + CALL_TIMEOUT).await {
                Ok(batch) => batch,
                Err(e) => return e,
            };
            mark_seen(&self.db, peer, None, None);
            let next = batch["seq"].as_u64().unwrap_or(seq);
            if batch["epoch"] != epoch || batch["reset"] == true {
                // It restarted or this Mac fell behind: start from now with a fresh list.
                (seq, epoch) = (next, batch["epoch"].clone());
                if let Err(e) = self.refresh(peer).await {
                    return e;
                }
                continue;
            }
            seq = next;
            let mut changed = false;
            for event in batch["events"].as_array().into_iter().flatten() {
                changed |= self.relay_event(peer, event);
            }
            if changed {
                if let Err(e) = self.refresh(peer).await {
                    return e;
                }
            }
        }
    }

    /// Re-emits one peer event under the names the window listens to. True when its familiars changed.
    fn relay_event(&self, peer: &str, event: &Value) -> bool {
        let kind = event["kind"].as_str().unwrap_or_default();
        let name = match kind {
            "agents_changed" => return true,
            "message" => "message",
            "delta" => "delta",
            "model" => "agent_model",
            "status" => "agent_status",
            _ => return false,
        };
        let mut payload = event["payload"].clone();
        let Some(agent) = payload["agentId"].as_str().filter(|id| valid_id(id)).map(String::from) else { return false };
        if kind == "status" {
            if let Ok(mut live) = self.live.lock() {
                let busy = &mut live.entry(peer.to_string()).or_default().busy;
                if payload["busy"] == true {
                    busy.insert(agent.clone());
                } else {
                    busy.remove(&agent);
                }
            }
        }
        if let Some(fields) = payload.as_object_mut() {
            fields.insert("agentId".into(), remote_id(peer, &agent).into());
            fields.remove("attachments");
        }
        self.ui.event(name, payload);
        false
    }

    // ----- What the window asks for -----

    /// Every linked Mac's familiars as last fetched, with remote ids and where they live.
    pub fn remote_familiars(&self) -> Vec<Value> {
        let rows = peer_rows(&self.db).unwrap_or_default();
        let mut all = Vec::new();
        for row in rows {
            let home = json!({ "peerId": row.id, "name": row.name, "online": self.online(&row.id) });
            for familiar in &row.familiars {
                let (Some(fields), Some(id)) = (familiar.as_object(), familiar["id"].as_str()) else { continue };
                let mut fields = fields.clone();
                fields.insert("id".into(), remote_id(&row.id, id).into());
                // Covens and SSH hosts belong to the home Mac.
                fields.insert("covenId".into(), Value::Null);
                fields.insert("host".into(), Value::Null);
                fields.insert("home".into(), home.clone());
                all.push(Value::Object(fields));
            }
        }
        all
    }

    pub fn remote_busy(&self) -> Vec<String> {
        let Ok(live) = self.live.lock() else { return Vec::new() };
        live.iter().flat_map(|(peer, state)| state.busy.iter().map(move |agent| remote_id(peer, agent))).collect()
    }

    /// The familiar's name and its Mac's, for messages.
    fn names(&self, peer: &str, agent: &str) -> Option<(String, String)> {
        let row = peer_row(&self.db, peer).ok().flatten()?;
        let familiar = row.familiars.iter().find(|f| f["id"] == agent).and_then(|f| f["name"].as_str()).unwrap_or("That familiar").to_string();
        Some((familiar, row.name))
    }

    pub async fn messages(&self, id: &str, before: Option<i64>, limit: Option<i64>) -> Result<Vec<Value>, String> {
        let (peer, agent) = parse_remote(id).ok_or("That familiar id is malformed.")?;
        let mut target = format!("/v1/familiars/{agent}/messages?limit={}", limit.unwrap_or(100).clamp(1, 500));
        if let Some(before) = before {
            target.push_str(&format!("&before={before}"));
        }
        let reply = self.peer_call(peer, "GET", &target, None, CALL_TIMEOUT).await.map_err(|e| self.explain(peer, e))?;
        let mut messages = match reply {
            Value::Array(messages) => messages,
            _ => return Err("The other Mac sent an odd answer.".into()),
        };
        for message in &mut messages {
            message["agentId"] = id.into();
        }
        Ok(messages)
    }

    pub async fn send(&self, id: &str, text: &str) -> Result<Value, String> {
        let (peer, agent) = parse_remote(id).ok_or("That familiar id is malformed.")?;
        let body = json!({ "text": text });
        let mut message = self.peer_call(peer, "POST", &format!("/v1/familiars/{agent}/messages"), Some(&body), CALL_TIMEOUT).await.map_err(|e| self.explain(peer, e))?;
        message["agentId"] = id.into();
        Ok(message)
    }

    pub async fn stop(&self, id: &str) -> Result<(), String> {
        let (peer, agent) = parse_remote(id).ok_or("That familiar id is malformed.")?;
        self.peer_call(peer, "POST", &format!("/v1/familiars/{agent}/stop"), None, CALL_TIMEOUT).await.map(|_| ()).map_err(|e| self.explain(peer, e))
    }

    pub async fn create(&self, peer: &str, input: &NewFamiliar) -> Result<Value, String> {
        let row = peer_row(&self.db, peer)?.ok_or("That Mac isn't linked anymore.")?;
        let body = serde_json::to_value(input).map_err(|e| e.to_string())?;
        let made = self.peer_call(peer, "POST", "/v1/familiars", Some(&body), CALL_TIMEOUT).await.map_err(|e| self.explain(peer, e))?;
        let id = made["id"].as_str().filter(|id| valid_id(id)).ok_or("The other Mac sent an odd answer.")?.to_string();
        // Its agents_changed event refreshes the list too; this makes the new familiar list right away.
        let _ = self.refresh(peer).await;
        let listed = self.remote_familiars().into_iter().find(|f| f["id"] == remote_id(peer, &id).as_str());
        Ok(listed.unwrap_or_else(|| {
            let mut made = made;
            made["id"] = remote_id(peer, &id).into();
            made["home"] = json!({ "peerId": peer, "name": row.name, "online": true });
            made
        }))
    }
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

/// Where to reach `address` on the link port, if it's an address `allow` accepts. A typed port wins.
async fn resolve(address: &str, allow: fn(IpAddr) -> bool) -> Option<SocketAddr> {
    let candidates: Vec<SocketAddr> = if let Ok(with_port) = address.parse::<SocketAddr>() {
        vec![with_port]
    } else if let Ok(ip) = address.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
        vec![SocketAddr::new(ip, PORT)]
    } else {
        tokio::net::lookup_host((address, PORT)).await.ok()?.collect()
    };
    candidates.into_iter().find(|to| allow(to.ip()))
}

// ---------- The app's side ----------

/// Runs a linked Mac's requests through this Mac's runtime.
struct AppLocal {
    app: tauri::AppHandle,
    db: Arc<Db>,
    runtime: Arc<crate::runtime::Runtime>,
}

impl Local for AppLocal {
    fn send(&self, agent_id: &str, text: String, source: &str) -> Result<Message, String> {
        self.runtime.send(agent_id, text, Some(source))
    }

    fn stop<'a>(&'a self, agent_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(self.runtime.stop(agent_id))
    }

    fn busy(&self) -> Vec<String> {
        self.runtime.busy_agents()
    }

    fn create(&self, input: NewFamiliar) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            let status = crate::new_agent(&self.db, input.into()).await?;
            let _ = tauri::Emitter::emit(&self.app, "agents_changed", ());
            serde_json::to_value(status).map_err(|e| e.to_string())
        })
    }
}

/// Starts the link: the server on the tailnet, and a relay for each linked Mac.
pub fn install(app: &tauri::AppHandle, db: Arc<Db>, secrets: Arc<dyn SecretStore>, runtime: Arc<crate::runtime::Runtime>) -> Result<(), String> {
    use tauri::Listener;
    let local = Arc::new(AppLocal { app: app.clone(), db: db.clone(), runtime });
    let link = Link::new(db, secrets, local, Arc::new(app.clone()), is_tailnet, mac_name())?;
    if LINK.set(link.clone()).is_err() {
        return Ok(());
    }
    // Local changes have no payload; relayed ones name their Mac and don't go back out.
    app.listen_any("agents_changed", |event| {
        if event.payload() == "null" {
            record("agents_changed", || Value::Null);
        }
    });
    tauri::async_runtime::spawn(async move {
        let peers: Vec<String> = link.tokens.lock().map(|t| t.keys().cloned().collect()).unwrap_or_default();
        for peer in peers {
            link.start_relay(&peer);
        }
        link.listen().await;
    });
    Ok(())
}

/// Notes an event about one of this Mac's familiars for linked Macs. `payload` runs only when a Mac is linked.
pub fn record(kind: &str, payload: impl FnOnce() -> Value) {
    if let Some(link) = LINK.get().filter(|l| l.has_peers()) {
        link.events.push(kind, payload());
    }
}

/// Refuses a familiar that lives on another Mac, for commands that only work on this Mac's familiars.
pub fn local_only(id: &str) -> Result<(), String> {
    lives_elsewhere(LINK.get().map(Arc::as_ref), id)
}

fn lives_elsewhere(link: Option<&Link>, id: &str) -> Result<(), String> {
    if !is_remote(id) {
        return Ok(());
    }
    let (peer, agent) = parse_remote(id).ok_or("That familiar id is malformed.")?;
    let (familiar, mac) = link.and_then(|l| l.names(peer, agent)).unwrap_or_else(|| ("That familiar".into(), "another Mac".into()));
    Err(format!("{familiar} lives on {mac}. Open Grimoire there to change it."))
}

/// A familiar as `list_agents` shows it: this Mac's have no home.
pub fn local_value(status: &crate::remote::AgentStatus) -> Value {
    let mut value = serde_json::to_value(status).unwrap_or_default();
    value["home"] = Value::Null;
    value
}

pub fn remote_familiars() -> Vec<Value> {
    LINK.get().map(|l| l.remote_familiars()).unwrap_or_default()
}

pub fn remote_busy() -> Vec<String> {
    LINK.get().map(|l| l.remote_busy()).unwrap_or_default()
}

pub async fn messages(id: &str, before: Option<i64>, limit: Option<i64>) -> Result<Vec<Value>, String> {
    running()?.messages(id, before, limit).await
}

pub async fn send(id: &str, text: &str) -> Result<Value, String> {
    running()?.send(id, text).await
}

pub async fn stop(id: &str) -> Result<(), String> {
    running()?.stop(id).await
}

pub async fn create(peer: &str, input: NewFamiliar) -> Result<Value, String> {
    running()?.create(peer, &input).await
}

#[tauri::command]
pub fn link_status() -> Result<LinkStatus, String> {
    running()?.status()
}

#[tauri::command]
pub fn link_start_pairing() -> Result<PairingCode, String> {
    running()?.start_pairing()
}

#[tauri::command]
pub async fn link_pair(address: String, code: String) -> Result<Peer, String> {
    running()?.pair(&address, &code).await
}

#[tauri::command]
pub async fn link_unlink(peer_id: String) -> Result<(), String> {
    running()?.unlink(&peer_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Agent;
    use crate::vault::MemoryStore;

    struct FakeLocal {
        db: Arc<Db>,
        sent: Mutex<Vec<(String, String, String)>>,
    }

    impl Local for FakeLocal {
        fn send(&self, agent_id: &str, text: String, source: &str) -> Result<Message, String> {
            self.sent.lock().unwrap().push((agent_id.into(), text.clone(), source.into()));
            self.db.insert_message_from(agent_id, "user", &text, "done", Some(source))
        }
        fn stop<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn busy(&self) -> Vec<String> {
            Vec::new()
        }
        fn create(&self, _: NewFamiliar) -> BoxFuture<'_, Result<Value, String>> {
            Box::pin(async { Err("not in tests".to_string()) })
        }
    }

    #[derive(Default)]
    struct Events(Mutex<Vec<(String, Value)>>);

    impl Ui for Events {
        fn event(&self, name: &str, payload: Value) {
            self.0.lock().unwrap().push((name.into(), payload));
        }
    }

    struct Mac {
        link: Arc<Link>,
        db: Arc<Db>,
        secrets: Arc<MemoryStore>,
        local: Arc<FakeLocal>,
        ui: Arc<Events>,
    }

    fn loopback(ip: IpAddr) -> bool {
        ip.is_loopback()
    }

    fn mac(name: &str) -> Mac {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets = Arc::new(MemoryStore::new());
        let local = Arc::new(FakeLocal { db: db.clone(), sent: Mutex::new(Vec::new()) });
        let ui = Arc::new(Events::default());
        let link = Link::new(db.clone(), secrets.clone(), local.clone(), ui.clone(), loopback, name.into()).unwrap();
        Mac { link, db, secrets, local, ui }
    }

    /// Starts `mac`'s server on a loopback port and returns its address.
    async fn listen(mac: &Mac) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(mac.link.clone().serve(listener));
        let mut server = mac.link.server.lock().unwrap();
        server.listening = true;
        server.addresses = vec![address.clone()];
        address
    }

    fn agent(id: &str, name: &str) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            provider: Provider::Claude,
            model: None,
            workspace: "/tmp/x".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: 0,
        }
    }

    fn pair_body(code: &str, install_id: &str) -> Vec<u8> {
        json!({ "code": code, "installId": install_id, "name": "Laptop", "address": "100.64.0.9", "tokenForYou": new_token() }).to_string().into_bytes()
    }

    fn with_code(link: &Link) -> String {
        link.server.lock().unwrap().listening = true;
        link.start_pairing().unwrap().code
    }

    #[tokio::test]
    async fn a_code_pairs_once_and_stores_tokens_in_the_secret_store() {
        let a = mac("Studio");
        let code = with_code(&a.link);
        assert_eq!(code.len(), CODE_LEN);
        let now = Instant::now();
        let (status, reply) = a.link.answer_pair(&pair_body(&code.to_lowercase(), "peer-b"), now);
        assert_eq!(status, 200, "{reply}");
        let token = reply["tokenForMe"].as_str().unwrap().to_string();
        assert_eq!(a.secrets.get(&expect_key("peer-b")).unwrap().as_deref(), Some(token.as_str()));
        assert!(a.secrets.get(&send_key("peer-b")).unwrap().is_some());
        let row: String = a.db.with(|c| c.query_row("SELECT id || name || address || familiars FROM link_peers", [], |r| r.get(0))).unwrap();
        assert!(!row.contains(&token), "tokens stay out of the database");
        let (again, _) = a.link.answer_pair(&pair_body(&code, "peer-c"), now);
        assert_eq!(again, 403, "a code works once");
    }

    #[tokio::test]
    async fn expired_codes_and_five_misses_end_a_code() {
        let a = mac("Studio");
        let code = with_code(&a.link);
        assert!(!a.link.check_code(&code, Instant::now() + CODE_TTL));
        assert!(!a.link.check_code(&code, Instant::now()), "an expired code is gone");

        let code = with_code(&a.link);
        for _ in 0..CODE_TRIES {
            assert!(!a.link.check_code("WRONGCDE", Instant::now()));
        }
        assert!(!a.link.check_code(&code, Instant::now()), "five misses kill the code");

        let code = with_code(&a.link);
        assert!(!a.link.check_code("WRONGCDE", Instant::now()));
        assert!(a.link.check_code(&format!("{}-{}", &code[..4], &code[4..]), Instant::now()), "a miss or a dash doesn't matter");
    }

    #[tokio::test]
    async fn pairing_is_rate_limited() {
        let a = mac("Studio");
        let now = Instant::now();
        for _ in 0..PAIR_PER_MINUTE {
            assert_ne!(a.link.answer_pair(b"{}", now).0, 429);
        }
        assert_eq!(a.link.answer_pair(b"{}", now).0, 429);
        assert_ne!(a.link.answer_pair(b"{}", now + Duration::from_secs(61)).0, 429);
    }

    #[tokio::test]
    async fn tokens_map_to_their_peer_and_others_get_401() {
        let a = mac("Studio");
        a.link.tokens.lock().unwrap().insert("b".into(), Tokens { send: "s".into(), expect: "token-b".repeat(6) });
        a.link.tokens.lock().unwrap().insert("c".into(), Tokens { send: "s".into(), expect: "token-c".repeat(6) });
        assert_eq!(a.link.peer_for(Some(&format!("Bearer {}", "token-c".repeat(6)))).as_deref(), Some("c"));
        assert_eq!(a.link.peer_for(Some(&"token-c".repeat(6))), None, "needs the Bearer scheme");
        assert_eq!(a.link.peer_for(Some("Bearer nope")), None);
        assert_eq!(a.link.peer_for(None), None);
        let request = |auth: Option<&str>| http::Request {
            head: http::parse_head(&format!("GET /v1/familiars HTTP/1.1{}", auth.map(|a| format!("\r\nAuthorization: {a}")).unwrap_or_default())).unwrap(),
            body: Vec::new(),
        };
        assert_eq!(a.link.route(request(None)).await.0, 401);
        assert_eq!(a.link.route(request(Some("Bearer wrong"))).await.0, 401);
        assert_eq!(a.link.route(request(Some(&format!("Bearer {}", "token-b".repeat(6))))).await.0, 200);
    }

    #[test]
    fn only_tailnet_addresses_pass() {
        for ok in ["100.64.0.1", "100.127.255.254", "100.100.100.100", "fd7a:115c:a1e0::1", "fd7a:115c:a1e0:ab12::5", "::ffff:100.64.1.2"] {
            assert!(is_tailnet(ok.parse().unwrap()), "{ok}");
        }
        for bad in ["100.63.255.255", "100.128.0.1", "10.0.0.1", "127.0.0.1", "192.168.1.2", "8.8.8.8", "fd7a:115c:a1e1::1", "fe80::1", "::1"] {
            assert!(!is_tailnet(bad.parse().unwrap()), "{bad}");
        }
    }

    #[tokio::test]
    async fn resolves_only_allowed_addresses() {
        assert_eq!(resolve("100.64.0.2", is_tailnet).await, Some("100.64.0.2:47620".parse().unwrap()));
        assert_eq!(resolve("[fd7a:115c:a1e0::2]", is_tailnet).await, Some(format!("[fd7a:115c:a1e0::2]:{PORT}").parse().unwrap()));
        assert_eq!(resolve("192.168.1.5", is_tailnet).await, None);
        assert_eq!(resolve("127.0.0.1:9", is_tailnet).await, None);
    }

    #[test]
    fn remote_ids_round_trip_and_malformed_ones_are_refused() {
        let id = remote_id("1b4e28ba-2fa1-11d2-883f-0016d3cca427", "abc-123");
        assert_eq!(id, "peer:1b4e28ba-2fa1-11d2-883f-0016d3cca427:abc-123");
        assert_eq!(parse_remote(&id), Some(("1b4e28ba-2fa1-11d2-883f-0016d3cca427", "abc-123")));
        assert!(is_remote(&id) && !is_remote("abc-123"));
        for bad in ["peer:", "peer:a", "peer::b", "peer:a:", "peer:a:b:c", "peer:a/../b:c", "peer:a:../../etc", "peer:a:b%2f", "nope:a:b", &format!("peer:a:{}", "x".repeat(65))] {
            assert_eq!(parse_remote(bad), None, "{bad}");
        }
    }

    #[test]
    fn remote_ids_are_refused_with_where_the_familiar_lives() {
        let a = mac("Studio");
        save_peer(&a.db, "mac-b", "Laptop", "100.64.0.9").unwrap();
        cache_familiars(&a.db, "mac-b", &[json!({ "id": "f1", "name": "Scribe" })]);
        assert_eq!(lives_elsewhere(Some(&a.link), "f1"), Ok(()));
        assert_eq!(lives_elsewhere(Some(&a.link), "peer:mac-b:f1").unwrap_err(), "Scribe lives on Laptop. Open Grimoire there to change it.");
        assert_eq!(lives_elsewhere(None, "peer:mac-b:f1").unwrap_err(), "That familiar lives on another Mac. Open Grimoire there to change it.");
        assert_eq!(lives_elsewhere(Some(&a.link), "peer:../x").unwrap_err(), "That familiar id is malformed.");
    }

    #[tokio::test]
    async fn the_event_log_returns_what_came_after_and_wraps() {
        let log = EventLog::new();
        assert!(log.after(0).is_none());
        log.push("status", json!(1));
        log.push("status", json!(2));
        let batch = log.after(1).unwrap();
        assert_eq!((batch.seq, batch.events.len(), batch.reset), (2, 1, false));
        assert_eq!(batch.events[0]["payload"], 2);
        assert!(log.after(9).unwrap().reset, "a sequence the log never had resets");
        for i in 0..RING + 5 {
            log.push("delta", json!(i));
        }
        let batch = log.after(1).unwrap();
        assert!(batch.reset, "events fell out of the ring");
        assert_eq!(batch.events.len(), RING);
        assert_eq!(log.after(log.head() - 1).unwrap().events.len(), 1);
    }

    #[tokio::test]
    async fn a_waiting_poll_wakes_on_a_new_event() {
        let log = Arc::new(EventLog::new());
        let waiter = tokio::spawn({
            let log = log.clone();
            async move { log.wait(0, Duration::from_secs(5)).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        log.push("message", json!({ "agentId": "a" }));
        let batch = tokio::time::timeout(Duration::from_secs(2), waiter).await.unwrap().unwrap();
        assert_eq!(batch.events.len(), 1);
        let empty = log.wait(1, Duration::from_millis(20)).await;
        assert!(empty.events.is_empty() && empty.seq == 1);
    }

    #[tokio::test]
    async fn relayed_events_get_remote_ids() {
        let a = mac("Studio");
        let event = json!({ "kind": "status", "payload": { "agentId": "f1", "busy": true } });
        assert!(!a.link.relay_event("mac-b", &event));
        assert_eq!(a.link.remote_busy(), ["peer:mac-b:f1"]);
        assert!(a.link.relay_event("mac-b", &json!({ "kind": "agents_changed", "payload": null })));
        assert!(!a.link.relay_event("mac-b", &json!({ "kind": "delta", "payload": { "agentId": "../x" } })));
        let events = a.ui.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], ("agent_status".to_string(), json!({ "agentId": "peer:mac-b:f1", "busy": true })));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_macs_pair_list_read_send_and_unlink() {
        let (a, b) = (mac("Studio"), mac("Laptop"));
        let a_address = listen(&a).await;
        listen(&b).await;
        a.db.insert_agent(&agent("f1", "Scribe")).unwrap();
        a.db.insert_message("f1", "assistant", "Hello from the studio", "done").unwrap();

        let code = a.link.start_pairing().unwrap().code;
        let wrong = b.link.pair(&a_address, "ABCDEFGH").await.unwrap_err();
        assert!(wrong.contains("wrong or has expired"), "{wrong}");
        let peer = b.link.pair(&a_address, &code).await.unwrap();
        assert_eq!((peer.id.as_str(), peer.name.as_str()), (a.link.install_id.as_str(), "Studio"));
        assert!(a.link.status().unwrap().peers.iter().any(|p| p.id == b.link.install_id && p.name == "Laptop"));
        assert!(b.link.pair(&a_address, &code).await.is_err(), "the code is used up");

        b.link.refresh(&peer.id).await.unwrap();
        let listed = b.link.remote_familiars();
        let remote = remote_id(&peer.id, "f1");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["id"], remote.as_str());
        assert_eq!(listed[0]["name"], "Scribe");
        assert_eq!(listed[0]["home"]["name"], "Studio");

        let messages = b.link.messages(&remote, None, None).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!((messages[0]["text"].as_str(), messages[0]["agentId"].as_str()), (Some("Hello from the studio"), Some(remote.as_str())));

        let sent = b.link.send(&remote, "Hi from the laptop").await.unwrap();
        assert_eq!(sent["agentId"], remote.as_str());
        assert_eq!(a.local.sent.lock().unwrap()[0], ("f1".into(), "Hi from the laptop".into(), "From Laptop".into()));
        assert!(b.link.messages(&remote_id(&peer.id, "nobody"), None, None).await.unwrap_err().contains("no longer exists"));

        // A's events reach B's window with B's ids.
        a.link.events.push("delta", json!({ "agentId": "f1", "messageId": 1, "chunk": "hi" }));
        let relayed = async {
            loop {
                if b.ui.0.lock().unwrap().iter().any(|(name, p)| name == "delta" && p["agentId"] == remote.as_str()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), relayed).await.expect("the delta was relayed");

        b.link.unlink(&peer.id).await.unwrap();
        assert!(b.link.status().unwrap().peers.is_empty());
        assert!(a.link.status().unwrap().peers.is_empty(), "the other Mac dropped its side too");
        assert!(b.secrets.get(&send_key(&peer.id)).unwrap().is_none());
        assert!(a.secrets.get(&expect_key(&b.link.install_id)).unwrap().is_none());
        assert!(b.link.remote_familiars().is_empty());
    }
}
