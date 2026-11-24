//! Linked Macs. Two Macs running Grimoire pair over the owner's Tailscale tailnet and show each
//! other's familiars. A familiar lives only on its home Mac; other Macs list it, read its chat, send
//! it messages, and stop its turns through this Mac's link server. Nothing else crosses: no vault,
//! approvals, settings, memory, or tools.
//!
//! The server listens on port 47620 on the addresses Tailscale reports for this Mac, and drops
//! connections from anything Tailscale doesn't list as a peer. 100.64.0.0/10 alone proves nothing:
//! ISPs and hotspots use it for carrier-grade NAT. Without Tailscale's CLI there is no peer list, so
//! linking stays off. WireGuard encrypts the traffic, so the protocol is
//! plain HTTP/1.1 JSON. Pairing trades a one-time code for a token each way; the tokens live in the
//! Keychain.

use crate::db::{Access, Avatar, Db, Message, Provider};
use crate::http;
use crate::runtime::TurnOutcome;
use crate::vault::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::AbortHandle;
use tokio::time::Instant;

pub const PORT: u16 = 47620;
pub const REMOTE_PREFIX: &str = "peer:";
const BODY_LIMIT: usize = 1024 * 1024;
const RESPONSE_LIMIT: u64 = 32 * 1024 * 1024;
const RING: usize = 1000;
pub(crate) const LONG_POLL: Duration = Duration::from_secs(25);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a connection without a token yet may take to send its request.
const PRE_AUTH_READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
const BIND_RETRY: Duration = Duration::from_secs(60);
const BACKOFF_MIN: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
pub(crate) const CODE_TTL: Duration = Duration::from_secs(10 * 60);
pub(crate) const CODE_TRIES: u8 = 5;
const CODE_LEN: usize = 8;
/// No 0, O, 1, or I, so a code read aloud can't be misheard.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub(crate) const PAIR_PER_MINUTE: usize = 20;
/// Connections from linked Macs, once their token checks out.
const MAX_CONNECTIONS: usize = 64;
/// Connections that haven't shown a token yet, pairing included.
const PRE_AUTH_CONNECTIONS: usize = 8;
/// Of those, how many one address may hold, so one device can't take them all.
const PRE_AUTH_PER_ADDRESS: usize = 2;
/// How long both tokens from a crossed pairing stay valid. See `Tokens`.
const PAIR_WINDOW: Duration = Duration::from_secs(2 * 60);
/// The least time between two `tailscale status` runs for an address it didn't list.
const TAILNET_RECHECK: Duration = Duration::from_secs(10);
const TAILSCALE_TIMEOUT: Duration = Duration::from_secs(5);
const TAILSCALE_APP: &str = "/Applications/Tailscale.app";
/// Tailscale's Apple Developer Team ID, which signs its app.
const TAILSCALE_TEAM: &str = "W5364U7YZB";
const NO_CLI: &str = "Grimoire can't find Tailscale's command-line tool, so linking is off. Make sure the Tailscale app is installed.";
const INSTALL_ID: &str = "link_install_id";
/// How often the home Mac tries to bring a handoff's reply back before it gives up, and the longest
/// wait between tries: about 25 minutes in all.
const REPLY_TRIES: usize = 10;
const REPLY_BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);
/// The first wait between those tries. Tests don't wait seconds.
const REPLY_BACKOFF_MIN: Duration = if cfg!(test) { Duration::from_millis(20) } else { BACKOFF_MIN };
/// The most handoffs one linked Mac sends here in `delegation::WINDOW` (10 minutes).
const HANDOFFS_PER_PEER: usize = 10;
/// The most cross-Mac turns one linked Mac has running here at once.
const HANDOFFS_IN_FLIGHT: usize = 3;

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a linked Mac may do with this Mac's familiars. The app runs it through the runtime.
pub trait Local: Send + Sync {
    fn send(&self, agent_id: &str, text: String, source: &str) -> Result<Message, String>;
    fn stop<'a>(&'a self, agent_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
    fn busy(&self) -> Vec<String>;
    /// Makes a familiar here and returns it as `list_agents` shows it.
    fn create(&self, input: NewFamiliar) -> BoxFuture<'_, Result<Value, String>>;
    /// A familiar on `peer` (called `mac`) asks or messages one here: see `delegation::incoming`.
    /// `here` is this Mac's install id. Returns the side conversation's id and its exchange.
    fn handoff(&self, here: &str, peer: &str, mac: &str, request: &crate::delegation::Incoming) -> Result<(String, BoxFuture<'static, TurnOutcome>), String>;
    /// `peer` brings back the reply to a handoff sent from here: see `delegation::away_reply`.
    fn handoff_reply(&self, peer: &str, thread: &str, request: i64, outcome: TurnOutcome) -> Result<(), String>;
}

/// A familiar on a linked Mac, as familiars here pick it for `ask_familiar` or `message_familiar`.
#[derive(Debug, Clone, PartialEq)]
pub struct Linked {
    /// Its `peer:` id.
    pub id: String,
    pub peer: String,
    /// Its id on its home Mac.
    pub agent: String,
    pub name: String,
    /// Its home Mac's name.
    pub mac: String,
    pub online: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
}

impl Linked {
    /// "Scribe on NigelBot".
    pub fn label(&self) -> String {
        format!("{} on {}", self.name, self.mac)
    }
}

/// Tailscale's CLI isn't at a place Grimoire trusts, or isn't Tailscale's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoCli;

/// What Tailscale knows about this Mac: its CLI in the app, a fake in tests.
pub trait Tailnet: Send + Sync {
    /// The output of `tailscale status --json`, or None when the CLI didn't answer.
    fn status(&self) -> BoxFuture<'_, Result<Option<String>, NoCli>>;
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
pub(crate) fn valid_id(id: &str) -> bool {
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

/// Tailscale's addresses for this Mac and for its peers. A stopped Tailscale has none.
#[derive(Debug, PartialEq)]
struct TailscaleStatus {
    own: Vec<IpAddr>,
    peers: HashSet<IpAddr>,
}

/// Reads `tailscale status --json`: Self.TailscaleIPs and each Peer's TailscaleIPs.
fn parse_status(text: &str) -> Option<TailscaleStatus> {
    let status: Value = serde_json::from_str(text).ok()?;
    let state = status.get("BackendState")?.as_str()?;
    if state != "Running" {
        return Some(TailscaleStatus { own: Vec::new(), peers: HashSet::new() });
    }
    let ips = |node: &Value| -> Vec<IpAddr> {
        node["TailscaleIPs"].as_array().into_iter().flatten().filter_map(|ip| ip.as_str()?.parse::<IpAddr>().ok()).map(|ip| ip.to_canonical()).collect()
    };
    let mut own = Vec::new();
    for ip in ips(&status["Self"]) {
        if !own.contains(&ip) {
            own.push(ip);
        }
    }
    own.sort_by_key(|ip| ip.is_ipv6());
    let peers = status["Peer"].as_object().into_iter().flat_map(|peers| peers.values()).flat_map(ips).collect();
    Some(TailscaleStatus { own, peers })
}

/// Tailnet addresses only, as a check on what Tailscale reports. Tests run on loopback.
fn in_range(ip: IpAddr) -> bool {
    is_tailnet(ip) || (cfg!(test) && ip.to_canonical().is_loopback())
}

/// The addresses this Mac listens on and the peers it trusts, as last checked.
#[derive(Debug)]
struct TailnetView {
    /// Tailscale's CLI was found. Without it nothing is trusted and nothing listens.
    cli: bool,
    own: Vec<IpAddr>,
    peers: HashSet<IpAddr>,
}

impl TailnetView {
    /// Before Tailscale is asked, nothing is trusted.
    fn unchecked() -> Self {
        TailnetView { cli: true, own: Vec::new(), peers: HashSet::new() }
    }

    fn new(status: Result<Option<&str>, NoCli>) -> Self {
        match status {
            Err(NoCli) => TailnetView { cli: false, own: Vec::new(), peers: HashSet::new() },
            Ok(text) => {
                let (own, peers) = text.and_then(parse_status).map_or_else(Default::default, |s| (s.own, s.peers));
                TailnetView { cli: true, own: own.into_iter().filter(|ip| in_range(*ip)).collect(), peers }
            }
        }
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        in_range(ip) && self.peers.contains(&ip)
    }
}

/// The Tailscale app's own CLI, the only program Grimoire runs as Tailscale.
const TAILSCALE_CLI: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";

/// The app's CLI at `cli`, resolved, when it's a file inside the app at `app`. Never the login
/// shell's PATH: whatever answers as Tailscale picks the addresses this Mac sends tokens to. The
/// links in /usr/local/bin and /opt/homebrew/bin count only when they resolve to this same file, so
/// running it directly covers them, and a link anywhere else is never run.
fn tailscale_program(app: &Path, cli: &Path) -> Option<PathBuf> {
    let app = std::fs::canonicalize(app).ok()?;
    std::fs::canonicalize(cli).ok().filter(|real| real.starts_with(&app) && real.is_file())
}

/// The app's CLI, once `signed` (the codesign check) confirms Tailscale signed the app. Any admin
/// account can write to /Applications, so where a file sits there proves nothing.
async fn find_tailscale<Signed, Check>(app: &Path, cli: &Path, signed: Signed) -> Result<PathBuf, NoCli>
where
    Signed: FnOnce(PathBuf) -> Check,
    Check: Future<Output = bool>,
{
    let program = tailscale_program(app, cli).ok_or(NoCli)?;
    if signed(app.to_path_buf()).await {
        Ok(program)
    } else {
        Err(NoCli)
    }
}

/// Whether macOS confirms the app at `app`, its CLI included, is signed by Tailscale's team.
async fn signed_by_tailscale(app: PathBuf) -> bool {
    let requirement = format!("-R=anchor apple generic and certificate leaf[subject.OU] = \"{TAILSCALE_TEAM}\"");
    let mut command = tokio::process::Command::new("/usr/bin/codesign");
    command
        .args(["--verify", "--strict"])
        .arg(requirement)
        .arg(app)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    matches!(tokio::time::timeout(TAILSCALE_TIMEOUT, command.status()).await, Ok(Ok(status)) if status.success())
}

struct TailscaleCli;

impl Tailnet for TailscaleCli {
    fn status(&self) -> BoxFuture<'_, Result<Option<String>, NoCli>> {
        Box::pin(async {
            let program = find_tailscale(Path::new(TAILSCALE_APP), Path::new(TAILSCALE_CLI), signed_by_tailscale).await?;
            let mut command = tokio::process::Command::new(&program);
            command
                .args(["status", "--json"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let Ok(Ok(output)) = tokio::time::timeout(TAILSCALE_TIMEOUT, command.output()).await else { return Ok(None) };
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            Ok(parse_status(&text).is_some().then_some(text))
        })
    }
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

pub(crate) fn clean_mac_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= 80 && !name.chars().any(char::is_control)).then(|| name.to_string())
}

fn valid_token(token: &str) -> bool {
    (32..=128).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// 244 random bits from the system's generator.
pub(crate) fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

pub(crate) fn new_code() -> String {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    // Bytes 6 and 8 carry the uuid's version and variant; the rest are random.
    [0, 1, 2, 3, 4, 5, 10, 11].iter().map(|&i| CODE_ALPHABET[(bytes[i] % 32) as usize] as char).collect()
}

/// A code as typed, without spaces or dashes and in capitals.
pub(crate) fn normalize_code(code: &str) -> String {
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
pub(crate) struct EventLog {
    pub(crate) epoch: String,
    ring: Mutex<(u64, VecDeque<(u64, Value)>)>,
    notify: Notify,
}

pub(crate) struct Batch {
    pub(crate) seq: u64,
    pub(crate) events: Vec<Value>,
    /// The caller missed events, or asked about a sequence this log never had.
    pub(crate) reset: bool,
}

impl EventLog {
    pub(crate) fn new() -> Self {
        EventLog { epoch: uuid::Uuid::new_v4().simple().to_string(), ring: Mutex::new((0, VecDeque::new())), notify: Notify::new() }
    }

    pub(crate) fn push(&self, kind: &str, payload: Value) {
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

    pub(crate) fn head(&self) -> u64 {
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
    pub(crate) async fn wait(&self, after: u64, wait: Duration) -> Batch {
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

/// A token this Mac takes from a peer.
struct Expected {
    token: String,
    /// PAIR_WINDOW after its pairing, when it stops working unless pinned or the only one.
    expires: Instant,
    /// The peer has used it, so it outlasts PAIR_WINDOW.
    pinned: bool,
}

/// A pairing makes a token each way, and each Mac picks the token it expects. When two Macs pair each
/// other at once, each runs two pairings. Each keeps the send token from the pairing that finished
/// first, and takes every token it expects from pairings in the last PAIR_WINDOW, since it can't tell
/// which one the other Mac kept. The first token the peer uses is pinned and the others go at once.
/// Once the window passes, a token the peer never used stops working, unless it's the only one.
struct Tokens {
    /// What this Mac sends to the peer.
    send: String,
    /// When `send` was set, so a pairing that crosses it keeps it.
    paired: Instant,
    /// What the peer may send to this Mac. The Keychain keeps them space-separated, each several with
    /// its expiry; one token alone is the format from before crossed pairings.
    expect: Vec<Expected>,
}

impl Tokens {
    fn valid(&self, i: usize, now: Instant) -> bool {
        let e = &self.expect[i];
        e.pinned || self.expect.len() == 1 || now < e.expires
    }

    /// The peer used token `i`: it becomes the only one.
    fn pin(&mut self, i: usize) {
        let mut used = self.expect.swap_remove(i);
        used.pinned = true;
        self.expect = vec![used];
    }

    /// What the Keychain keeps, space-separated. One token is written alone, as before crossed
    /// pairings. Several are each written `token:expiry`, the expiry in Unix milliseconds, so a
    /// restart keeps their window rather than starting a new one.
    fn expect_text(&self) -> String {
        if let [only] = self.expect.as_slice() {
            return only.token.clone();
        }
        let (now, wall) = (Instant::now(), crate::db::now_ms());
        let at = |e: &Expected| wall + e.expires.saturating_duration_since(now).as_millis() as i64;
        self.expect.iter().map(|e| format!("{}:{}", e.token, at(e))).collect::<Vec<_>>().join(" ")
    }
}

/// Reads tokens back from the Keychain. One alone is the pinned token, or the only one, and keeps
/// working. Several come from a crossed pairing nobody used yet: each keeps the expiry saved with
/// it, and the first one the peer uses wins. A token among several with no expiry is from before
/// expiries were saved, and counts as expired.
fn loaded_tokens(send: String, expect: &str) -> Option<Tokens> {
    let (now, wall) = (Instant::now(), crate::db::now_ms());
    let parse = |text: &str| {
        let (token, expires) = match text.split_once(':') {
            Some((token, at)) => (token, at.parse::<i64>().ok()?),
            None => (text, wall),
        };
        let left = Duration::from_millis(expires.saturating_sub(wall).max(0) as u64).min(PAIR_WINDOW);
        valid_token(token).then(|| Expected { token: token.to_string(), expires: now + left, pinned: false })
    };
    let mut expect: Vec<Expected> = expect.split_whitespace().filter_map(parse).collect();
    if let [only] = expect.as_mut_slice() {
        only.pinned = true;
    }
    // `paired` only matters inside the window, so a restart counts as long after it.
    (valid_token(&send) && !expect.is_empty()).then_some(Tokens { send, paired: now.checked_sub(PAIR_WINDOW).unwrap_or(now), expect })
}

#[derive(Default)]
struct Live {
    online: bool,
    busy: HashSet<String>,
    relay: Option<AbortHandle>,
    /// The peer's event log id and the last event this Mac relayed from it. It outlives a relay so a
    /// reconnect can tell whether it missed anything.
    cursor: Option<(Value, u64)>,
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
    /// Tailscale doesn't list the address as a peer, so nothing was sent.
    Untrusted(String),
}

fn untrusted(address: &str) -> CallError {
    CallError::Untrusted(format!(
        "Tailscale doesn't list {address} as a device on your tailnet, so Grimoire didn't connect. Check the name, and that Tailscale is on for both Macs."
    ))
}

pub struct Link {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    local: Arc<dyn Local>,
    ui: Arc<dyn Ui>,
    tailnet: Arc<dyn Tailnet>,
    /// Which addresses may connect, and which this Mac connects to, with when Tailscale was asked.
    view: Mutex<(TailnetView, Option<Instant>)>,
    install_id: String,
    name: String,
    events: EventLog,
    pairing: Mutex<Option<Pairing>>,
    pair_tries: Mutex<VecDeque<Instant>>,
    tokens: Mutex<HashMap<String, Tokens>>,
    live: Mutex<HashMap<String, Live>>,
    server: Mutex<ThisMac>,
    connections: Arc<Semaphore>,
    pre_auth: Arc<Semaphore>,
    /// How many pre-auth connections each address holds.
    pre_auth_from: Arc<Mutex<HashMap<IpAddr, usize>>>,
    /// Each linked Mac's recent handoffs here, by its install id.
    handoffs_from: crate::delegation::Limiter,
    /// Each linked Mac's handoffs still running here.
    running_from: Arc<Mutex<HashMap<String, usize>>>,
}

/// One running handoff from a linked Mac, counted until it's dropped.
struct Running {
    peer: String,
    from: Arc<Mutex<HashMap<String, usize>>>,
}

impl Running {
    fn start(from: &Arc<Mutex<HashMap<String, usize>>>, peer: &str) -> Option<Running> {
        let mut running = from.lock().ok()?;
        let held = running.entry(peer.to_string()).or_default();
        if *held >= HANDOFFS_IN_FLIGHT {
            return None;
        }
        *held += 1;
        Some(Running { peer: peer.to_string(), from: from.clone() })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(mut running) = self.from.lock() {
            if let Some(held) = running.get_mut(&self.peer) {
                *held -= 1;
                if *held == 0 {
                    running.remove(&self.peer);
                }
            }
        }
    }
}

/// A connection's place in the pre-auth pool, counted against its address too.
struct PreAuth {
    _permit: OwnedSemaphorePermit,
    ip: IpAddr,
    from: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl Drop for PreAuth {
    fn drop(&mut self) {
        if let Ok(mut from) = self.from.lock() {
            match from.get(&self.ip).copied() {
                Some(held) if held > 1 => {
                    from.insert(self.ip, held - 1);
                }
                _ => {
                    from.remove(&self.ip);
                }
            }
        }
    }
}

static LINK: OnceLock<Arc<Link>> = OnceLock::new();

fn running() -> Result<&'static Arc<Link>, String> {
    LINK.get().ok_or_else(|| "Linked Macs aren't running.".into())
}

impl Link {
    fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>, local: Arc<dyn Local>, ui: Arc<dyn Ui>, tailnet: Arc<dyn Tailnet>, name: String) -> Result<Arc<Self>, String> {
        let install_id = install_id(&db)?;
        let mut tokens = HashMap::new();
        for peer in peer_rows(&db)? {
            let loaded = match (secrets.get(&send_key(&peer.id)), secrets.get(&expect_key(&peer.id))) {
                (Ok(Some(send)), Ok(Some(expect))) => loaded_tokens(send, &expect),
                _ => None,
            };
            match loaded {
                Some(loaded) => {
                    tokens.insert(peer.id, loaded);
                }
                None => eprintln!("The Keychain has no link tokens for {}. Pair it again.", peer.name),
            }
        }
        let server = ThisMac { install_id: install_id.clone(), name: name.clone(), ..Default::default() };
        Ok(Arc::new(Link {
            db,
            secrets,
            local,
            ui,
            tailnet,
            view: Mutex::new((TailnetView::unchecked(), None)),
            install_id,
            name,
            events: EventLog::new(),
            pairing: Mutex::new(None),
            pair_tries: Mutex::new(VecDeque::new()),
            tokens: Mutex::new(tokens),
            live: Mutex::new(HashMap::new()),
            server: Mutex::new(server),
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            pre_auth: Arc::new(Semaphore::new(PRE_AUTH_CONNECTIONS)),
            pre_auth_from: Arc::new(Mutex::new(HashMap::new())),
            handoffs_from: crate::delegation::Limiter::default(),
            running_from: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    /// Asks Tailscale again for this Mac's addresses and its peers. None when its CLI isn't found.
    pub(crate) async fn check_tailnet(&self) -> Option<Vec<IpAddr>> {
        let status = self.tailnet.status().await;
        let view = TailnetView::new(status.as_ref().map(Option::as_deref).map_err(|e| *e));
        let own = view.cli.then(|| view.own.clone());
        if let Ok(mut slot) = self.view.lock() {
            *slot = (view, Some(Instant::now()));
        }
        own
    }

    /// Whether `ip` is a peer on the tailnet. An address Tailscale didn't list sends this Mac back to
    /// ask it again, at most every TAILNET_RECHECK, in case the device just joined.
    pub(crate) async fn trusts(&self, ip: IpAddr) -> bool {
        let recheck = match self.view.lock() {
            Ok(mut slot) => {
                if slot.0.trusts(ip) {
                    return true;
                }
                let due = slot.1.is_none_or(|at| at.elapsed() >= TAILNET_RECHECK);
                if due {
                    // Claim the recheck so a burst of strangers runs the CLI once.
                    slot.1 = Some(Instant::now());
                }
                due
            }
            Err(_) => return false,
        };
        if !recheck {
            return false;
        }
        self.check_tailnet().await;
        self.view.lock().is_ok_and(|slot| slot.0.trusts(ip))
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

    /// Saves one pairing with `id`. Tokens it expects from pairings inside PAIR_WINDOW stay valid too.
    fn add_peer(self: &Arc<Self>, id: &str, name: &str, address: &str, send: String, expect: String) -> Result<Peer, String> {
        {
            // Held throughout, so two pairings with the same Mac at once both keep their token.
            let mut all = self.tokens.lock().map_err(|_| "link state poisoned")?;
            let now = Instant::now();
            let mut tokens = Tokens { send, paired: now, expect: Vec::new() };
            if let Some(old) = all.remove(id) {
                if now.duration_since(old.paired) < PAIR_WINDOW {
                    // A crossed pairing: the other Mac may already be using the token sent from here.
                    (tokens.send, tokens.paired) = (old.send, old.paired);
                }
                tokens.expect = old.expect.into_iter().filter(|e| now < e.expires).collect();
            }
            tokens.expect.push(Expected { token: expect, expires: now + PAIR_WINDOW, pinned: false });
            self.secrets.set(&send_key(id), &tokens.send)?;
            self.secrets.set(&expect_key(id), &tokens.expect_text())?;
            save_peer(&self.db, id, name, address)?;
            all.insert(id.to_string(), tokens);
        }
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
        match self.add_peer(&request.install_id, &name, &address, request.token_for_you, token_for_me.clone()) {
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
        let server = self.server.lock().map_err(|_| "link state poisoned")?.clone();
        let not_listening = || server.error.clone().unwrap_or_else(|| "This Mac isn't on a tailnet yet. Connect Tailscale, then try again.".into());
        let mine = server.addresses.first().cloned().ok_or_else(not_listening)?;
        let token_for_you = new_token();
        let body = json!({ "code": code, "installId": self.install_id, "name": self.name, "address": mine, "tokenForYou": token_for_you });
        let reply = match self.call(&address, None, "POST", "/v1/pair", Some(&body), CALL_TIMEOUT).await {
            Ok(reply) => reply,
            Err(CallError::Offline) => return Err(format!("Couldn't reach Grimoire at {address}. Check that it's open there and on the same tailnet.")),
            Err(CallError::Refused(_, why) | CallError::Untrusted(why)) => return Err(why),
        };
        let reply: PairReply = serde_json::from_value(reply).map_err(|_| "The other Mac sent an odd answer.")?;
        let name = clean_mac_name(&reply.name);
        let ok = valid_id(&reply.install_id) && reply.install_id != self.install_id && valid_token(&reply.token_for_me);
        let Some(name) = name.filter(|_| ok) else { return Err("The other Mac sent an odd answer.".into()) };
        let peer = self.add_peer(&reply.install_id, &name, &address, reply.token_for_me, token_for_you)?;
        // Use the new token at once, so the other Mac pins it and drops any other from a crossed
        // pairing. The relay says hello too, but maybe later.
        let _ = self.peer_call(&peer.id, "GET", "/v1/hello", None, CALL_TIMEOUT).await;
        Ok(peer)
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

    /// The peer whose token this is. A token that works is pinned; see `Tokens`.
    fn peer_for(&self, authorization: Option<&str>) -> Option<String> {
        let token = authorization?.strip_prefix("Bearer ")?.trim();
        let mut tokens = self.tokens.lock().ok()?;
        // Compare with every token so the time doesn't say which one matched.
        let mut found = None;
        for (id, t) in tokens.iter() {
            for (i, expect) in t.expect.iter().enumerate() {
                if http::same_secret(&expect.token, token) {
                    found = Some((id.clone(), i));
                }
            }
        }
        let (peer, i) = found?;
        let t = tokens.get_mut(&peer)?;
        let now = Instant::now();
        if !t.valid(i, now) {
            return None;
        }
        if t.expect.len() > 1 || !t.expect[i].pinned {
            t.pin(i);
            let _ = self.secrets.set(&expect_key(&peer), &t.expect_text());
        }
        Some(peer)
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
            // Every connection starts in the small pre-auth pool, so strangers can't fill the slots
            // linked Macs use.
            let Some(permit) = self.admit(from.ip()) else { continue };
            let link = self.clone();
            tokio::spawn(async move {
                if link.trusts(from.ip()).await {
                    link.answer(stream, permit).await;
                }
            });
        }
    }

    /// A place in the pre-auth pool for a connection from `ip`, while the pool and that address's
    /// share of it have room.
    fn admit(&self, ip: IpAddr) -> Option<PreAuth> {
        let ip = ip.to_canonical();
        let mut from = self.pre_auth_from.lock().ok()?;
        let held = from.get(&ip).copied().unwrap_or(0);
        if held >= PRE_AUTH_PER_ADDRESS {
            return None;
        }
        let permit = self.pre_auth.clone().try_acquire_owned().ok()?;
        from.insert(ip, held + 1);
        Some(PreAuth { _permit: permit, ip, from: self.pre_auth_from.clone() })
    }

    async fn answer(self: &Arc<Self>, mut stream: TcpStream, pre_auth: PreAuth) {
        let mut pre_auth = Some(pre_auth);
        let mut paired: Option<OwnedSemaphorePermit> = None;
        let authorized = std::sync::atomic::AtomicBool::new(false);
        // Only pairing is open; anything else needs a token before its body is read. A good token
        // moves the connection from the pre-auth pool to the linked Macs' pool.
        let gate = |head: &http::Head| {
            if head.path == "/v1/pair" {
                return Ok(());
            }
            if self.peer_for(head.authorization.as_deref()).is_none() {
                return Err(("401 Unauthorized", "This Mac isn't linked. Pair it again."));
            }
            match self.connections.clone().try_acquire_owned() {
                Ok(permit) => {
                    paired = Some(permit);
                    pre_auth = None;
                    authorized.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                }
                Err(_) => Err(("503 Service Unavailable", "This Mac is busy. Try again in a moment.")),
            }
        };
        // A request without a token gets a short time to arrive; one with a good token gets longer.
        let started = Instant::now();
        let read = {
            let read = http::read_request(&mut stream, BODY_LIMIT, gate);
            tokio::pin!(read);
            match tokio::time::timeout_at(started + PRE_AUTH_READ_TIMEOUT, &mut read).await {
                Err(_) if authorized.load(std::sync::atomic::Ordering::Relaxed) => tokio::time::timeout_at(started + READ_TIMEOUT, &mut read).await,
                read => read,
            }
        };
        let (status, body) = match read {
            Ok(Ok(request)) => {
                let (status, value) = self.route(request).await;
                (status_line(status), value.to_string())
            }
            Ok(Err(Some((status, text)))) => (status, error(text).to_string()),
            Ok(Err(None)) | Err(_) => return,
        };
        let _ = tokio::time::timeout(WRITE_TIMEOUT, http::reply(&mut stream, status, "application/json", body.as_bytes())).await;
        drop((pre_auth, paired));
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
                // A linked Mac can only summon familiars that stay in their workspace. Full access is
                // granted on the familiar's home Mac.
                Ok(input) if input.access != Access::Workspace => {
                    Err("A familiar summoned from another Mac gets workspace access. Change it on its home Mac if it needs more.".into())
                }
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
            ("POST", ["handoffs"]) => return self.handoff_here(&peer, &request.body, std::time::Instant::now()),
            ("POST", ["handoffs", thread, "reply"]) => self.reply_here(&peer, thread, &request.body),
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

    /// A familiar on `peer` asks or messages one here. The exchange runs in the background and its
    /// reply goes back to `peer` alone, with this Mac's token for it. Each linked Mac gets at most
    /// HANDOFFS_IN_FLIGHT running here and HANDOFFS_PER_PEER in 10 minutes; past either, 429.
    fn handoff_here(self: &Arc<Self>, peer: &str, body: &[u8], now: std::time::Instant) -> (u16, Value) {
        let request: crate::delegation::Incoming = match serde_json::from_slice(body) {
            Ok(request) => request,
            Err(e) => return (400, error(&format!("Bad handoff: {e}"))),
        };
        let Some(running) = Running::start(&self.running_from, peer) else {
            let busy = format!("{} is already working on {HANDOFFS_IN_FLIGHT} requests from your Mac. Wait for a reply, then try again.", self.name);
            return (429, error(&busy));
        };
        if !self.handoffs_from.under(peer, HANDOFFS_PER_PEER, now) {
            let many = format!(
                "Your Mac sent {} {HANDOFFS_PER_PEER} requests in the last {} minutes, which is Grimoire's limit. Wait a few minutes, then try again.",
                self.name,
                crate::delegation::WINDOW.as_secs() / 60
            );
            return (429, error(&many));
        }
        let started = peer_row(&self.db, peer).and_then(|row| {
            let mac = row.map_or_else(|| "a linked Mac".into(), |p| p.name);
            self.local.handoff(&self.install_id, peer, &mac, &request)
        });
        let (thread, turn) = match started {
            Ok(started) => started,
            Err(e) => return (400, error(&e)),
        };
        let (link, peer, reply_to) = (self.clone(), peer.to_string(), thread.clone());
        tokio::spawn(async move {
            let outcome = turn.await;
            drop(running);
            link.send_reply(&peer, &reply_to, request.request_id, outcome).await;
        });
        (200, json!({ "threadId": thread }))
    }

    /// `peer` brings back the reply to a handoff a familiar here sent it.
    fn reply_here(&self, peer: &str, thread: &str, body: &[u8]) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Reply {
            request_id: i64,
            status: String,
            text: String,
        }
        if !valid_id(thread) {
            return Err("Bad reply.".into());
        }
        let reply: Reply = serde_json::from_slice(body).map_err(|_| "Bad reply.")?;
        let outcome = match reply.status.as_str() {
            "done" => TurnOutcome::Done(reply.text),
            "stopped" => TurnOutcome::Stopped,
            "failed" => TurnOutcome::Failed(reply.text),
            _ => return Err("Bad reply.".into()),
        };
        self.local.handoff_reply(peer, thread, reply.request_id, outcome).map(|_| json!({}))
    }

    /// Takes a handoff's reply back to the Mac that asked, trying again while it's offline or busy
    /// (503). A Mac that refuses it otherwise, or unlinked this one (401, 403, 404), won't take it
    /// later either.
    async fn send_reply(&self, peer: &str, thread: &str, request: i64, outcome: TurnOutcome) {
        let (status, text) = match outcome {
            TurnOutcome::Done(text) => ("done", text),
            TurnOutcome::Stopped => ("stopped", String::new()),
            TurnOutcome::Failed(reason) => ("failed", reason),
        };
        let body = json!({ "requestId": request, "status": status, "text": text });
        let target = format!("/v1/handoffs/{thread}/reply");
        let mut wait = REPLY_BACKOFF_MIN;
        for _ in 0..REPLY_TRIES {
            match self.peer_call(peer, "POST", &target, Some(&body), CALL_TIMEOUT).await {
                Ok(_) => return,
                Err(CallError::Refused(status, why)) if status != 503 => return eprintln!("A linked Mac refused a handoff's reply: {why}"),
                Err(_) => {
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(REPLY_BACKOFF_MAX);
                }
            }
        }
        eprintln!("Gave up bringing a handoff's reply back to a linked Mac that stayed offline.");
    }

    /// Tells each Mac that asked something a restart here cut off that no reply is coming.
    fn reply_cut_off(self: &Arc<Self>, cut_off: Vec<crate::handoffs::CutOff>) {
        for request in cut_off {
            let (link, reason) = (self.clone(), format!("Grimoire on {} closed before it finished. Ask again if you still need it.", self.name));
            tokio::spawn(async move { link.send_reply(&request.peer, &request.thread, request.request, TurnOutcome::Failed(reason)).await });
        }
    }

    async fn events_here(&self, query: &str) -> Value {
        let Some(after) = http::query_param(query, "after").and_then(|v| v.parse::<u64>().ok()) else {
            return json!({ "seq": self.events.head(), "epoch": self.events.epoch, "events": [], "reset": false });
        };
        let wait = http::query_param(query, "wait").and_then(|v| v.parse::<u64>().ok()).map_or(LONG_POLL, Duration::from_secs).min(LONG_POLL);
        let batch = self.events.wait(after, wait).await;
        json!({ "seq": batch.seq, "epoch": self.events.epoch, "events": batch.events, "reset": batch.reset })
    }

    /// Binds the link port on the addresses Tailscale reports for this Mac, and checks again every
    /// minute, which also refreshes the list of trusted peers.
    async fn listen(self: Arc<Self>) {
        let mut bound: HashMap<IpAddr, AbortHandle> = HashMap::new();
        loop {
            self.rebind(&mut bound).await;
            tokio::time::sleep(BIND_RETRY).await;
        }
    }

    /// One pass of `listen`: binds the addresses Tailscale reports and lets go of the rest.
    async fn rebind(self: &Arc<Self>, bound: &mut HashMap<IpAddr, AbortHandle>) {
        let checked = self.check_tailnet().await;
        let found = checked.clone().unwrap_or_default();
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
        let error = if checked.is_none() {
            Some(NO_CLI.to_string())
        } else if found.is_empty() {
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
    }

    // ----- Client -----

    async fn call(&self, address: &str, token: Option<&str>, method: &str, target: &str, body: Option<&Value>, timeout: Duration) -> Result<Value, CallError> {
        tokio::time::timeout(timeout, self.request(address, token, method, target, body)).await.unwrap_or(Err(CallError::Offline))
    }

    async fn request(&self, address: &str, token: Option<&str>, method: &str, target: &str, body: Option<&Value>) -> Result<Value, CallError> {
        let candidates = resolve(address).await;
        if candidates.is_empty() {
            return Err(CallError::Offline);
        }
        // A token or pairing code goes only to an address Tailscale lists as a peer.
        let mut to = None;
        for candidate in candidates {
            if self.trusts(candidate.ip()).await {
                to = Some(candidate);
                break;
            }
        }
        let Some(to) = to else {
            let cli = self.view.lock().is_ok_and(|slot| slot.0.cli);
            return Err(if cli { untrusted(address) } else { CallError::Untrusted(NO_CLI.into()) });
        };
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
            CallError::Refused(_, why) | CallError::Untrusted(why) => why,
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

    fn cursor(&self, peer: &str) -> Option<(Value, u64)> {
        self.live.lock().ok()?.get(peer)?.cursor.clone()
    }

    fn set_cursor(&self, peer: &str, cursor: (Value, u64)) {
        if let Ok(mut live) = self.live.lock() {
            live.entry(peer.to_string()).or_default().cursor = Some(cursor);
        }
    }

    /// Connects to a peer and relays its events until a call fails. A reconnect picks up after the
    /// last event relayed, so a short drop loses nothing.
    async fn follow(&self, peer: &str, backoff: &mut Duration) -> CallError {
        let hello = match self.peer_call(peer, "GET", "/v1/hello", None, CALL_TIMEOUT).await {
            Ok(hello) => hello,
            Err(e) => return e,
        };
        let (mut epoch, mut seq) = match self.cursor(peer) {
            Some(cursor) => cursor,
            None => match self.peer_call(peer, "GET", "/v1/events", None, CALL_TIMEOUT).await {
                Ok(start) => (start["epoch"].clone(), start["seq"].as_u64().unwrap_or(0)),
                Err(e) => return e,
            },
        };
        if let Err(e) = self.refresh(peer).await {
            return e;
        }
        self.set_cursor(peer, (epoch.clone(), seq));
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
                // It restarted or this Mac fell behind, so some messages never arrived. Start from now
                // with a fresh list, and have the window reload every chat from that Mac. The cursor
                // moves last, so a reconnect after a failed refresh sees the gap again.
                if let Err(e) = self.refresh(peer).await {
                    return e;
                }
                self.ui.event("link_reload", json!({ "agentId": null, "peerId": peer }));
                (epoch, seq) = (batch["epoch"].clone(), next);
                self.set_cursor(peer, (epoch.clone(), seq));
                continue;
            }
            seq = next;
            let mut changed = false;
            for event in batch["events"].as_array().into_iter().flatten() {
                changed |= self.relay_event(peer, event);
            }
            self.set_cursor(peer, (epoch.clone(), seq));
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
            // The home Mac switched the familiar to another chat, so the open transcript is stale.
            "chat" => "link_reload",
            _ => return false,
        };
        let mut payload = event["payload"].clone();
        let Some(agent) = payload["agentId"].as_str().filter(|id| valid_id(id)).map(String::from) else { return false };
        if kind == "chat" {
            payload = json!({ "agentId": agent });
        }
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

    /// Every linked Mac's familiars as last fetched, for familiars here to ask or message.
    pub fn linked(&self) -> Vec<Linked> {
        let mut all = Vec::new();
        for row in peer_rows(&self.db).unwrap_or_default() {
            let online = self.online(&row.id);
            for familiar in &row.familiars {
                let (Some(agent), Some(name)) = (familiar["id"].as_str().filter(|id| valid_id(id)), familiar["name"].as_str()) else { continue };
                let text = |key: &str| familiar[key].as_str().map(String::from);
                all.push(Linked {
                    id: remote_id(&row.id, agent),
                    peer: row.id.clone(),
                    agent: agent.into(),
                    name: name.into(),
                    mac: row.name.clone(),
                    online,
                    provider: text("provider"),
                    model: text("activeModel").or_else(|| text("model")),
                });
            }
        }
        all
    }

    /// This Mac's name, as linked Macs show it.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn install_id(&self) -> &str {
        &self.install_id
    }

    /// Sends a handoff (see `delegation::away`) to `target`'s home Mac and returns its thread id there.
    pub async fn hand_off(&self, target: &Linked, body: &Value) -> Result<String, String> {
        match self.peer_call(&target.peer, "POST", "/v1/handoffs", Some(body), CALL_TIMEOUT).await {
            Ok(reply) => reply["threadId"].as_str().filter(|t| valid_id(t)).map(String::from).ok_or_else(|| "The other Mac sent an odd answer.".into()),
            Err(CallError::Offline) => Err(format!("{} is offline, so {} can't be reached right now.", target.mac, target.name)),
            Err(CallError::Refused(_, why) | CallError::Untrusted(why)) => Err(why),
        }
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
        503 => "503 Service Unavailable",
        _ => "500 Internal Server Error",
    }
}

/// Where `address` may be on the link port. A typed port wins.
async fn resolve(address: &str) -> Vec<SocketAddr> {
    if let Ok(with_port) = address.parse::<SocketAddr>() {
        vec![with_port]
    } else if let Ok(ip) = address.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
        vec![SocketAddr::new(ip, PORT)]
    } else {
        tokio::net::lookup_host((address, PORT)).await.map(|found| found.collect()).unwrap_or_default()
    }
}

// ---------- The app's side ----------

/// Runs a linked Mac's requests through this Mac's runtime.
struct AppLocal {
    app: tauri::AppHandle,
    db: Arc<Db>,
    runtime: Arc<crate::runtime::Runtime>,
    approvals: Arc<crate::approvals::Approvals>,
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

    fn handoff(&self, here: &str, peer: &str, mac: &str, request: &crate::delegation::Incoming) -> Result<(String, BoxFuture<'static, TurnOutcome>), String> {
        let familiars = Arc::new(self.familiars());
        crate::delegation::incoming(&self.db, &crate::delegation::CHAINS, familiars, here, peer, mac, request)
    }

    fn handoff_reply(&self, peer: &str, thread: &str, request: i64, outcome: TurnOutcome) -> Result<(), String> {
        crate::delegation::away_reply(&self.db, &crate::delegation::WAITERS, &self.familiars(), peer, thread, request, outcome)
    }
}

impl AppLocal {
    fn familiars(&self) -> crate::delegation::AppFamiliars {
        crate::delegation::AppFamiliars { app: self.app.clone(), db: self.db.clone(), runtime: self.runtime.clone(), approvals: self.approvals.clone() }
    }
}

/// Starts the link: the server on the tailnet, a relay for each linked Mac, failed replies for
/// `cut_off` requests, and a sweep for replies that never came.
pub fn install(
    app: &tauri::AppHandle,
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    runtime: Arc<crate::runtime::Runtime>,
    approvals: Arc<crate::approvals::Approvals>,
    cut_off: Vec<crate::handoffs::CutOff>,
) -> Result<(), String> {
    use tauri::Listener;
    let local = Arc::new(AppLocal { app: app.clone(), db: db.clone(), runtime, approvals });
    let link = Link::new(db.clone(), secrets, local.clone(), Arc::new(app.clone()), Arc::new(TailscaleCli), mac_name())?;
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
        link.reply_cut_off(cut_off);
        link.listen().await;
    });
    tauri::async_runtime::spawn(async move {
        let familiars = local.familiars();
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if let Err(e) = crate::delegation::expire(&db, &crate::delegation::WAITERS, &familiars, std::time::Instant::now()) {
                eprintln!("Couldn't close requests to linked Macs that got no reply: {e}");
            }
        }
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

/// The link, once it runs.
pub fn current() -> Option<Arc<Link>> {
    LINK.get().cloned()
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
    use crate::delegation::{away, AwayOut, Chains, Familiars, Kind, Limiter, Reach, Waiters};
    use crate::vault::MemoryStore;

    struct FakeLocal {
        db: Arc<Db>,
        sent: Mutex<Vec<(String, String, String)>>,
        created: Mutex<Vec<NewFamiliar>>,
        familiars: Arc<FakeFamiliars>,
        chains: &'static Chains,
        waiters: Waiters,
    }

    /// Familiars that answer at once with who asked what, and keep what they deliver and link.
    struct FakeFamiliars {
        db: Arc<Db>,
        delivered: Mutex<Vec<(String, String, String)>>,
        rows: Mutex<Vec<(String, String, String)>>,
        /// What the person here was asked, and how they answer.
        asked: Mutex<Vec<crate::approvals::Request>>,
        answer: Mutex<crate::approvals::Decision>,
        /// Holds each turn until it's let go, when set.
        hold: Mutex<Option<Arc<tokio::sync::Notify>>>,
    }

    impl Familiars for FakeFamiliars {
        fn approve(&self, request: crate::approvals::Request) -> BoxFuture<'static, crate::approvals::Decision> {
            self.asked.lock().unwrap().push(request);
            let answer = *self.answer.lock().unwrap();
            Box::pin(async move { answer })
        }
        fn side_turn(&self, chat_id: &str, handoff: i64, asker: &str, message: &str) -> BoxFuture<'static, TurnOutcome> {
            let (db, chat, reply) = (self.db.clone(), chat_id.to_string(), format!("{asker} asked: {message}"));
            let hold = self.hold.lock().unwrap().clone();
            Box::pin(async move {
                if let Some(hold) = hold {
                    hold.notified().await;
                }
                db.insert_message_in(&chat, &crate::side::get(&db, &chat).unwrap().unwrap().target_id, "assistant", &reply, "done", None).unwrap();
                crate::handoffs::set(&db, handoff, crate::handoffs::Status::Done).unwrap();
                TurnOutcome::Done(reply)
            })
        }
        fn deliver(&self, asker_id: &str, _: Option<String>, text: String, source: &str) {
            self.delivered.lock().unwrap().push((asker_id.into(), text, source.into()));
        }
        fn link_row(&self, _: Option<&str>, agent_id: &str, label: &str, source: &str) -> Result<(), String> {
            self.rows.lock().unwrap().push((agent_id.into(), label.into(), source.into()));
            Ok(())
        }
        fn changed(&self) {}
    }

    impl Local for FakeLocal {
        fn handoff(&self, here: &str, peer: &str, mac: &str, request: &crate::delegation::Incoming) -> Result<(String, BoxFuture<'static, TurnOutcome>), String> {
            crate::delegation::incoming(&self.db, self.chains, self.familiars.clone(), here, peer, mac, request)
        }
        fn handoff_reply(&self, peer: &str, thread: &str, request: i64, outcome: TurnOutcome) -> Result<(), String> {
            crate::delegation::away_reply(&self.db, &self.waiters, self.familiars.as_ref(), peer, thread, request, outcome)
        }
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
        fn create(&self, input: NewFamiliar) -> BoxFuture<'_, Result<Value, String>> {
            self.created.lock().unwrap().push(input);
            Box::pin(async { Ok(json!({ "id": "made" })) })
        }
    }

    /// Tailscale as the tests need it: a status to report, or none for a Mac without the CLI.
    struct FakeTailnet {
        status: Mutex<Option<String>>,
        asked: Mutex<usize>,
    }

    impl Tailnet for FakeTailnet {
        fn status(&self) -> BoxFuture<'_, Result<Option<String>, NoCli>> {
            *self.asked.lock().unwrap() += 1;
            let status = self.status.lock().unwrap().clone();
            Box::pin(async move { status.map(Some).ok_or(NoCli) })
        }
    }

    fn status_json(own: &[&str], peers: &[&str]) -> String {
        let peers: serde_json::Map<String, Value> = peers.iter().enumerate().map(|(i, ip)| (format!("nodekey:{i}"), json!({ "TailscaleIPs": [ip] }))).collect();
        json!({ "BackendState": "Running", "Self": { "TailscaleIPs": own }, "Peer": peers }).to_string()
    }

    fn fake_tailnet(status: Option<String>) -> Arc<FakeTailnet> {
        Arc::new(FakeTailnet { status: Mutex::new(status), asked: Mutex::new(0) })
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

    /// A Mac whose Tailscale lists loopback as itself and as its one peer, so test Macs can talk.
    fn mac(name: &str) -> Mac {
        mac_on(name, fake_tailnet(Some(status_json(&["127.0.0.1"], &["127.0.0.1"]))))
    }

    fn mac_on(name: &str, tailnet: Arc<FakeTailnet>) -> Mac {
        let db = Arc::new(Db::open_in_memory().unwrap());
        mac_with(name, db, Arc::new(MemoryStore::new()), tailnet)
    }

    fn mac_with(name: &str, db: Arc<Db>, secrets: Arc<MemoryStore>, tailnet: Arc<FakeTailnet>) -> Mac {
        let familiars = Arc::new(FakeFamiliars {
            db: db.clone(),
            delivered: Mutex::new(Vec::new()),
            rows: Mutex::new(Vec::new()),
            asked: Mutex::new(Vec::new()),
            answer: Mutex::new(crate::approvals::Decision::Once),
            hold: Mutex::new(None),
        });
        let local = Arc::new(FakeLocal {
            db: db.clone(),
            sent: Mutex::new(Vec::new()),
            created: Mutex::new(Vec::new()),
            familiars,
            chains: Box::leak(Box::default()),
            waiters: Waiters::default(),
        });
        let ui = Arc::new(Events::default());
        let link = Link::new(db.clone(), secrets.clone(), local.clone(), ui.clone(), tailnet, name.into()).unwrap();
        Mac { link, db, secrets, local, ui }
    }

    fn events_named(mac: &Mac, name: &str) -> Vec<Value> {
        mac.ui.0.lock().unwrap().iter().filter(|(n, _)| n == name).map(|(_, p)| p.clone()).collect()
    }

    async fn eventually(what: &str, check: impl Fn() -> bool) {
        let wait = async {
            while !check() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait).await.unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    fn expected(token: &str, expires: Instant) -> Expected {
        Expected { token: token.into(), expires, pinned: false }
    }

    fn tokens(send: &str, expect: &[&str]) -> Tokens {
        Tokens { send: send.into(), paired: Instant::now(), expect: expect.iter().map(|t| expected(t, Instant::now() + PAIR_WINDOW)).collect() }
    }

    /// A stand-in for a linked Mac that answers each path, and how many times it was asked, as
    /// `answer` says.
    async fn fake_peer(answer: impl Fn(&str, usize) -> (u16, Value) + Send + Sync + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (answer, asked) = (Arc::new(answer), Arc::new(Mutex::new(HashMap::<String, usize>::new())));
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (answer, asked) = (answer.clone(), asked.clone());
                tokio::spawn(async move {
                    let Ok(request) = http::read_request(&mut stream, BODY_LIMIT, |_| Ok(())).await else { return };
                    let times = {
                        let mut asked = asked.lock().unwrap();
                        let times = asked.entry(request.head.path.clone()).or_default();
                        *times += 1;
                        *times
                    };
                    let (status, body) = answer(&request.head.path, times);
                    http::reply(&mut stream, status_line(status), "application/json", body.to_string().as_bytes()).await;
                });
            }
        });
        address
    }

    fn authorized(link: &Link, token: &str) -> bool {
        link.peer_for(Some(&format!("Bearer {token}"))).is_some()
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
        a.link.tokens.lock().unwrap().insert("b".into(), tokens("s", &[&"token-b".repeat(6)]));
        a.link.tokens.lock().unwrap().insert("c".into(), tokens("s", &[&"token-c".repeat(6)]));
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
    async fn resolves_addresses_on_the_link_port() {
        assert_eq!(resolve("100.64.0.2").await, ["100.64.0.2:47620".parse().unwrap()]);
        assert_eq!(resolve("[fd7a:115c:a1e0::2]").await, [format!("[fd7a:115c:a1e0::2]:{PORT}").parse().unwrap()]);
        assert_eq!(resolve("127.0.0.1:9").await, ["127.0.0.1:9".parse().unwrap()]);
    }

    #[test]
    fn reads_tailscale_status() {
        let text = status_json(&["100.106.62.29", "fd7a:115c:a1e0::e039:3e1e"], &["100.104.85.100", "fd7a:115c:a1e0::e801:5588"]);
        let status = parse_status(&text).unwrap();
        assert_eq!(status.own, ["100.106.62.29".parse::<IpAddr>().unwrap(), "fd7a:115c:a1e0::e039:3e1e".parse().unwrap()]);
        assert!(status.peers.contains(&"100.104.85.100".parse().unwrap()) && status.peers.contains(&"fd7a:115c:a1e0::e801:5588".parse().unwrap()));
        assert_eq!(status.peers.len(), 2);
        let stopped = parse_status(r#"{"BackendState":"Stopped","Self":{"TailscaleIPs":null},"Peer":null}"#).unwrap();
        assert!(stopped.own.is_empty() && stopped.peers.is_empty(), "a stopped Tailscale has no addresses");
        assert_eq!(parse_status("failed to connect to local tailscaled"), None);
        assert_eq!(parse_status("{}"), None);
    }

    #[test]
    fn only_listed_peers_inside_the_tailnet_range_pass() {
        let text = status_json(&["100.101.1.2", "192.168.1.5"], &["100.104.85.100", "10.0.0.5"]);
        let view = TailnetView::new(Ok(Some(&text)));
        assert_eq!(view.own, ["100.101.1.2".parse::<IpAddr>().unwrap()], "this Mac listens on tailnet addresses only");
        assert!(view.trusts("100.104.85.100".parse().unwrap()));
        assert!(view.trusts("::ffff:100.104.85.100".parse().unwrap()));
        assert!(!view.trusts("10.0.0.5".parse().unwrap()), "outside the tailnet's range even when listed");
        assert!(!view.trusts("100.72.9.9".parse().unwrap()), "carrier-grade NAT isn't the tailnet");
        assert!(!view.trusts("100.101.1.2".parse().unwrap()), "this Mac isn't its own peer");
        let silent = TailnetView::new(Ok(None));
        assert!(silent.cli && silent.own.is_empty() && !silent.trusts("100.104.85.100".parse().unwrap()));
        let missing = TailnetView::new(Err(NoCli));
        assert!(!missing.cli && !missing.trusts("100.104.85.100".parse().unwrap()), "no CLI, no peers");
    }

    #[tokio::test]
    async fn without_tailscales_cli_nothing_listens_and_no_token_leaves() {
        let a = mac_on("Studio", fake_tailnet(None));
        a.link.rebind(&mut HashMap::new()).await;
        let this = a.link.status().unwrap().this_mac;
        assert_eq!((this.listening, this.error.as_deref()), (false, Some(NO_CLI)));
        assert_eq!(a.link.start_pairing().err().as_deref(), Some(NO_CLI));
        assert_eq!(a.link.pair("100.104.85.100", "ABCDEFGH").await.unwrap_err(), NO_CLI);
        save_peer(&a.db, "mac-b", "Laptop", "100.104.85.100").unwrap();
        a.link.tokens.lock().unwrap().insert("mac-b".into(), tokens(&new_token(), &[&new_token()]));
        match a.link.peer_call("mac-b", "GET", "/v1/hello", None, CALL_TIMEOUT).await {
            Err(CallError::Untrusted(why)) => assert_eq!(why, NO_CLI),
            other => panic!("{other:?}"),
        }
        assert!(!a.link.trusts("100.104.85.100".parse().unwrap()).await, "a tailnet address alone isn't enough");
    }

    #[tokio::test]
    async fn runs_only_the_tailscale_apps_own_cli_once_codesign_vouches_for_the_app() {
        let dir = std::env::temp_dir().join(format!("grimoire-cli-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("Tailscale.app/Contents/MacOS")).unwrap();
        let app = std::fs::canonicalize(dir.join("Tailscale.app")).unwrap();
        let cli = app.join("Contents/MacOS/Tailscale");
        std::fs::write(&cli, "").unwrap();
        let checked = Mutex::new(Vec::new());
        let signed = |ok: bool| {
            let checked = &checked;
            move |path: PathBuf| {
                checked.lock().unwrap().push(path);
                std::future::ready(ok)
            }
        };
        assert_eq!(find_tailscale(&app, &cli, signed(true)).await, Ok(cli.clone()));
        assert_eq!(*checked.lock().unwrap(), vec![app.clone()], "codesign checks the app itself");
        assert_eq!(find_tailscale(&app, &cli, signed(false)).await, Err(NoCli), "an app Tailscale didn't sign is never run");

        // A link out of the app, or a file root owns elsewhere, is never the CLI.
        let impostor = dir.join("impostor");
        std::fs::write(&impostor, "").unwrap();
        let link_out = app.join("Contents/MacOS/Linked");
        std::os::unix::fs::symlink(&impostor, &link_out).unwrap();
        assert_eq!(tailscale_program(&app, &link_out), None);
        assert_eq!(tailscale_program(&app, Path::new("/bin/ls")), None);
        assert_eq!(tailscale_program(&app, &app.join("Contents/MacOS/missing")), None);
        assert_eq!(find_tailscale(&app, &impostor, signed(true)).await, Err(NoCli), "even with a good signature on the app");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rechecks_tailscale_for_a_new_address_at_most_every_few_seconds() {
        let tailnet = fake_tailnet(Some(status_json(&["100.101.1.2"], &[])));
        let a = mac_on("Studio", tailnet.clone());
        assert!(!a.link.trusts("100.104.0.7".parse().unwrap()).await);
        assert!(!a.link.trusts("100.104.0.8".parse().unwrap()).await);
        assert_eq!(*tailnet.asked.lock().unwrap(), 1, "the second stranger waits for the recheck");
        // The device joins; the listen loop's next check picks it up and moves the address.
        *tailnet.status.lock().unwrap() = Some(status_json(&["100.101.1.3"], &["100.104.0.7"]));
        assert_eq!(a.link.check_tailnet().await, Some(vec!["100.101.1.3".parse::<IpAddr>().unwrap()]));
        assert!(a.link.trusts("100.104.0.7".parse().unwrap()).await);
    }

    #[tokio::test]
    async fn sends_no_token_or_code_to_an_address_tailscale_does_not_list() {
        let b = mac_on("Laptop", fake_tailnet(Some(status_json(&["100.101.1.2"], &["100.104.85.100"]))));
        b.link.server.lock().unwrap().addresses = vec!["100.101.1.2".into()];
        let refused = b.link.pair("100.72.0.5", "ABCDEFGH").await.unwrap_err();
        assert!(refused.contains("doesn't list 100.72.0.5"), "{refused}");
        save_peer(&b.db, "mac-a", "Studio", "100.72.0.5").unwrap();
        b.link.tokens.lock().unwrap().insert("mac-a".into(), tokens(&new_token(), &[&new_token()]));
        assert!(matches!(b.link.peer_call("mac-a", "GET", "/v1/hello", None, CALL_TIMEOUT).await, Err(CallError::Untrusted(_))));
    }

    #[tokio::test]
    async fn the_server_drops_connections_from_addresses_tailscale_does_not_list() {
        let a = mac_on("Studio", fake_tailnet(Some(status_json(&["127.0.0.1"], &["100.104.85.100"]))));
        let address = listen(&a).await;
        let mut stream = TcpStream::connect(&address).await.unwrap();
        let _ = stream.write_all(b"POST /v1/pair HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}").await;
        let mut reply = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut reply)).await.unwrap();
        assert!(reply.is_empty(), "{}", String::from_utf8_lossy(&reply));
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
        let pinned = a.link.tokens.lock().unwrap()[&b.link.install_id].expect.iter().map(|e| e.pinned).collect::<Vec<_>>();
        assert_eq!(pinned, [true], "B used its token before pair returned, so A pinned it");
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

    /// Both halves of each pairing, in the orders two Macs pairing each other at once can run them.
    #[tokio::test]
    async fn review_crossed_pairing_keeps_both_credentials_valid() {
        for a_answers_first in [true, false] {
            for a_finishes_first in [true, false] {
                let (a, b) = (mac("Studio"), mac("Laptop"));
                let (a_id, b_id) = (a.link.install_id.clone(), b.link.install_id.clone());
                let (code_a, code_b) = (with_code(&a.link), with_code(&b.link));
                // A pairs with B's code and B with A's; each picks the token it expects back.
                let (a_expects, b_expects) = (new_token(), new_token());
                let request = |code: &str, from: &str, token: &str| {
                    json!({ "code": code, "installId": from, "name": "Mac", "address": "127.0.0.1", "tokenForYou": token }).to_string().into_bytes()
                };
                let answer = |by: &Mac, body: Vec<u8>| {
                    let (status, reply) = by.link.answer_pair(&body, Instant::now());
                    assert_eq!(status, 200, "{reply}");
                    reply["tokenForMe"].as_str().unwrap().to_string()
                };
                let (b_reply, a_reply) = if a_answers_first {
                    let a_reply = answer(&a, request(&code_a, &b_id, &b_expects));
                    (answer(&b, request(&code_b, &a_id, &a_expects)), a_reply)
                } else {
                    let b_reply = answer(&b, request(&code_b, &a_id, &a_expects));
                    (b_reply, answer(&a, request(&code_a, &b_id, &b_expects)))
                };
                let finish_a = || a.link.add_peer(&b_id, "Laptop", "127.0.0.1", b_reply.clone(), a_expects.clone()).unwrap();
                let finish_b = || b.link.add_peer(&a_id, "Studio", "127.0.0.1", a_reply.clone(), b_expects.clone()).unwrap();
                if a_finishes_first {
                    finish_a();
                    finish_b();
                } else {
                    finish_b();
                    finish_a();
                }
                let a_sends = a.link.tokens.lock().unwrap()[&b_id].send.clone();
                let b_sends = b.link.tokens.lock().unwrap()[&a_id].send.clone();
                assert_eq!((authorized(&b.link, &a_sends), authorized(&a.link, &b_sends)), (true, true), "answers first: {a_answers_first}, finishes first: {a_finishes_first}");
                // What the Keychain holds works after a restart too.
                let reloaded = mac_with("Studio", a.db.clone(), a.secrets.clone(), fake_tailnet(None));
                assert!(authorized(&reloaded.link, &b_sends));
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_macs_pairing_each_other_at_once_can_both_call() {
        for _ in 0..5 {
            let (a, b) = (mac("Studio"), mac("Laptop"));
            let (a_address, b_address) = (listen(&a).await, listen(&b).await);
            let (code_a, code_b) = (a.link.start_pairing().unwrap().code, b.link.start_pairing().unwrap().code);
            let (from_a, from_b) = tokio::join!(a.link.pair(&b_address, &code_b), b.link.pair(&a_address, &code_a));
            let (a_peer, b_peer) = (from_a.unwrap().id, from_b.unwrap().id);
            assert!(a.link.peer_call(&a_peer, "GET", "/v1/hello", None, CALL_TIMEOUT).await.is_ok());
            assert!(b.link.peer_call(&b_peer, "GET", "/v1/hello", None, CALL_TIMEOUT).await.is_ok());
        }
    }

    #[test]
    fn a_used_token_is_pinned_and_the_others_stop_working_at_once() {
        let a = mac("Studio");
        let (before, used, after) = (new_token(), new_token(), new_token());
        a.link.tokens.lock().unwrap().insert("b".into(), tokens("s", &[&before, &used, &after]));
        assert!(authorized(&a.link, &used));
        assert!(!authorized(&a.link, &before) && !authorized(&a.link, &after));
        assert_eq!(a.secrets.get(&expect_key("b")).unwrap().as_deref(), Some(used.as_str()), "the Keychain keeps the pinned one");
        // After a restart the pinned token alone works, long after the window.
        let reloaded = mac_with("Studio", a.db.clone(), a.secrets.clone(), fake_tailnet(None));
        reloaded.link.tokens.lock().unwrap().insert("b".into(), loaded_tokens("s".repeat(32), &a.secrets.get(&expect_key("b")).unwrap().unwrap()).unwrap());
        assert!(authorized(&reloaded.link, &used) && !authorized(&reloaded.link, &after));
    }

    #[test]
    fn tokens_nobody_used_stop_working_when_the_window_passes() {
        let a = mac("Studio");
        let (one, two) = (new_token(), new_token());
        let old = Instant::now().checked_sub(PAIR_WINDOW).unwrap();
        a.link.tokens.lock().unwrap().insert("b".into(), Tokens { send: "s".into(), paired: old, expect: vec![expected(&one, old), expected(&two, old)] });
        assert!(!authorized(&a.link, &one) && !authorized(&a.link, &two));
        // A link with one token keeps it, used or not.
        let only = new_token();
        a.link.tokens.lock().unwrap().insert("c".into(), Tokens { send: "s".into(), paired: old, expect: vec![expected(&only, old)] });
        assert!(authorized(&a.link, &only));
    }

    #[test]
    fn several_tokens_saved_by_a_crossed_pairing_load_and_the_first_used_wins() {
        let a = mac("Studio");
        let (one, two) = (new_token(), new_token());
        let saved = tokens("s", &[&one, &two]).expect_text();
        assert!(saved.starts_with(&format!("{one}:")) && saved.contains(&format!(" {two}:")), "{saved}");
        let loaded = loaded_tokens(new_token(), &saved).unwrap();
        assert!(loaded.expect.iter().all(|e| !e.pinned));
        a.link.tokens.lock().unwrap().insert("b".into(), loaded);
        assert!(authorized(&a.link, &two));
        assert!(!authorized(&a.link, &one));
        assert_eq!(a.secrets.get(&expect_key("b")).unwrap(), Some(two), "the pinned token is saved alone, as before");
    }

    #[test]
    fn a_restart_keeps_unused_tokens_original_expiry() {
        let a = mac("Studio");
        let (live, lapsed, legacy, only) = (new_token(), new_token(), new_token(), new_token());
        let wall = crate::db::now_ms();
        // Saved before a restart: one still inside its window, one whose window passed while Grimoire was closed.
        let saved = format!("{live}:{} {lapsed}:{}", wall + 60_000, wall - 1);
        let loaded = loaded_tokens(new_token(), &saved).unwrap();
        let left = loaded.expect[0].expires.saturating_duration_since(Instant::now());
        assert!(left > Duration::from_secs(50) && left <= Duration::from_secs(60), "{left:?}");
        a.link.tokens.lock().unwrap().insert("b".into(), loaded);
        assert!(!authorized(&a.link, &lapsed), "no fresh window after a restart");
        assert!(authorized(&a.link, &live));
        // Several tokens saved before expiries were: none gets a fresh window.
        a.link.tokens.lock().unwrap().insert("c".into(), loaded_tokens(new_token(), &format!("{legacy} {}", new_token())).unwrap());
        assert!(!authorized(&a.link, &legacy));
        // One token alone, the format every link used before, keeps working.
        a.link.tokens.lock().unwrap().insert("d".into(), loaded_tokens(new_token(), &only).unwrap());
        assert!(authorized(&a.link, &only));
    }

    #[tokio::test]
    async fn links_saved_before_crossed_pairings_still_load() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets = Arc::new(MemoryStore::new());
        let (send, expect) = (new_token(), new_token());
        save_peer(&db, "mac-b", "Laptop", "100.64.0.9").unwrap();
        secrets.set(&send_key("mac-b"), &send).unwrap();
        secrets.set(&expect_key("mac-b"), &expect).unwrap();
        let a = mac_with("Studio", db, secrets, fake_tailnet(None));
        assert_eq!(a.link.peer_for(Some(&format!("Bearer {expect}"))).as_deref(), Some("mac-b"));
        assert_eq!(a.link.tokens.lock().unwrap()["mac-b"].send, send);
        // A new pairing long after the old one replaces the old token.
        let fresh = new_token();
        a.link.add_peer("mac-b", "Laptop", "100.64.0.9", new_token(), fresh.clone()).unwrap();
        assert!(!authorized(&a.link, &expect) && authorized(&a.link, &fresh));
    }

    #[tokio::test]
    async fn linked_macs_summon_with_workspace_access_only_and_reach_nothing_else() {
        let a = mac("Studio");
        let token = new_token();
        a.link.tokens.lock().unwrap().insert("b".into(), tokens("s", &[&token]));
        let request = |method: &str, path: &str, body: Value| http::Request {
            head: http::parse_head(&format!("{method} {path} HTTP/1.1\r\nAuthorization: Bearer {token}")).unwrap(),
            body: body.to_string().into_bytes(),
        };
        let familiar = |access: &str| json!({ "name": "Scout", "provider": "claude", "access": access, "avatar": Avatar::for_seed("x") });
        let (status, reply) = a.link.route(request("POST", "/v1/familiars", familiar("full"))).await;
        assert_eq!(status, 400);
        assert!(reply["error"].as_str().unwrap().contains("workspace access"), "{reply}");
        assert!(a.local.created.lock().unwrap().is_empty(), "nothing was made");
        assert_eq!(a.link.route(request("POST", "/v1/familiars", familiar("workspace"))).await.0, 200);
        assert_eq!(a.local.created.lock().unwrap()[0].access, Access::Workspace);
        for (method, path) in [("GET", "/v1/vault"), ("POST", "/v1/approvals"), ("POST", "/v1/tools"), ("GET", "/v1/settings"), ("POST", "/v1/familiars/f1/access")] {
            assert_eq!(a.link.route(request(method, path, json!({}))).await.0, 404, "{method} {path}");
        }
    }

    #[tokio::test]
    async fn a_chat_switch_on_the_home_mac_reloads_that_chat() {
        let a = mac("Studio");
        assert!(!a.link.relay_event("mac-b", &json!({ "kind": "chat", "payload": { "agentId": "f1" } })));
        assert_eq!(events_named(&a, "link_reload"), [json!({ "agentId": "peer:mac-b:f1" })]);
    }

    /// Pairs B with A over loopback and waits until B follows A's events.
    async fn linked() -> (Mac, Mac, String) {
        let (a, b) = (mac("Studio"), mac("Laptop"));
        let a_address = listen(&a).await;
        listen(&b).await;
        a.db.insert_agent(&agent("f1", "Scribe")).unwrap();
        let code = a.link.start_pairing().unwrap().code;
        let peer = b.link.pair(&a_address, &code).await.unwrap().id;
        eventually("B to follow A", || b.link.online(&peer)).await;
        (a, b, peer)
    }

    fn stop_relay(mac: &Mac, peer: &str) {
        let relay = mac.link.live.lock().unwrap().get_mut(peer).and_then(|l| l.relay.take());
        relay.unwrap().abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reconnect_relays_what_it_missed_or_reloads_when_it_cannot() {
        let (a, b, peer) = linked().await;
        let remote = remote_id(&peer, "f1");

        // A short drop: the events wait in A's log and arrive on reconnect, with no reload.
        stop_relay(&b, &peer);
        a.link.events.push("message", json!({ "agentId": "f1", "id": 7, "text": "while you were out" }));
        b.link.start_relay(&peer);
        eventually("the missed message", || events_named(&b, "message").iter().any(|m| m["id"] == 7 && m["agentId"] == remote.as_str())).await;
        assert!(events_named(&b, "link_reload").is_empty());

        // A long drop: more than the log holds, so B reloads everything from A.
        stop_relay(&b, &peer);
        for i in 0..RING + 5 {
            a.link.events.push("delta", json!({ "agentId": "f1", "messageId": 8, "chunk": i.to_string() }));
        }
        b.link.start_relay(&peer);
        eventually("a reload after the overflow", || events_named(&b, "link_reload").iter().any(|r| r["agentId"].is_null() && r["peerId"] == peer.as_str())).await;

        // A restart: A's log has a new id, so its numbers mean nothing to B.
        stop_relay(&b, &peer);
        b.ui.0.lock().unwrap().clear();
        b.link.set_cursor(&peer, (json!("an-earlier-run"), 3));
        b.link.start_relay(&peer);
        eventually("a reload after the restart", || !events_named(&b, "link_reload").is_empty()).await;
        assert_eq!(events_named(&b, "link_reload")[0]["agentId"], Value::Null);
        assert!(events_named(&b, "delta").is_empty(), "nothing from the old numbering is relayed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_chat_switch_reaches_the_linked_mac_as_a_reload() {
        let (a, b, peer) = linked().await;
        a.link.events.push("chat", json!({ "agentId": "f1" }));
        let remote = remote_id(&peer, "f1");
        eventually("the reload", || events_named(&b, "link_reload").contains(&json!({ "agentId": remote }))).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connections_without_a_token_get_a_small_pool_a_small_share_and_little_time() {
        let (a, _b, _) = linked().await;
        let address = a.link.server.lock().unwrap().addresses[0].clone();
        // B's long poll holds a linked Mac's slot, not a pre-auth one.
        eventually("B's poll to leave the pre-auth pool", || a.link.pre_auth.available_permits() == PRE_AUTH_CONNECTIONS).await;
        assert!(a.link.connections.available_permits() < MAX_CONNECTIONS);

        let mut idle = Vec::new();
        for _ in 0..PRE_AUTH_PER_ADDRESS {
            idle.push(TcpStream::connect(&address).await.unwrap());
        }
        eventually("the idle connections to take their share", || a.link.pre_auth.available_permits() == PRE_AUTH_CONNECTIONS - PRE_AUTH_PER_ADDRESS).await;
        let mut one_more = TcpStream::connect(&address).await.unwrap();
        let mut reply = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(2), one_more.read_to_end(&mut reply)).await;
        assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "the third from one address is dropped at once, though the pool has room");
        assert!(a.link.connections.available_permits() > 0, "linked Macs' slots are untouched");

        // A connection that never sends its request is closed after a few seconds.
        let read = tokio::time::timeout(PRE_AUTH_READ_TIMEOUT + Duration::from_secs(3), idle[0].read_to_end(&mut reply)).await;
        assert!(read.is_ok(), "the idle connection was closed");
        eventually("the pool to empty", || a.link.pre_auth.available_permits() == PRE_AUTH_CONNECTIONS).await;
    }

    #[test]
    fn one_address_holds_at_most_two_pre_auth_slots() {
        let a = mac("Studio");
        let ip = |i: u8| IpAddr::V4(std::net::Ipv4Addr::new(100, 64, 0, i));
        let first: Vec<PreAuth> = (0..PRE_AUTH_PER_ADDRESS).map(|_| a.link.admit(ip(1)).unwrap()).collect();
        assert!(a.link.admit(ip(1)).is_none());
        assert!(a.link.admit("::ffff:100.64.0.1".parse().unwrap()).is_none(), "the same address written as IPv6");
        let others: Vec<PreAuth> = (2..=4).flat_map(|i| [a.link.admit(ip(i)).unwrap(), a.link.admit(ip(i)).unwrap()]).collect();
        assert_eq!((a.link.pre_auth.available_permits(), others.len()), (0, 6));
        assert!(a.link.admit(ip(9)).is_none(), "the pool is full");
        drop(first);
        assert!(a.link.admit(ip(1)).is_some(), "closed connections give their slots back");
        assert!(!a.link.pre_auth_from.lock().unwrap().contains_key(&ip(1)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reset_whose_refresh_fails_shows_again_on_the_next_connect() {
        let failing = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let fail = failing.clone();
        let address = fake_peer(move |path, times| match path {
            "/v1/hello" => (200, json!({ "name": "Studio" })),
            // The first list comes before the events; the next follows the reset.
            "/v1/familiars" if times > 1 && fail.load(std::sync::atomic::Ordering::SeqCst) => (500, error("Busy.")),
            "/v1/familiars" => (200, json!({ "familiars": [], "busy": [] })),
            _ => (200, json!({ "seq": 9, "epoch": "restarted", "events": [], "reset": false })),
        })
        .await;
        let b = mac("Laptop");
        save_peer(&b.db, "mac-a", "Studio", &address).unwrap();
        b.link.tokens.lock().unwrap().insert("mac-a".into(), tokens(&new_token(), &[&new_token()]));
        b.link.set_cursor("mac-a", (json!("earlier"), 3));
        assert!(matches!(b.link.follow("mac-a", &mut BACKOFF_MIN.clone()).await, CallError::Refused(500, _)));
        assert_eq!(b.link.cursor("mac-a"), Some((json!("earlier"), 3)), "the cursor waits for the refresh");
        assert!(events_named(&b, "link_reload").is_empty());

        failing.store(false, std::sync::atomic::Ordering::SeqCst);
        let link = b.link.clone();
        let follow = tokio::spawn(async move { link.follow("mac-a", &mut BACKOFF_MIN.clone()).await });
        eventually("the reload on the next connect", || !events_named(&b, "link_reload").is_empty()).await;
        eventually("the cursor to move", || b.link.cursor("mac-a") == Some((json!("restarted"), 9))).await;
        follow.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_linked_macs_familiars_show_as_offline_right_after_a_restart() {
        let (_a, b, peer) = linked().await;
        eventually("B to list Scribe", || b.link.remote_familiars().len() == 1).await;
        // B restarts and can't reach A yet.
        let restarted = mac_with("Laptop", b.db.clone(), b.secrets.clone(), fake_tailnet(Some(status_json(&["127.0.0.1"], &[]))));
        let listed = restarted.link.remote_familiars();
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0]["id"].as_str(), listed[0]["name"].as_str()), (Some(remote_id(&peer, "f1").as_str()), Some("Scribe")));
        assert_eq!(listed[0]["home"], json!({ "peerId": peer, "name": "Studio", "online": false }));
        assert!(restarted.link.linked().iter().all(|f| f.name == "Scribe" && !f.online));
        // Unlinking takes them away for good.
        restarted.link.forget(&peer).unwrap();
        assert!(restarted.link.remote_familiars().is_empty());
        let rows: i64 = b.db.with(|c| c.query_row("SELECT COUNT(*) FROM link_peers", [], |r| r.get(0))).unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_restart_on_the_home_mac_fails_the_request_the_other_mac_waits_on() {
        let (a, b, a_id) = linked_with_ada().await;
        // Ada on B messaged Scribe on A, and A quit before Scribe replied.
        let away = crate::handoffs::Away { thread: None, peer: &a_id, asker_id: "ada", target_id: "f1", target_name: "Scribe" };
        let request = crate::handoffs::record_away(&b.db, &away, "message", "Draft it", crate::handoffs::Status::Waiting).unwrap();
        let chat = crate::side::create_remote(&a.db, &b.link.install_id, "ada", "Ada", "f1", "Draft it").unwrap();
        let handoff = crate::handoffs::record(&a.db, &chat.id, "message", "Draft it", crate::handoffs::Status::Waiting).unwrap();
        crate::handoffs::set_remote_request(&a.db, handoff, request).unwrap();

        let cut_off = crate::handoffs::cut_off_remote(&a.db).unwrap();
        assert_eq!(cut_off, [crate::handoffs::CutOff { peer: b.link.install_id.clone(), thread: chat.id.clone(), request }]);
        crate::delegation::recover(&a.db).unwrap();
        assert!(crate::handoffs::cut_off_remote(&a.db).unwrap().is_empty(), "the restart closed it on A");
        a.link.reply_cut_off(cut_off);
        let heard = || b.local.familiars.delivered.lock().unwrap().iter().any(|(asker, text, _)| asker == "ada" && text.contains("Grimoire on Studio closed before it finished"));
        eventually("Ada to hear no reply is coming", heard).await;
        assert_eq!(crate::handoffs::list(&b.db, 10).unwrap().into_iter().find(|h| h.id == request).unwrap().status, crate::handoffs::Status::Failed);
    }

    // ----- Familiars asking familiars on another Mac -----

    fn never_asked(_: crate::approvals::Request) -> std::future::Ready<crate::approvals::Decision> {
        panic!("should not ask the person")
    }

    /// Ada on B ("Laptop") hands something to Scribe on A ("Studio").
    async fn hand(b: &Mac, kind: Kind, message: &str, thread: Option<String>) -> Result<String, String> {
        let target = b.link.linked().into_iter().find(|f| f.name == "Scribe").expect("B lists A's Scribe");
        let out = AwayOut { asker: agent("ada", "Ada"), target, message: message.into(), thread };
        let (chains, limiter) = (Chains::default(), Limiter::default());
        let reach = Reach { db: &b.db, chains: &chains, limiter: &limiter, waiters: &b.local.waiters, link: &b.link, familiars: b.local.familiars.as_ref() };
        away(&reach, out, kind, None, Some("home-chat".into()), crate::delegation::TIMEOUT, Instant::now().into_std(), never_asked).await
    }

    async fn linked_with_ada() -> (Mac, Mac, String) {
        let (a, b, peer) = linked().await;
        b.db.insert_agent(&agent("ada", "Ada")).unwrap();
        (a, b, peer)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ask_to_a_familiar_on_another_mac_returns_its_reply_and_shows_on_both_macs() {
        let (a, b, peer) = linked_with_ada().await;
        let reply = hand(&b, Kind::Ask, " Summarize the notes ", None).await.unwrap();
        assert!(reply.starts_with("Ada on Laptop asked: Summarize the notes\n\n(Conversation "), "{reply}");
        assert!(reply.contains("with Scribe on Studio"), "{reply}");

        // The home Mac ran it in Scribe's side chat, asked by Ada as a familiar on B, never as one of A's.
        let home = crate::handoffs::list(&a.db, 10).unwrap();
        assert_eq!(home.len(), 1);
        let thread = home[0].chat_id.clone();
        assert_eq!(crate::side::get(&a.db, &thread).unwrap().unwrap().asker_id, remote_id(&b.link.install_id, "ada"));
        let shown = (home[0].from_id.as_str(), home[0].from_name.as_str(), home[0].from_mac.as_deref(), home[0].to_id.as_str(), home[0].to_mac.as_deref());
        assert_eq!(shown, (remote_id(&b.link.install_id, "ada").as_str(), "Ada", Some("Laptop"), "f1", None));
        assert_eq!(home[0].status, crate::handoffs::Status::Done);
        assert!(a.db.get_agent("ada").unwrap().is_none(), "the asker isn't a familiar on A");
        let asked_as: Option<i64> = a.db.with(|c| c.query_row("SELECT remote_request FROM handoffs", [], |r| r.get(0))).unwrap();
        assert_eq!(asked_as, Some(crate::handoffs::list(&b.db, 10).unwrap()[0].id), "A keeps B's id for the request");

        // The asking Mac lists it too, with the thread on A, and its chat links there without opening it.
        let asked = crate::handoffs::list(&b.db, 10).unwrap();
        let shown = (asked[0].from_id.as_str(), asked[0].to_id.as_str(), asked[0].to_name.as_str(), asked[0].to_mac.as_deref(), asked[0].chat_id.as_str());
        assert_eq!(shown, ("ada", remote_id(&peer, "f1").as_str(), "Scribe", Some("Studio"), thread.as_str()));
        assert_eq!(asked[0].status, crate::handoffs::Status::Done);
        let rows = b.local.familiars.rows.lock().unwrap().clone();
        assert_eq!(rows, [("ada".to_string(), "Asked Scribe on Studio".to_string(), remote_id(&peer, &thread))]);

        // The thread id follows up in the same conversation on A.
        let again = hand(&b, Kind::Ask, "And the second part?", Some(thread.clone())).await.unwrap();
        assert!(again.contains(&format!("Conversation {thread} with")), "{again}");
        let texts: Vec<String> = crate::side::messages(&a.db, &thread).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(texts.len(), 2, "both replies are in the one conversation: {texts:?}");
        assert!(b.local.familiars.delivered.lock().unwrap().is_empty(), "the waiting call got each reply");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_message_to_a_familiar_on_another_mac_comes_back_as_a_new_message() {
        let (_a, b, _) = linked_with_ada().await;
        let sent = hand(&b, Kind::Message, "Draft the summary", None).await.unwrap();
        assert!(sent.starts_with("Sent to Scribe on Studio."), "{sent}");
        eventually("the reply", || !b.local.familiars.delivered.lock().unwrap().is_empty()).await;
        let (asker, text, source) = b.local.familiars.delivered.lock().unwrap()[0].clone();
        assert_eq!((asker.as_str(), source.as_str()), ("ada", "Reply from Scribe on Studio"));
        assert!(text.starts_with("Scribe on Studio replied to your message:\n\nAda on Laptop asked: Draft the summary"), "{text}");
        assert_eq!(crate::handoffs::list(&b.db, 10).unwrap()[0].status, crate::handoffs::Status::Done);
    }

    #[tokio::test]
    async fn a_familiar_on_an_offline_mac_fails_fast() {
        let b = mac("Laptop");
        b.db.insert_agent(&agent("ada", "Ada")).unwrap();
        save_peer(&b.db, "mac-a", "NigelBot", "127.0.0.1:1").unwrap();
        cache_familiars(&b.db, "mac-a", &[json!({ "id": "f1", "name": "Scribe" })]);
        b.link.tokens.lock().unwrap().insert("mac-a".into(), tokens(&new_token(), &[&new_token()]));
        let (chains, limiter) = (Chains::default(), Limiter::default());
        let reach = Reach { db: &b.db, chains: &chains, limiter: &limiter, waiters: &b.local.waiters, link: &b.link, familiars: b.local.familiars.as_ref() };
        let out = || AwayOut { asker: agent("ada", "Ada"), target: b.link.linked().remove(0), message: "hi".into(), thread: None };
        let offline = "NigelBot is offline, so Scribe can't be reached right now.";
        assert_eq!(away(&reach, out(), Kind::Ask, None, None, crate::delegation::TIMEOUT, Instant::now().into_std(), never_asked).await.unwrap_err(), offline);
        assert!(crate::handoffs::list(&b.db, 10).unwrap().is_empty(), "nothing went out");
        // Listed as online, but nobody answers there.
        b.link.set_online("mac-a", true);
        assert_eq!(away(&reach, out(), Kind::Message, None, None, crate::delegation::TIMEOUT, Instant::now().into_std(), never_asked).await.unwrap_err(), offline);
        assert_eq!(crate::handoffs::list(&b.db, 10).unwrap()[0].status, crate::handoffs::Status::Failed);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_chain_limit_and_loops_hold_across_macs() {
        let (a, b, peer) = linked_with_ada().await;
        let on_b = |id: &str| format!("{}:{id}", b.link.install_id);
        let body = |chain: Vec<String>| {
            json!({ "targetId": "f1", "asker": { "id": "ada", "name": "Ada", "mac": "Laptop" }, "message": "hi", "kind": "ask",
                    "depth": chain.len(), "chainIds": chain, "requestId": 1 })
        };
        // Three on B already waiting on each other, so Scribe would be a fourth.
        let deep = body(vec![on_b("x"), on_b("y"), on_b("ada")]);
        assert!(b.link.peer_call(&peer, "POST", "/v1/handoffs", Some(&deep), CALL_TIMEOUT).await.is_ok(), "three in a row is the limit, not over it");
        let deeper = body(vec![on_b("w"), on_b("x"), on_b("y"), on_b("ada")]);
        match b.link.peer_call(&peer, "POST", "/v1/handoffs", Some(&deeper), CALL_TIMEOUT).await {
            Err(CallError::Refused(400, why)) => assert!(why.contains("more than 3 familiars"), "{why}"),
            other => panic!("{other:?}"),
        }
        // B can't claim the asker is a familiar on some other Mac.
        let forged = json!({ "targetId": "f1", "asker": { "id": "ada", "name": "Ada", "mac": "Laptop" }, "message": "hi", "kind": "ask",
                             "depth": 1, "chainIds": [format!("{}:ada", a.link.install_id)], "requestId": 1 });
        assert!(matches!(b.link.peer_call(&peer, "POST", "/v1/handoffs", Some(&forged), CALL_TIMEOUT).await, Err(CallError::Refused(400, _))));

        // While a question from B to Scribe runs, the chain counts on A: Scribe asks Critic, and Critic
        // asking anyone would be a fourth.
        a.db.insert_agent(&agent("c", "Critic")).unwrap();
        a.db.insert_agent(&agent("e", "Editor")).unwrap();
        let request: crate::delegation::Incoming = serde_json::from_value(body(vec![on_b("x"), on_b("ada")])).unwrap();
        let (_, turn) = a.local.handoff(&a.link.install_id, &b.link.install_id, "Laptop", &request).unwrap();
        let chains = a.local.chains;
        let scribe_critic = chains.begin(&agent("f1", "Scribe"), &agent("c", "Critic")).unwrap();
        assert!(chains.begin(&agent("c", "Critic"), &agent("e", "Editor")).err().unwrap().contains("more than 3"));
        // And nobody in the chain can be asked back.
        let back: crate::delegation::Incoming = serde_json::from_value(json!({ "targetId": "f1", "asker": { "id": "z", "name": "Zed", "mac": "Laptop" },
            "message": "hi", "kind": "ask", "depth": 2, "chainIds": [format!("{}:f1", a.link.install_id), on_b("z")], "requestId": 2 })).unwrap();
        assert!(a.local.handoff(&a.link.install_id, &b.link.install_id, "Laptop", &back).err().unwrap().contains("waiting on you"));
        drop((scribe_critic, turn));
        assert!(chains.begin(&agent("c", "Critic"), &agent("e", "Editor")).is_ok(), "the chain ends with the question");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_the_mac_a_request_went_to_can_reply_to_it() {
        let (a, b, peer) = linked_with_ada().await;
        let c = mac("Other");
        listen(&c).await;
        let b_address = b.link.server.lock().unwrap().addresses[0].clone();
        let code = b.link.start_pairing().unwrap().code;
        let b_from_c = c.link.pair(&b_address, &code).await.unwrap().id;

        let row = crate::handoffs::Away { thread: None, peer: &peer, asker_id: "ada", target_id: "f1", target_name: "Scribe" };
        let id = crate::handoffs::record_away(&b.db, &row, "message", "hi", crate::handoffs::Status::Waiting).unwrap();
        let reply = json!({ "requestId": id, "status": "done", "text": "Forged" });
        match c.link.peer_call(&b_from_c, "POST", "/v1/handoffs/t1/reply", Some(&reply), CALL_TIMEOUT).await {
            Err(CallError::Refused(400, why)) => assert!(why.contains("isn't for a request"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(crate::handoffs::list(&b.db, 10).unwrap()[0].status, crate::handoffs::Status::Waiting);
        assert!(b.local.familiars.delivered.lock().unwrap().is_empty());

        // The Mac it went to can, once.
        let real = json!({ "requestId": id, "status": "done", "text": "Real" });
        assert!(a.link.peer_call(&b.link.install_id, "POST", "/v1/handoffs/t1/reply", Some(&real), CALL_TIMEOUT).await.is_ok());
        assert_eq!(b.local.familiars.delivered.lock().unwrap().len(), 1);
        assert!(a.link.peer_call(&b.link.install_id, "POST", "/v1/handoffs/t1/reply", Some(&real), CALL_TIMEOUT).await.is_err(), "a second reply is refused");
        // A reply names the thread the request went to.
        let other = crate::handoffs::record_away(&b.db, &crate::handoffs::Away { thread: Some("t9"), ..row }, "message", "hi", crate::handoffs::Status::Waiting).unwrap();
        let wrong_thread = json!({ "requestId": other, "status": "done", "text": "x" });
        assert!(a.link.peer_call(&b.link.install_id, "POST", "/v1/handoffs/t1/reply", Some(&wrong_thread), CALL_TIMEOUT).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handoff_requests_are_checked_before_anything_runs() {
        let (a, b, peer) = linked_with_ada().await;
        let on_b = format!("{}:ada", b.link.install_id);
        let post = |body: Value| {
            let b = &b.link;
            let peer = peer.clone();
            async move { b.peer_call(&peer, "POST", "/v1/handoffs", Some(&body), CALL_TIMEOUT).await }
        };
        let good = json!({ "targetId": "f1", "asker": { "id": "ada", "name": "Ada", "mac": "Laptop" }, "message": "hi", "kind": "ask", "depth": 1, "chainIds": [on_b], "requestId": 1 });
        for (field, value) in [("targetId", json!("../f1")), ("targetId", json!("nobody")), ("kind", json!("order")), ("message", json!("  ")), ("message", json!("x".repeat(MAX_BODY_TEST)))] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(post(bad).await.is_err(), "{field}");
        }
        let mut extra = good.clone();
        extra["tools"] = json!(["vault_use"]);
        assert!(post(extra).await.is_err(), "unknown fields are refused");
        // The who-can-ask setting on the home Mac applies too.
        crate::delegation::save(&a.db, crate::delegation::Settings { scope: crate::delegation::Scope::Nobody, approve: false }).unwrap();
        assert!(post(good.clone()).await.is_err());
        crate::delegation::save(&a.db, crate::delegation::Settings::default()).unwrap();
        // A thread another asker started can't be followed up.
        let theirs = crate::side::create_remote(&a.db, &b.link.install_id, "zed", "Zed", "f1", "x").unwrap();
        let mut follow = good.clone();
        follow["thread"] = json!(theirs.id);
        assert!(post(follow).await.is_err());
        assert!(crate::handoffs::list(&a.db, 10).unwrap().is_empty(), "nothing was recorded");
        assert!(post(good).await.is_ok());
    }

    const MAX_BODY_TEST: usize = 200_001;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_home_mac_asks_its_person_first_when_set_and_a_no_fails_the_ask() {
        let (a, b, _) = linked_with_ada().await;
        crate::delegation::save(&a.db, crate::delegation::Settings { scope: crate::delegation::Scope::Anyone, approve: true }).unwrap();
        *a.local.familiars.answer.lock().unwrap() = crate::approvals::Decision::Deny;
        let err = hand(&b, Kind::Ask, "Check the draft", None).await.unwrap_err();
        assert!(err.starts_with("Scribe on Studio couldn't reply: The person didn't let Scribe take this."), "{err}");
        let asked = a.local.familiars.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        let shown = (asked[0].agent_id.as_str(), asked[0].kind, asked[0].title.as_str(), asked[0].detail.as_str(), asked[0].can_remember);
        assert_eq!(shown, ("f1", "familiar", "Ada on Laptop wants to ask Scribe", "Check the draft", false));
        // Scribe never ran it, and both Macs list it as failed.
        let home = crate::handoffs::list(&a.db, 10).unwrap();
        assert_eq!(home[0].status, crate::handoffs::Status::Failed);
        let rows: Vec<(String, String)> = crate::side::messages(&a.db, &home[0].chat_id).unwrap().into_iter().map(|m| (m.role, m.text)).collect();
        assert_eq!(rows, [("user".into(), "Check the draft".into()), ("error".into(), "The person didn't allow this.".into())]);
        assert_eq!(crate::handoffs::list(&b.db, 10).unwrap()[0].status, crate::handoffs::Status::Failed);

        // A yes lets it through.
        *a.local.familiars.answer.lock().unwrap() = crate::approvals::Decision::Once;
        assert!(hand(&b, Kind::Ask, "Again", None).await.unwrap().starts_with("Ada on Laptop asked: Again"));
        assert_eq!(a.local.familiars.asked.lock().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_linked_mac_gets_three_handoffs_running_and_ten_in_ten_minutes() {
        let (a, b, peer) = linked_with_ada().await;
        let body = json!({ "targetId": "f1", "asker": { "id": "ada", "name": "Ada", "mac": "Laptop" }, "message": "hi", "kind": "message",
                           "depth": 1, "chainIds": [format!("{}:ada", b.link.install_id)], "requestId": 1 });
        let post = || b.link.peer_call(&peer, "POST", "/v1/handoffs", Some(&body), CALL_TIMEOUT);
        let idle = || a.link.running_from.lock().unwrap().is_empty();
        // Scribe's turns wait, so three stay running.
        let hold = Arc::new(tokio::sync::Notify::new());
        *a.local.familiars.hold.lock().unwrap() = Some(hold.clone());
        for _ in 0..HANDOFFS_IN_FLIGHT {
            assert!(post().await.is_ok());
        }
        match post().await {
            Err(CallError::Refused(429, why)) => assert_eq!(why, "Studio is already working on 3 requests from your Mac. Wait for a reply, then try again."),
            other => panic!("{other:?}"),
        }
        *a.local.familiars.hold.lock().unwrap() = None;
        eventually("the held turns to end", || {
            hold.notify_waiters();
            idle()
        })
        .await;
        // The refused one didn't count; seven more make ten.
        for _ in HANDOFFS_IN_FLIGHT..HANDOFFS_PER_PEER {
            assert!(post().await.is_ok());
            eventually("the turn to end", idle).await;
        }
        match post().await {
            Err(CallError::Refused(429, why)) => assert!(why.starts_with("Your Mac sent Studio 10 requests in the last 10 minutes"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(crate::handoffs::list(&a.db, 20).unwrap().len(), HANDOFFS_PER_PEER, "nothing over the limit ran");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reply_retries_a_busy_mac_and_stops_at_one_that_refuses_it() {
        let tries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = tries.clone();
        let busy_then_ok = fake_peer(move |_, times| {
            counted.store(times, std::sync::atomic::Ordering::SeqCst);
            if times <= 2 {
                (503, error("This Mac is busy. Try again in a moment."))
            } else {
                (200, json!({}))
            }
        })
        .await;
        let b = mac("Laptop");
        save_peer(&b.db, "mac-a", "Studio", &busy_then_ok).unwrap();
        b.link.tokens.lock().unwrap().insert("mac-a".into(), tokens(&new_token(), &[&new_token()]));
        tokio::time::timeout(Duration::from_secs(5), b.link.send_reply("mac-a", "t1", 1, TurnOutcome::Done("ok".into()))).await.unwrap();
        assert_eq!(tries.load(std::sync::atomic::Ordering::SeqCst), 3, "two 503s, then delivered");

        for status in [401, 403, 404] {
            let refused = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = refused.clone();
            let gone = fake_peer(move |_, times| {
                counted.store(times, std::sync::atomic::Ordering::SeqCst);
                (status, error("No."))
            })
            .await;
            save_peer(&b.db, "mac-a", "Studio", &gone).unwrap();
            b.link.send_reply("mac-a", "t1", 1, TurnOutcome::Done("ok".into())).await;
            assert_eq!(refused.load(std::sync::atomic::Ordering::SeqCst), 1, "{status} ends it");
        }
    }
}
