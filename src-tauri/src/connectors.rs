//! Connectors: hosted MCP servers, like Linear or Notion, that familiars use once the person signs in
//! in the browser. Grimoire identifies itself to each server without a developer account or an OAuth
//! app of anyone's own: by OAuth dynamic client registration, or by its Client ID Metadata Document,
//! whose URL is its client id at servers that support one.
//!
//! Tokens live in the vault's Keychain store and never leave the app. A familiar's CLI starts one
//! `grimoire mcp` relay per connector; the relay sends each message over the app's tool socket, and the
//! app adds the token when it forwards the message to the server. Which familiar is calling comes from
//! the socket token, never from what the relay says.

use crate::db::{now_ms, Agent, Db};
use crate::vault::SecretStore;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State, Url};
use tokio::io::AsyncWriteExt;

pub const CHANGED: &str = "connectors_changed";
/// How long the person has to finish signing in, in the browser.
const SIGN_IN_WAIT: Duration = Duration::from_secs(5 * 60);
/// A token this close to expiring is refreshed before use.
const REFRESH_EARLY_MS: i64 = 60_000;
/// Grimoire's Client ID Metadata Document, from docs/oauth/client-metadata.json. Its URL is Grimoire's
/// client id at sign-in servers that support these documents.
const CLIENT_METADATA_URL: &str = "https://dev-talix.github.io/grimoire/oauth/client-metadata.json";
/// Whether a server that offers both registration and the metadata document gets the document. Off
/// until the document is published at `CLIENT_METADATA_URL`.
const PREFER_CLIENT_METADATA: bool = false;
/// The browser comes back here, with the port Grimoire got for this sign-in after the host. The
/// metadata document lists it without a port, which allows any loopback port (RFC 8252, 7.3).
const REDIRECT_HOST: &str = "http://127.0.0.1";
const REDIRECT_PATH: &str = "/callback";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub description: &'static str,
    /// App names a template might use for this service, lowercase.
    pub apps: &'static [&'static str],
    /// Set when the server doesn't let apps register themselves, so the person pastes a token instead.
    pub token_help: Option<&'static str>,
}

const fn entry(id: &'static str, name: &'static str, url: &'static str, description: &'static str, apps: &'static [&'static str]) -> CatalogEntry {
    CatalogEntry { id, name, url, description, apps, token_help: None }
}

/// Hosted MCP servers that sign in through the browser with no developer app. Each one's OAuth
/// metadata offered a registration endpoint or Client ID Metadata Documents when it was last checked,
/// except the ones with `token_help`. Run `cargo test catalog_servers_offer_sign_in -- --ignored
/// --nocapture` to check them again.
pub const CATALOG: &[CatalogEntry] = &[
    entry("linear", "Linear", "https://mcp.linear.app/mcp", "Issues, projects, and cycles", &["linear"]),
    entry("notion", "Notion", "https://mcp.notion.com/mcp", "Pages, databases, and search", &["notion"]),
    entry("sentry", "Sentry", "https://mcp.sentry.dev/mcp", "Errors, issues, and releases", &["sentry"]),
    CatalogEntry {
        token_help: Some("GitHub doesn't let apps register themselves. Make a fine-grained personal access token at github.com/settings/personal-access-tokens and paste it here."),
        ..entry("github", "GitHub", "https://api.githubcopilot.com/mcp/", "Repositories, issues, and pull requests", &["github"])
    },
    entry("atlassian", "Atlassian", "https://mcp.atlassian.com/v1/mcp", "Jira issues and Confluence pages", &["atlassian", "jira", "confluence"]),
    entry("figma", "Figma", "https://mcp.figma.com/mcp", "Design files and comments", &["figma"]),
    entry("stripe", "Stripe", "https://mcp.stripe.com", "Payments, customers, and invoices", &["stripe"]),
    entry("vercel", "Vercel", "https://mcp.vercel.com", "Projects, deployments, and logs", &["vercel"]),
    entry("supabase", "Supabase", "https://mcp.supabase.com/mcp", "Databases, tables, and edge functions", &["supabase"]),
    entry("cloudflare", "Cloudflare", "https://mcp.cloudflare.com/mcp", "Workers, DNS, and the Cloudflare API", &["cloudflare"]),
    entry("airtable", "Airtable", "https://mcp.airtable.com/mcp", "Bases, tables, and records", &["airtable"]),
    entry("clickup", "ClickUp", "https://mcp.clickup.com/mcp", "Tasks, lists, and docs", &["clickup"]),
    entry("monday", "monday.com", "https://mcp.monday.com/mcp", "Boards, items, and updates", &["monday", "monday.com"]),
    entry("intercom", "Intercom", "https://mcp.intercom.com/mcp", "Conversations and contacts", &["intercom"]),
    entry("canva", "Canva", "https://mcp.canva.com/mcp", "Designs and brand assets", &["canva"]),
    entry("webflow", "Webflow", "https://mcp.webflow.com/mcp", "Sites, pages, and CMS items", &["webflow"]),
    entry("paypal", "PayPal", "https://mcp.paypal.com/mcp", "Invoices, orders, and disputes", &["paypal"]),
    entry("zapier", "Zapier", "https://mcp.zapier.com/api/mcp/mcp", "Actions in the apps you've connected to Zapier", &["zapier"]),
    entry("todoist", "Todoist", "https://ai.todoist.net/mcp", "Tasks, projects, and due dates", &["todoist"]),
    entry("miro", "Miro", "https://mcp.miro.com/", "Boards, diagrams, and sticky notes", &["miro"]),
    entry("calendly", "Calendly", "https://mcp.calendly.com", "Event types, bookings, and availability", &["calendly"]),
    entry("cal", "Cal.com", "https://mcp.cal.com/mcp", "Bookings, event types, and schedules", &["cal.com", "cal"]),
    entry("dropbox", "Dropbox", "https://mcp.dropbox.com/mcp", "Files, folders, and sharing", &["dropbox"]),
    entry("coda", "Coda", "https://coda.io/apis/mcp", "Docs, tables, and rows", &["coda"]),
    entry("gamma", "Gamma", "https://mcp.gamma.app/mcp", "Presentations, documents, and web pages", &["gamma"]),
    entry("granola", "Granola", "https://mcp.granola.ai/mcp", "Meeting notes and transcripts", &["granola"]),
    entry("fireflies", "Fireflies.ai", "https://api.fireflies.ai/mcp", "Meeting transcripts and summaries", &["fireflies", "fireflies.ai"]),
    entry("attio", "Attio", "https://mcp.attio.com/mcp", "Companies, people, and deals", &["attio"]),
    entry("close", "Close", "https://mcp.close.com/mcp", "Leads, contacts, and opportunities", &["close", "close crm", "close.com"]),
    entry("mailchimp", "Mailchimp", "https://mcp.mailchimp.com/mcp", "Audiences, campaigns, and reports", &["mailchimp"]),
    entry("klaviyo", "Klaviyo", "https://mcp.klaviyo.com/mcp", "Profiles, campaigns, and flows", &["klaviyo"]),
    entry("square", "Square", "https://mcp.squareup.com/mcp", "Payments, orders, and catalog items", &["square"]),
    entry("wix", "Wix", "https://mcp.wix.com/mcp", "Sites, stores, and bookings", &["wix"]),
    entry("dovetail", "Dovetail", "https://dovetail.com/api/mcp", "Research notes, insights, and highlights", &["dovetail"]),
    entry("gitlab", "GitLab", "https://gitlab.com/api/v4/mcp", "Projects, issues, and merge requests", &["gitlab"]),
    entry("netlify", "Netlify", "https://netlify-mcp.netlify.app/mcp", "Sites, deploys, and forms", &["netlify"]),
    entry("neon", "Neon", "https://mcp.neon.tech/mcp", "Postgres projects, branches, and queries", &["neon"]),
    entry("railway", "Railway", "https://mcp.railway.com", "Projects, services, and deployments", &["railway"]),
    entry("digitalocean", "DigitalOcean", "https://mcp.digitalocean.com/mcp", "Apps, droplets, and databases", &["digitalocean"]),
    CatalogEntry {
        token_help: Some("Render signs in only for apps it knows. Make an API key under Account Settings in the Render Dashboard and paste it here. It can reach every workspace your account can."),
        ..entry("render", "Render", "https://mcp.render.com/mcp", "Services, deploys, and logs", &["render"])
    },
    entry("posthog", "PostHog", "https://mcp.posthog.com/mcp", "Product analytics, flags, and experiments", &["posthog"]),
    entry("amplitude", "Amplitude", "https://mcp.amplitude.com/mcp", "Charts, cohorts, and events", &["amplitude"]),
    entry("mixpanel", "Mixpanel", "https://mcp.mixpanel.com/mcp", "Reports, events, and funnels", &["mixpanel"]),
    entry("make", "Make", "https://mcp.make.com", "Scenarios you've built in Make", &["make", "make.com"]),
    entry("apify", "Apify", "https://mcp.apify.com", "Web scrapers and their results", &["apify"]),
];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connector {
    pub id: String,
    pub name: String,
    pub url: String,
    pub catalog_id: Option<String>,
    /// The MCP server name familiars see, like `linear` in `mcp__linear__create_issue`.
    pub key: String,
    /// "oauth" or "token" once signed in; None before.
    pub auth: Option<String>,
    /// The familiars that have it turned on.
    pub agents: Vec<String>,
}

pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE connectors (
           id TEXT PRIMARY KEY,
           name TEXT NOT NULL,
           url TEXT NOT NULL UNIQUE,
           catalog_id TEXT,
           key TEXT NOT NULL UNIQUE,
           auth TEXT,
           created_at INTEGER NOT NULL
         );
         CREATE TABLE agent_connectors (
           agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
           connector_id TEXT NOT NULL REFERENCES connectors(id) ON DELETE CASCADE,
           PRIMARY KEY (agent_id, connector_id)
         );
         PRAGMA user_version = 28;
         COMMIT;",
    )
}

/// The database the CLI launchers read enabled connectors from. Set once at startup.
static DB: OnceLock<Arc<Db>> = OnceLock::new();

pub fn install(db: Arc<Db>) {
    let _ = DB.set(db);
}

pub fn list(db: &Db) -> Result<Vec<Connector>, String> {
    let rows = db.with(|c| {
        let mut stmt = c.prepare_cached("SELECT id, name, url, catalog_id, key, auth FROM connectors ORDER BY created_at, name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Connector { id: r.get(0)?, name: r.get(1)?, url: r.get(2)?, catalog_id: r.get(3)?, key: r.get(4)?, auth: r.get(5)?, agents: vec![] })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    let links = db.with(|c| {
        let mut stmt = c.prepare_cached("SELECT connector_id, agent_id FROM agent_connectors ORDER BY agent_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    Ok(rows
        .into_iter()
        .map(|mut c| {
            c.agents = links.iter().filter(|(id, _)| *id == c.id).map(|(_, agent)| agent.clone()).collect();
            c
        })
        .collect())
}

fn get(db: &Db, id: &str) -> Result<Connector, String> {
    list(db)?.into_iter().find(|c| c.id == id).ok_or_else(|| "That connector was removed.".into())
}

/// Checks a server URL someone typed: https, or http on this Mac for a local server.
pub fn check_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|_| "That isn't a web address. Paste the server's full URL, starting with https://.".to_string())?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && local)) {
        return Err("Connectors need an https:// address.".into());
    }
    if !url.username().is_empty() || url.password().is_some() || url.host_str().is_none() {
        return Err("Leave any login out of the address; Grimoire signs in for you.".into());
    }
    Ok(url)
}

/// A server name for the CLIs: lowercase letters, digits, `-` and `_`, unique among connectors.
fn server_key(wanted: &str, taken: &[String]) -> String {
    let base: String = wanted.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let base = match base.trim_matches('_') {
        "" | crate::tools::SERVER_NAME => "connector".to_string(),
        b => b.to_string(),
    };
    (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}_{n}") }).find(|k| !taken.contains(k)).unwrap_or(base)
}

/// What a custom server is called by default: its host without `mcp.` or `www.`, like `example` for
/// `https://mcp.example.com/mcp`.
fn name_from_url(url: &Url) -> String {
    let host = url.host_str().unwrap_or("server");
    let host = host.strip_prefix("mcp.").or_else(|| host.strip_prefix("www.")).unwrap_or(host);
    host.split('.').next().unwrap_or(host).to_string()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewConnector {
    pub catalog_id: Option<String>,
    pub url: Option<String>,
    pub name: Option<String>,
}

pub fn add(db: &Db, input: NewConnector) -> Result<Connector, String> {
    let catalog = input.catalog_id.as_deref().map(|id| CATALOG.iter().find(|e| e.id == id).ok_or("Grimoire doesn't know that connector.")).transpose()?;
    let url = check_url(catalog.map_or(input.url.as_deref().unwrap_or_default(), |e| e.url))?;
    let name = match (catalog, input.name.as_deref().map(str::trim).filter(|n| !n.is_empty())) {
        (_, Some(name)) => name.to_string(),
        (Some(entry), None) => entry.name.to_string(),
        (None, None) => name_from_url(&url),
    };
    let existing = list(db)?;
    if let Some(same) = existing.iter().find(|c| Url::parse(&c.url).is_ok_and(|u| u == url)) {
        return Err(format!("{} is already added.", same.name));
    }
    let taken: Vec<String> = existing.into_iter().map(|c| c.key).collect();
    let key = server_key(catalog.map_or(name.as_str(), |e| e.id), &taken);
    let id = uuid::Uuid::new_v4().to_string();
    db.with(|c| {
        c.execute(
            "INSERT INTO connectors (id, name, url, catalog_id, key, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, name, url.as_str(), catalog.map(|e| e.id), key, now_ms()],
        )
    })?;
    get(db, &id)
}

pub fn remove(db: &Db, secrets: &dyn SecretStore, id: &str) -> Result<(), String> {
    secrets.delete(&secret_id(id))?;
    forget_sessions(id);
    db.with(|c| c.execute("DELETE FROM connectors WHERE id = ?1", params![id]).map(|_| ()))
}

/// Turns on exactly `ids` for the familiar `agent_id`.
pub fn set_for_agent(db: &Db, agent_id: &str, ids: &[String]) -> Result<(), String> {
    let agent = db.get_agent(agent_id)?.ok_or("That familiar no longer exists.")?;
    if agent.host.is_some() && !ids.is_empty() {
        return Err("Connectors run through this Mac, so a familiar on another Mac can't use them yet.".into());
    }
    let known = list(db)?;
    if let Some(unknown) = ids.iter().find(|id| !known.iter().any(|c| &c.id == *id)) {
        return Err(format!("No connector has the id {unknown}."));
    }
    db.with(|c| {
        let tx = c.unchecked_transaction()?;
        tx.execute("DELETE FROM agent_connectors WHERE agent_id = ?1", params![agent_id])?;
        for id in ids {
            tx.execute("INSERT OR IGNORE INTO agent_connectors (agent_id, connector_id) VALUES (?1, ?2)", params![agent_id, id])?;
        }
        tx.commit()
    })
}

fn enabled_for(db: &Db, agent_id: &str, connector_id: &str) -> Result<bool, String> {
    db.with(|c| {
        c.query_row("SELECT 1 FROM agent_connectors WHERE agent_id = ?1 AND connector_id = ?2", params![agent_id, connector_id], |_| Ok(()))
            .optional()
            .map(|row| row.is_some())
    })
}

/// The signed-in connectors `agent` has turned on, as (server name, connector id), for its CLI's MCP
/// config. Empty when the familiar runs on another Mac or the tool socket is down.
pub fn servers_for(agent: &Agent) -> Vec<(String, String)> {
    let Some(db) = DB.get().filter(|_| crate::tools::available_for(agent)) else {
        return vec![];
    };
    servers_in(db, &agent.id).unwrap_or_else(|e| {
        eprintln!("Connectors are off for this turn: {e}");
        vec![]
    })
}

fn servers_in(db: &Db, agent_id: &str) -> Result<Vec<(String, String)>, String> {
    Ok(list(db)?.into_iter().filter(|c| c.auth.is_some() && c.agents.iter().any(|a| a == agent_id)).map(|c| (c.key, c.id)).collect())
}

// ---- Tokens ----

/// What the Keychain holds for one connector. Never logged or sent to the webview.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Tokens {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    /// Milliseconds since the epoch.
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    token_endpoint: Option<String>,
    #[serde(default)]
    resource: Option<String>,
}

fn secret_id(connector_id: &str) -> String {
    format!("connector:{connector_id}")
}

fn load_tokens(secrets: &dyn SecretStore, connector_id: &str) -> Result<Option<Tokens>, String> {
    match secrets.get(&secret_id(connector_id))? {
        Some(text) => serde_json::from_str(&text).map(Some).map_err(|_| "The saved sign-in is unreadable. Sign in again.".into()),
        None => Ok(None),
    }
}

fn save_tokens(db: &Db, secrets: &dyn SecretStore, connector_id: &str, tokens: &Tokens, auth: &str) -> Result<(), String> {
    let text = serde_json::to_string(tokens).map_err(|e| e.to_string())?;
    secrets.set(&secret_id(connector_id), &text)?;
    db.with(|c| c.execute("UPDATE connectors SET auth = ?2 WHERE id = ?1", params![connector_id, auth]).map(|_| ()))
}

pub fn sign_out(db: &Db, secrets: &dyn SecretStore, id: &str) -> Result<(), String> {
    secrets.delete(&secret_id(id))?;
    forget_sessions(id);
    db.with(|c| c.execute("UPDATE connectors SET auth = NULL WHERE id = ?1", params![id]).map(|_| ()))
}

/// Saves a token the person pasted, for servers that don't let apps register themselves.
pub fn set_token(db: &Db, secrets: &dyn SecretStore, id: &str, token: &str) -> Result<(), String> {
    get(db, id)?;
    let token = token.trim();
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\\') {
        return Err("Paste the token on its own, with no spaces or quotes.".into());
    }
    forget_sessions(id);
    save_tokens(db, secrets, id, &Tokens { access_token: token.into(), ..Default::default() }, "token")
}

/// Reads a token endpoint's answer, keeping what it leaves out from `previous`, like a refresh token
/// it doesn't rotate.
fn tokens_from(answer: &Value, previous: Tokens) -> Result<Tokens, String> {
    let access_token = answer["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| match answer["error"].as_str() {
        Some(error) => format!("The server refused the sign-in ({error})."),
        None => "The server sent no access token.".to_string(),
    })?;
    Ok(Tokens {
        access_token: access_token.into(),
        refresh_token: answer["refresh_token"].as_str().map(String::from).or(previous.refresh_token),
        expires_at: answer["expires_in"].as_i64().map(|s| now_ms() + s * 1000),
        ..previous
    })
}

/// Serializes refreshes, so two tool calls at once don't both spend a one-time refresh token.
static REFRESHING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn refresh(db: &Db, secrets: &dyn SecretStore, connector_id: &str, stale: &str) -> Result<Option<Tokens>, String> {
    let _turn = REFRESHING.lock().await;
    let Some(tokens) = load_tokens(secrets, connector_id)? else { return Ok(None) };
    if tokens.access_token != stale {
        return Ok(Some(tokens)); // Another call refreshed it while this one waited.
    }
    let (Some(refresh_token), Some(endpoint), Some(client_id)) = (&tokens.refresh_token, &tokens.token_endpoint, &tokens.client_id) else {
        return Ok(None);
    };
    let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", refresh_token.as_str()), ("client_id", client_id.as_str())];
    if let Some(resource) = &tokens.resource {
        form.push(("resource", resource));
    }
    if let Some(secret) = &tokens.client_secret {
        form.push(("client_secret", secret));
    }
    let answer = post_form(endpoint, &form).await?;
    if !answer.ok() {
        return Ok(None);
    }
    let fresh = tokens_from(&answer.json()?, tokens.clone())?;
    // A sign-out, removal, or new sign-in while the refresh was out wins; don't bring the old one back.
    if load_tokens(secrets, connector_id)?.is_none_or(|now| now.access_token != stale) {
        return Ok(None);
    }
    save_tokens(db, secrets, connector_id, &fresh, "oauth")?;
    Ok(Some(fresh))
}

fn sign_in_again(name: &str) -> String {
    format!("{name} needs the person to sign in again in Grimoire's Settings, under Connectors.")
}

// ---- Relaying ----

#[derive(Default, Clone)]
struct Session {
    id: Option<String>,
    protocol: Option<String>,
    /// The client's initialize message, to open a new session when the server drops this one.
    init: Option<String>,
}

/// MCP sessions by "lane|connector", so each familiar and side conversation has its own.
static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();

fn sessions() -> std::sync::MutexGuard<'static, HashMap<String, Session>> {
    SESSIONS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn forget_sessions(connector_id: &str) {
    let suffix = format!("|{connector_id}");
    sessions().retain(|key, _| !key.ends_with(&suffix));
}

/// Forwards one JSON-RPC message from `familiar`'s relay to the connector's server and returns the
/// messages the server sent back. `lane` is the authorized tool key, which keeps side conversations'
/// sessions apart; `familiar` is the familiar it belongs to.
pub async fn forward(db: &Db, secrets: &dyn SecretStore, lane: &str, familiar: &str, connector_id: &str, message: Value) -> Result<Vec<Value>, String> {
    let connector = get(db, connector_id)?;
    if !enabled_for(db, familiar, connector_id)? {
        return Err(format!("This familiar doesn't have {} turned on.", connector.name));
    }
    let mut tokens = load_tokens(secrets, connector_id)?.ok_or_else(|| sign_in_again(&connector.name))?;
    if tokens.expires_at.is_some_and(|at| at - REFRESH_EARLY_MS < now_ms()) {
        match refresh(db, secrets, connector_id, &tokens.access_token.clone()).await? {
            Some(fresh) => tokens = fresh,
            // With no way to refresh, a token still works until it actually expires.
            None if tokens.expires_at.is_some_and(|at| at > now_ms()) => {}
            None => return Err(sign_in_again(&connector.name)),
        }
    }
    let session_key = format!("{lane}|{connector_id}");
    let initializing = message["method"] == "initialize";
    if initializing {
        sessions().remove(&session_key);
    }
    let body = message.to_string();
    let mut answer = post_message(&connector.url, &tokens.access_token, &session_key, &body).await?;
    if answer.status == 401 {
        tokens = refresh(db, secrets, connector_id, &tokens.access_token.clone()).await?.ok_or_else(|| sign_in_again(&connector.name))?;
        answer = post_message(&connector.url, &tokens.access_token, &session_key, &body).await?;
    }
    // The server dropped the session: open a new one the way the client did, then send this again.
    let init = sessions().get(&session_key).filter(|s| s.id.is_some()).and_then(|s| s.init.clone());
    if let (404, false, Some(init)) = (answer.status, initializing, init) {
        reopen(&connector.url, &tokens.access_token, &session_key, &init).await.map_err(|e| format!("{} ended the session and didn't start a new one ({e}).", connector.name))?;
        answer = post_message(&connector.url, &tokens.access_token, &session_key, &body).await?;
    }
    match answer.status {
        401 | 403 => return Err(sign_in_again(&connector.name)),
        202 => return Ok(vec![]),
        s if !(200..300).contains(&s) => return Err(format!("{} answered with HTTP {s}.", connector.name)),
        _ => {}
    }
    let messages = answer.messages()?;
    if initializing {
        remember_session(&session_key, &answer, &messages, &message, body);
    }
    Ok(messages)
}

fn remember_session(session_key: &str, answer: &Answer, messages: &[Value], init: &Value, body: String) {
    let protocol = messages.iter().find(|m| m["id"] == init["id"]).and_then(|m| m["result"]["protocolVersion"].as_str()).map(String::from);
    let session = Session { id: answer.header("mcp-session-id").map(String::from), protocol, init: Some(body) };
    sessions().insert(session_key.to_string(), session);
}

/// Opens a new session with the client's own initialize message and confirms it, as the client would.
async fn reopen(url: &str, token: &str, session_key: &str, init: &str) -> Result<(), String> {
    sessions().remove(session_key);
    let answer = post_message(url, token, session_key, init).await?;
    if !answer.ok() {
        return Err(format!("HTTP {}", answer.status));
    }
    let message: Value = serde_json::from_str(init).map_err(|e| e.to_string())?;
    remember_session(session_key, &answer, &answer.messages()?, &message, init.to_string());
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string();
    post_message(url, token, session_key, &initialized).await.map(|_| ())
}

async fn post_message(url: &str, token: &str, session_key: &str, body: &str) -> Result<Answer, String> {
    let session = sessions().get(session_key).cloned().unwrap_or_default();
    let mut headers = vec![
        ("Content-Type", "application/json".to_string()),
        ("Accept", "application/json, text/event-stream".to_string()),
        ("Authorization", format!("Bearer {token}")),
    ];
    if let Some(id) = session.id {
        headers.push(("Mcp-Session-Id", id));
    }
    if let Some(protocol) = session.protocol {
        headers.push(("MCP-Protocol-Version", protocol));
    }
    fetch("POST", url, &headers, Some(body)).await
}

// ---- HTTP through curl ----

/// One HTTP answer.
#[derive(Debug, Default, PartialEq)]
pub struct Answer {
    pub status: u16,
    headers: Vec<(String, String)>,
    pub body: String,
}

impl Answer {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Result<Value, String> {
        serde_json::from_str(&self.body).map_err(|_| format!("The server sent something other than JSON (HTTP {}).", self.status))
    }

    /// The JSON-RPC messages in a reply: a JSON object or batch, or the data of each server-sent event.
    fn messages(&self) -> Result<Vec<Value>, String> {
        let event_stream = self.header("content-type").is_some_and(|t| t.to_ascii_lowercase().starts_with("text/event-stream"));
        if event_stream {
            return Ok(sse_data(&self.body).iter().filter_map(|data| serde_json::from_str(data).ok()).collect());
        }
        if self.body.trim().is_empty() {
            return Ok(vec![]);
        }
        match self.json()? {
            Value::Array(batch) => Ok(batch),
            one => Ok(vec![one]),
        }
    }
}

/// The data of each event in a server-sent event stream, with multi-line data joined.
fn sse_data(stream: &str) -> Vec<String> {
    let mut events = vec![];
    let mut data: Vec<&str> = vec![];
    for line in stream.lines().chain(std::iter::once("")) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if !data.is_empty() {
                events.push(data.join("\n"));
                data.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    events
}

/// A value for a curl config file, quoted so curl reads it back exactly.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The curl config for one request. It goes in on stdin, so tokens never show in process arguments.
fn curl_config(method: &str, url: &str, headers: &[(&str, String)], body: Option<&str>) -> String {
    let mut lines = vec![
        "silent".to_string(),
        "show-error".to_string(),
        "dump-header = \"-\"".to_string(),
        "proto = \"=https,http\"".to_string(),
        "connect-timeout = 15".to_string(),
        // Long enough for a slow tool, like a search across a big workspace.
        "max-time = 600".to_string(),
        format!("request = {}", quoted(method)),
        format!("url = {}", quoted(url)),
    ];
    if method == "GET" {
        lines.push("location".into());
        lines.push("max-redirs = 5".into());
        lines.push("proto-redir = \"=https\"".into());
    }
    lines.extend(headers.iter().map(|(name, value)| format!("header = {}", quoted(&format!("{name}: {value}")))));
    if let Some(body) = body {
        lines.push(format!("data-raw = {}", quoted(body)));
    }
    lines.join("\n") + "\n"
}

/// Splits curl's output, headers then body, into the last response. Redirects and 1xx responses
/// come first as their own header blocks.
fn parse_output(output: &[u8]) -> Result<Answer, String> {
    let text = String::from_utf8_lossy(output);
    let mut rest: &str = &text;
    let mut answer = None;
    while rest.starts_with("HTTP/") {
        let (head, body) = rest.split_once("\r\n\r\n").or_else(|| rest.split_once("\n\n")).unwrap_or((rest, ""));
        let mut lines = head.lines();
        let status = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).ok_or("The server sent an unreadable reply.")?;
        let headers = lines.filter_map(|l| l.split_once(':')).map(|(n, v)| (n.trim().to_string(), v.trim().to_string())).collect();
        answer = Some(Answer { status, headers, body: String::new() });
        rest = body;
    }
    let mut answer = answer.ok_or("The server sent an unreadable reply.")?;
    answer.body = rest.to_string();
    Ok(answer)
}

async fn fetch(method: &str, url: &str, headers: &[(&str, String)], body: Option<&str>) -> Result<Answer, String> {
    let host = Url::parse(url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| "the server".into());
    // --disable skips any ~/.curlrc, so the person's curl settings can't redirect the request.
    let mut child = tokio::process::Command::new("/usr/bin/curl")
        .args(["--disable", "--config", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start curl: {e}"))?;
    let config = curl_config(method, url, headers, body);
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config.as_bytes()).await.map_err(|e| format!("Could not reach {host}: {e}"))?;
    }
    let output = child.wait_with_output().await.map_err(|e| format!("Could not reach {host}: {e}"))?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr).trim().trim_start_matches("curl: ").to_string();
        return Err(format!("Could not reach {host}: {why}"));
    }
    parse_output(&output.stdout)
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut url = Url::parse("http://form.invalid/").expect("static URL");
    url.query_pairs_mut().extend_pairs(pairs);
    url.query().unwrap_or_default().to_string()
}

async fn post_form(url: &str, pairs: &[(&str, &str)]) -> Result<Answer, String> {
    let headers = [("Content-Type", "application/x-www-form-urlencoded".to_string()), ("Accept", "application/json".to_string())];
    fetch("POST", url, &headers, Some(&form_body(pairs))).await
}

async fn get_json(url: &str) -> Option<Value> {
    let answer = fetch("GET", url, &[("Accept", "application/json".to_string())], None).await.ok()?;
    answer.ok().then(|| answer.json().ok()).flatten().filter(Value::is_object)
}

// ---- Signing in ----

/// A `key="value"` parameter of a WWW-Authenticate header.
fn auth_param(header: &str, key: &str) -> Option<String> {
    let at = header.find(&format!("{key}=\""))? + key.len() + 2;
    header[at..].split('"').next().map(String::from)
}

/// Where a server's protected-resource metadata may be, most specific first.
fn resource_metadata_urls(server: &Url) -> Vec<String> {
    let origin = server.origin().ascii_serialization();
    let path = server.path().trim_end_matches('/');
    let mut urls = vec![];
    if !path.is_empty() {
        urls.push(format!("{origin}/.well-known/oauth-protected-resource{path}"));
    }
    urls.push(format!("{origin}/.well-known/oauth-protected-resource"));
    urls
}

/// Where an authorization server's metadata may be (RFC 8414, then OpenID Connect).
fn issuer_metadata_urls(issuer: &Url) -> Vec<String> {
    let origin = issuer.origin().ascii_serialization();
    let path = issuer.path().trim_end_matches('/');
    if path.is_empty() {
        vec![format!("{origin}/.well-known/oauth-authorization-server"), format!("{origin}/.well-known/openid-configuration")]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ]
    }
}

/// What signing in to one server needs, found from its metadata.
#[derive(Debug, PartialEq)]
struct Plan {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    /// The sign-in server takes a Client ID Metadata Document's URL as the client id.
    client_metadata_documents: bool,
    resource: String,
    scope: Option<String>,
}

/// How Grimoire gets a client id at one sign-in server.
#[derive(Debug, PartialEq)]
enum Registration {
    /// Use the metadata document's URL.
    Document,
    /// Register at this endpoint (RFC 7591).
    Dynamic(String),
    /// Neither works, so the person pastes a token.
    Token,
}

fn registration(plan: &Plan) -> Registration {
    match (&plan.registration_endpoint, plan.client_metadata_documents) {
        (Some(_), true) if PREFER_CLIENT_METADATA => Registration::Document,
        (Some(endpoint), _) => Registration::Dynamic(endpoint.clone()),
        (None, true) => Registration::Document,
        (None, false) => Registration::Token,
    }
}

async fn discover(server: &Url) -> Result<Plan, String> {
    let host = server.host_str().unwrap_or("The server").to_string();
    let probe = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "Grimoire", "version": env!("CARGO_PKG_VERSION") } } });
    let headers = [("Content-Type", "application/json".to_string()), ("Accept", "application/json, text/event-stream".to_string())];
    let challenge = fetch("POST", server.as_str(), &headers, Some(&probe.to_string())).await?;
    if challenge.ok() {
        return Err(format!("{host} doesn't ask for a sign-in. Grimoire only connects servers that do."));
    }
    let www = challenge.header("www-authenticate").unwrap_or_default().to_string();
    let mut metadata_urls: Vec<String> = auth_param(&www, "resource_metadata").into_iter().collect();
    metadata_urls.extend(resource_metadata_urls(server));
    let mut resource_meta = None;
    for url in metadata_urls {
        if let Some(meta) = get_json(&url).await {
            resource_meta = Some(meta);
            break;
        }
    }
    let issuer = resource_meta.as_ref().and_then(|m| m["authorization_servers"][0].as_str()).map(String::from).unwrap_or_else(|| server.origin().ascii_serialization());
    let issuer = Url::parse(&issuer).map_err(|_| format!("{host} named an unreadable sign-in server."))?;
    let mut server_meta = None;
    for url in issuer_metadata_urls(&issuer) {
        if let Some(meta) = get_json(&url).await {
            server_meta = Some(meta);
            break;
        }
    }
    let text = |meta: &Option<Value>, key: &str| meta.as_ref().and_then(|m| m[key].as_str()).map(String::from);
    // Servers from before metadata discovery use these paths on their own origin.
    let origin = issuer.origin().ascii_serialization();
    let (authorization_endpoint, token_endpoint, registration_endpoint) = match &server_meta {
        Some(_) => (
            text(&server_meta, "authorization_endpoint").ok_or(format!("{host}'s sign-in server has no authorization endpoint."))?,
            text(&server_meta, "token_endpoint").ok_or(format!("{host}'s sign-in server has no token endpoint."))?,
            text(&server_meta, "registration_endpoint"),
        ),
        None => (format!("{origin}/authorize"), format!("{origin}/token"), Some(format!("{origin}/register"))),
    };
    let endpoints: Vec<&str> = [Some(&authorization_endpoint), Some(&token_endpoint), registration_endpoint.as_ref()].into_iter().flatten().map(String::as_str).collect();
    check_sign_in_server(server, &issuer, text(&server_meta, "issuer").as_deref(), &endpoints)?;
    let published = resource_meta.as_ref().and_then(|m| m["scopes_supported"].as_array()).map(|list| list.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "));
    Ok(Plan {
        issuer: issuer.to_string(),
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        client_metadata_documents: server_meta.as_ref().is_some_and(|m| m["client_id_metadata_document_supported"] == true),
        resource: text(&resource_meta, "resource").unwrap_or_else(|| server.to_string()),
        scope: auth_param(&www, "scope").or(published.filter(|s| !s.is_empty())),
    })
}

/// Refuses a sign-in server that names itself as another issuer (RFC 8414), or whose endpoints, which
/// receive codes and secrets, aren't https. A local http test server may use local http endpoints.
fn check_sign_in_server(server: &Url, issuer: &Url, named: Option<&str>, endpoints: &[&str]) -> Result<(), String> {
    let host = server.host_str().unwrap_or("The server");
    if named.is_some_and(|n| n.trim_end_matches('/') != issuer.as_str().trim_end_matches('/')) {
        return Err(format!("{host}'s sign-in server describes itself as a different server, so Grimoire won't sign in there."));
    }
    let local = server.scheme() == "http";
    let safe = |e: &&str| Url::parse(e).is_ok_and(|u| u.scheme() == "https" || (local && check_url(e).is_ok()));
    if !endpoints.iter().all(safe) {
        return Err(format!("{host}'s sign-in server uses an address without https, so Grimoire won't sign in there."));
    }
    Ok(())
}

fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..chunk.len() + 1 {
            out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// The S256 PKCE challenge for a verifier (RFC 7636).
fn code_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn authorize_url(plan: &Plan, client_id: &str, redirect: &str, challenge: &str, state: &str) -> Result<String, String> {
    let mut url = Url::parse(&plan.authorization_endpoint).map_err(|_| "The sign-in server's address is unreadable.".to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query.extend_pairs([
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", redirect),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("resource", plan.resource.as_str()),
        ]);
        if let Some(scope) = &plan.scope {
            query.append_pair("scope", scope);
        }
    }
    Ok(url.into())
}

/// The code from the browser's redirect, after checking `state`. Errors carry what the server said.
fn callback_code(query: &str, state: &str) -> Result<String, String> {
    let url = Url::parse(&format!("http://127.0.0.1/callback?{query}")).map_err(|_| "The sign-in redirect was unreadable.".to_string())?;
    let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if let Some(error) = pairs.get("error") {
        let detail = pairs.get("error_description").map(|d| format!(": {d}")).unwrap_or_default();
        return Err(format!("The sign-in didn't finish ({error}{detail})."));
    }
    if pairs.get("state").is_none_or(|s| !crate::http::same_secret(s, state)) {
        return Err("The sign-in redirect didn't match this sign-in. Try again.".into());
    }
    pairs.get("code").filter(|c| !c.is_empty()).cloned().ok_or("The sign-in redirect had no code.".into())
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Grimoire</title>\
    <body style=\"font:16px -apple-system,sans-serif;display:grid;place-items:center;height:90vh;margin:0\">\
    <p>You're signed in. You can close this tab and go back to Grimoire.</p>";
const FAILED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Grimoire</title>\
    <body style=\"font:16px -apple-system,sans-serif;display:grid;place-items:center;height:90vh;margin:0\">\
    <p>The sign-in didn't finish. Go back to Grimoire to try again.</p>";

/// Waits for the browser to come back to the loopback redirect and returns its code.
async fn wait_for_code(listener: &tokio::net::TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let Ok(request) = crate::http::read_request(&mut stream, 0, |_| Ok(())).await else { continue };
        if request.head.method != "GET" || request.head.path != "/callback" {
            crate::http::reply(&mut stream, "404 Not Found", "text/plain", b"Not found.").await;
            continue;
        }
        let code = callback_code(&request.head.query, state);
        let page = if code.is_ok() { DONE_PAGE } else { FAILED_PAGE };
        crate::http::reply(&mut stream, "200 OK", "text/html; charset=utf-8", page.as_bytes()).await;
        return code;
    }
}

fn redirect_uri(port: u16) -> String {
    format!("{REDIRECT_HOST}:{port}{REDIRECT_PATH}")
}

/// Grimoire's client id at one sign-in server, and the secret registration gave it, if any.
struct Client {
    id: String,
    secret: Option<String>,
}

/// Gets a client id at the plan's sign-in server, by registering or with the metadata document at
/// `document`. `name` is the connector's, for errors.
async fn client_for(plan: &Plan, name: &str, redirect: &str, document: &str) -> Result<Client, String> {
    let endpoint = match registration(plan) {
        Registration::Token => return Err(format!("{name} doesn't let apps register themselves, so Grimoire can't sign in for you. Paste an access token instead.")),
        Registration::Dynamic(endpoint) => endpoint,
        Registration::Document => {
            // The sign-in server fetches the document itself. Without it, the browser would land on the
            // server's error page, so check first and say why.
            if get_json(document).await.is_none_or(|d| d["client_id"] != document) {
                return Err(format!("{name} needs Grimoire's client document, which isn't published yet, so Grimoire can't sign in there. Paste an access token instead."));
            }
            return Ok(Client { id: document.into(), secret: None });
        }
    };
    let body = json!({
        "client_name": "Grimoire",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        // OpenID servers treat a client without this as a website and may refuse a loopback redirect.
        "application_type": "native",
    });
    let headers = [("Content-Type", "application/json".to_string()), ("Accept", "application/json".to_string())];
    let registered = fetch("POST", &endpoint, &headers, Some(&body.to_string())).await?;
    let client = registered.json().ok().filter(|_| registered.ok());
    let id = client.as_ref().and_then(|c| c["client_id"].as_str()).map(String::from);
    let id = id.ok_or_else(|| format!("{name} didn't register Grimoire (HTTP {}). Paste an access token instead.", registered.status))?;
    Ok(Client { id, secret: client.as_ref().and_then(|c| c["client_secret"].as_str()).map(String::from) })
}

/// Signs in to a connector: discovery, a client id, the browser, then the token exchange. `open` shows
/// the authorize URL in the browser; `document` is the metadata document's URL.
async fn sign_in(open: impl FnOnce(String) -> Result<(), String>, db: &Db, secrets: &dyn SecretStore, id: &str, document: &str) -> Result<(), String> {
    let connector = get(db, id)?;
    let server = check_url(&connector.url)?;
    let plan = discover(&server).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("Could not open the sign-in redirect: {e}"))?;
    let redirect = redirect_uri(listener.local_addr().map_err(|e| e.to_string())?.port());
    let Client { id: client_id, secret: client_secret } = client_for(&plan, &connector.name, &redirect, document).await?;
    // Two random UUIDs give 244 random bits, in the characters PKCE allows.
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let state = uuid::Uuid::new_v4().simple().to_string();
    open(authorize_url(&plan, &client_id, &redirect, &code_challenge(&verifier), &state)?)?;
    let code = tokio::time::timeout(SIGN_IN_WAIT, wait_for_code(&listener, &state)).await.map_err(|_| "The sign-in timed out. Try again.".to_string())??;
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
        ("resource", plan.resource.as_str()),
    ];
    if let Some(secret) = &client_secret {
        form.push(("client_secret", secret));
    }
    let answer = post_form(&plan.token_endpoint, &form).await?;
    let previous = Tokens {
        client_id: Some(client_id.clone()),
        client_secret: client_secret.clone(),
        token_endpoint: Some(plan.token_endpoint.clone()),
        resource: Some(plan.resource.clone()),
        ..Default::default()
    };
    let tokens = tokens_from(&answer.json()?, previous)?;
    forget_sessions(id);
    save_tokens(db, secrets, id, &tokens, "oauth")
}

// ---- Commands ----

#[tauri::command]
pub fn connector_catalog() -> &'static [CatalogEntry] {
    CATALOG
}

#[tauri::command]
pub fn list_connectors(state: State<crate::AppState>) -> Result<Vec<Connector>, String> {
    list(&state.db)
}

#[tauri::command]
pub fn add_connector(app: AppHandle, state: State<crate::AppState>, input: NewConnector) -> Result<Connector, String> {
    let added = add(&state.db, input)?;
    let _ = app.emit(CHANGED, ());
    Ok(added)
}

#[tauri::command]
pub fn remove_connector(app: AppHandle, state: State<crate::AppState>, id: String) -> Result<(), String> {
    remove(&state.db, state.secrets.as_ref(), &id)?;
    let _ = app.emit(CHANGED, ());
    Ok(())
}

#[tauri::command]
pub async fn connector_sign_in(app: AppHandle, state: State<'_, crate::AppState>, id: String) -> Result<Connector, String> {
    let open = |url: String| {
        use tauri_plugin_opener::OpenerExt;
        app.opener().open_url(url, None::<&str>).map_err(|e| format!("Could not open the browser: {e}"))
    };
    sign_in(open, &state.db, state.secrets.as_ref(), &id, CLIENT_METADATA_URL).await?;
    let _ = app.emit(CHANGED, ());
    get(&state.db, &id)
}

#[tauri::command]
pub fn connector_set_token(app: AppHandle, state: State<crate::AppState>, id: String, token: String) -> Result<Connector, String> {
    set_token(&state.db, state.secrets.as_ref(), &id, &token)?;
    let _ = app.emit(CHANGED, ());
    get(&state.db, &id)
}

#[tauri::command]
pub fn connector_sign_out(app: AppHandle, state: State<crate::AppState>, id: String) -> Result<Connector, String> {
    sign_out(&state.db, state.secrets.as_ref(), &id)?;
    let _ = app.emit(CHANGED, ());
    get(&state.db, &id)
}

#[tauri::command]
pub fn set_agent_connectors(app: AppHandle, state: State<crate::AppState>, agent_id: String, ids: Vec<String>) -> Result<(), String> {
    crate::link::local_only(&agent_id)?;
    set_for_agent(&state.db, &agent_id, &ids)?;
    let _ = app.emit(CHANGED, ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Avatar, Provider};
    use crate::vault::MemoryStore;

    fn agent(id: &str, host: Option<&str>) -> Agent {
        Agent {
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
            host: host.map(String::from),
            created_at: now_ms(),
        }
    }

    fn setup() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("a1", None)).unwrap();
        db.insert_agent(&agent("a2", None)).unwrap();
        db.insert_agent(&agent("far", Some("studio.local"))).unwrap();
        db
    }

    fn catalog(db: &Db, id: &str) -> Connector {
        add(db, NewConnector { catalog_id: Some(id.into()), ..Default::default() }).unwrap()
    }

    #[test]
    fn catalog_entries_are_valid_and_unique() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        for e in CATALOG {
            assert!(check_url(e.url).is_ok(), "{}", e.id);
            assert_eq!(server_key(e.id, &[]), e.id, "catalog ids are already server names");
            assert!(e.apps.iter().all(|a| *a == a.to_lowercase()), "{}", e.id);
        }
    }

    #[test]
    fn adds_from_the_catalog_and_by_url() {
        let db = setup();
        let linear = catalog(&db, "linear");
        assert_eq!((linear.name.as_str(), linear.key.as_str(), linear.auth.as_deref()), ("Linear", "linear", None));
        assert_eq!(linear.catalog_id.as_deref(), Some("linear"));

        let custom = add(&db, NewConnector { url: Some(" https://mcp.example.com/mcp ".into()), ..Default::default() }).unwrap();
        assert_eq!((custom.name.as_str(), custom.key.as_str(), custom.url.as_str()), ("example", "example", "https://mcp.example.com/mcp"));
        let named = add(&db, NewConnector { url: Some("https://tools.example.com/linear".into()), name: Some("Linear".into()), ..Default::default() }).unwrap();
        assert_eq!(named.key, "linear_2", "keys stay unique");

        let again = add(&db, NewConnector { catalog_id: Some("linear".into()), ..Default::default() });
        assert_eq!(again.unwrap_err(), "Linear is already added.");
        assert!(add(&db, NewConnector { catalog_id: Some("gmail".into()), ..Default::default() }).is_err());
        assert_eq!(list(&db).unwrap().len(), 3);
    }

    #[test]
    fn refuses_unsafe_urls() {
        for bad in ["", "mcp.linear.app", "http://mcp.example.com/mcp", "ftp://example.com", "https://user:pw@example.com/mcp", "file:///etc/passwd"] {
            assert!(check_url(bad).is_err(), "{bad:?}");
        }
        assert!(check_url("http://127.0.0.1:8080/mcp").is_ok());
        assert!(check_url("http://localhost:3000/mcp").is_ok());
        assert_eq!(server_key("grimoire", &[]), "connector", "can't shadow Grimoire's own tools");
        assert_eq!(server_key("Hugging Face!", &[]), "hugging_face");
    }

    #[test]
    fn turns_connectors_on_per_familiar() {
        let db = setup();
        let secrets = MemoryStore::new();
        let linear = catalog(&db, "linear");
        let notion = catalog(&db, "notion");
        set_for_agent(&db, "a1", &[linear.id.clone(), notion.id.clone()]).unwrap();
        set_for_agent(&db, "a2", std::slice::from_ref(&notion.id)).unwrap();
        assert!(servers_in(&db, "a1").unwrap().is_empty(), "nothing reaches a familiar before sign-in");

        set_token(&db, &secrets, &linear.id, "lin_abc").unwrap();
        assert_eq!(servers_in(&db, "a1").unwrap(), [("linear".to_string(), linear.id.clone())]);
        assert!(servers_in(&db, "a2").unwrap().is_empty());
        assert_eq!(get(&db, &notion.id).unwrap().agents, ["a1", "a2"]);

        set_for_agent(&db, "a1", &[]).unwrap();
        assert!(servers_in(&db, "a1").unwrap().is_empty());
        assert!(set_for_agent(&db, "a1", &["nope".into()]).unwrap_err().contains("No connector"));
        assert!(set_for_agent(&db, "gone", &[]).is_err());
        assert!(set_for_agent(&db, "far", std::slice::from_ref(&linear.id)).unwrap_err().contains("another Mac"));

        db.delete_agent("a2").unwrap();
        assert!(get(&db, &notion.id).unwrap().agents.is_empty());
    }

    #[test]
    fn keeps_tokens_in_the_secret_store_only() {
        let db = setup();
        let secrets = MemoryStore::new();
        let github = catalog(&db, "github");
        assert!(set_token(&db, &secrets, &github.id, "has space").is_err());
        set_token(&db, &secrets, &github.id, " ghp_secret ").unwrap();
        assert_eq!(load_tokens(&secrets, &github.id).unwrap().unwrap().access_token, "ghp_secret");
        let listed = serde_json::to_string(&list(&db).unwrap()).unwrap();
        assert!(!listed.contains("ghp_secret"), "the list never carries a token");
        assert_eq!(get(&db, &github.id).unwrap().auth.as_deref(), Some("token"));

        sign_out(&db, &secrets, &github.id).unwrap();
        assert!(load_tokens(&secrets, &github.id).unwrap().is_none());
        assert_eq!(get(&db, &github.id).unwrap().auth, None);

        set_token(&db, &secrets, &github.id, "ghp_two").unwrap();
        remove(&db, &secrets, &github.id).unwrap();
        assert!(secrets.0.lock().unwrap().is_empty());
        assert!(list(&db).unwrap().is_empty());
    }

    #[tokio::test]
    async fn forwards_only_for_familiars_that_have_it_on() {
        let db = setup();
        let secrets = MemoryStore::new();
        let linear = catalog(&db, "linear");
        let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        let refused = forward(&db, &secrets, "a1", "a1", &linear.id, ping.clone()).await.unwrap_err();
        assert_eq!(refused, "This familiar doesn't have Linear turned on.");
        set_for_agent(&db, "a1", std::slice::from_ref(&linear.id)).unwrap();
        let unsigned = forward(&db, &secrets, "a1~chat", "a1", &linear.id, ping.clone()).await.unwrap_err();
        assert!(unsigned.contains("sign in again"), "{unsigned}");
        assert_eq!(forward(&db, &secrets, "a1", "a1", "missing", ping).await.unwrap_err(), "That connector was removed.");
    }

    #[test]
    fn reads_token_answers() {
        let previous = Tokens { refresh_token: Some("r1".into()), client_id: Some("c".into()), ..Default::default() };
        let fresh = tokens_from(&json!({ "access_token": "a2", "expires_in": 3600 }), previous.clone()).unwrap();
        assert_eq!((fresh.access_token.as_str(), fresh.refresh_token.as_deref(), fresh.client_id.as_deref()), ("a2", Some("r1"), Some("c")));
        assert!(fresh.expires_at.unwrap() > now_ms() + 3_500_000);
        let rotated = tokens_from(&json!({ "access_token": "a3", "refresh_token": "r2" }), previous.clone()).unwrap();
        assert_eq!(rotated.refresh_token.as_deref(), Some("r2"));
        assert_eq!(tokens_from(&json!({ "error": "invalid_grant" }), previous).err().unwrap(), "The server refused the sign-in (invalid_grant).");
    }

    #[test]
    fn quotes_curl_config_values() {
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quoted("x\ny"), r#""x\ny""#);
        let config = curl_config("POST", "https://mcp.linear.app/mcp", &[("Authorization", "Bearer t".into())], Some("{\"id\":1}"));
        assert!(config.starts_with("silent\n"));
        assert!(config.contains("header = \"Authorization: Bearer t\"\n"));
        assert!(config.contains("data-raw = \"{\\\"id\\\":1}\"\n"));
        assert!(!config.contains("location"), "POSTs don't follow redirects");
        assert!(curl_config("GET", "https://x", &[], None).contains("location\n"));
    }

    #[test]
    fn parses_curl_output() {
        let raw = b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /b\r\n\r\nHTTP/2 200\r\ncontent-type: text/event-stream\r\nMcp-Session-Id: s1\r\n\r\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        let answer = parse_output(raw).unwrap();
        assert_eq!(answer.status, 200);
        assert_eq!(answer.header("mcp-session-id"), Some("s1"));
        assert_eq!(answer.messages().unwrap(), [json!({ "jsonrpc": "2.0", "id": 1, "result": {} })]);

        let batch = parse_output(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n[{\"id\":1},{\"id\":2}]").unwrap();
        assert_eq!(batch.messages().unwrap().len(), 2);
        let accepted = parse_output(b"HTTP/1.1 202 Accepted\r\n\r\n").unwrap();
        assert_eq!((accepted.status, accepted.messages().unwrap().len()), (202, 0));
        assert!(parse_output(b"garbage").is_err());
    }

    #[test]
    fn joins_multiline_sse_data() {
        assert_eq!(sse_data("data: a\ndata: b\n\n: comment\ndata:c\r\n\r\n"), ["a\nb", "c"]);
        assert!(sse_data("event: ping\n\n").is_empty());
    }

    #[test]
    fn finds_metadata_urls() {
        let server = Url::parse("https://mcp.linear.app/mcp").unwrap();
        assert_eq!(resource_metadata_urls(&server), [
            "https://mcp.linear.app/.well-known/oauth-protected-resource/mcp",
            "https://mcp.linear.app/.well-known/oauth-protected-resource",
        ]);
        let issuer = Url::parse("https://auth.monday.com/mcp").unwrap();
        assert_eq!(issuer_metadata_urls(&issuer)[0], "https://auth.monday.com/.well-known/oauth-authorization-server/mcp");
        assert_eq!(issuer_metadata_urls(&Url::parse("https://mcp.notion.com").unwrap()).len(), 2);
        let www = r#"Bearer realm="OAuth", resource_metadata="https://x.dev/.well-known/oauth-protected-resource", scope="read write""#;
        assert_eq!(auth_param(www, "resource_metadata").as_deref(), Some("https://x.dev/.well-known/oauth-protected-resource"));
        assert_eq!(auth_param(www, "scope").as_deref(), Some("read write"));
        assert_eq!(auth_param("Bearer", "scope"), None);
    }

    #[test]
    fn builds_pkce_and_the_authorize_url() {
        // RFC 7636, appendix B.
        assert_eq!(code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        let plan = Plan {
            issuer: "https://mcp.linear.app".into(),
            authorization_endpoint: "https://mcp.linear.app/authorize".into(),
            token_endpoint: "https://mcp.linear.app/token".into(),
            registration_endpoint: None,
            client_metadata_documents: true,
            resource: "https://mcp.linear.app/mcp".into(),
            scope: Some("read write".into()),
        };
        let url = Url::parse(&authorize_url(&plan, CLIENT_METADATA_URL, &redirect_uri(5555), "chal", "st").unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], CLIENT_METADATA_URL, "a document's URL is the client id as is");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:5555/callback");
        assert_eq!((q["code_challenge_method"].as_str(), q["resource"].as_str(), q["scope"].as_str()), ("S256", "https://mcp.linear.app/mcp", "read write"));
        assert_eq!(form_body(&[("a", "x y"), ("b", "&=")]), "a=x+y&b=%26%3D");
    }

    /// One request as a local test server saw it.
    #[derive(Debug, Clone)]
    struct Seen {
        path: String,
        headers: String,
        body: String,
    }

    /// A local stand-in for a hosted MCP server and its token endpoint. Each request gets the next
    /// canned response; what it saw goes into `seen`.
    async fn fake_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Seen>>>) {
        fake_server_at(|_| responses).await
    }

    /// `fake_server`, with responses made from the server's own address, like `http://127.0.0.1:5555`.
    async fn fake_server_at(responses: impl FnOnce(&str) -> Vec<String>) -> (String, Arc<Mutex<Vec<Seen>>>) {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let responses = responses(&base);
        let seen = Arc::new(Mutex::new(vec![]));
        let log = seen.clone();
        tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = vec![];
                let mut chunk = [0u8; 4096];
                let (head, length) = loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some((head, _)) = text.split_once("\r\n\r\n") {
                        let length = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                        break (head.to_string(), length);
                    }
                };
                while buf.len() < head.len() + 4 + length {
                    let n = stream.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body = String::from_utf8_lossy(&buf[head.len() + 4..]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or_default().to_string();
                log.lock().unwrap().push(Seen { path, headers: head.to_lowercase(), body });
                stream.write_all(response.as_bytes()).await.unwrap();
                let _ = stream.shutdown().await;
            }
        });
        (base, seen)
    }

    fn response(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    #[tokio::test]
    async fn relays_through_curl_with_the_token_session_and_a_refresh() {
        let db = setup();
        let secrets = MemoryStore::new();
        let (base, seen) = fake_server(vec![
            response("200 OK", "Content-Type: text/event-stream\r\nMcp-Session-Id: sess-1\r\n", "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-06-18\"}}\n\n"),
            response("401 Unauthorized", "", ""),
            response("200 OK", "Content-Type: application/json\r\n", "{\"access_token\":\"tok-2\",\"expires_in\":3600}"),
            response("200 OK", "Content-Type: application/json\r\n", "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}"),
        ])
        .await;
        let c = add(&db, NewConnector { url: Some(format!("{base}/mcp")), name: Some("Local".into()), ..Default::default() }).unwrap();
        set_for_agent(&db, "a1", std::slice::from_ref(&c.id)).unwrap();
        let tokens = Tokens {
            access_token: "tok-1".into(),
            refresh_token: Some("ref-1".into()),
            client_id: Some("client-1".into()),
            token_endpoint: Some(format!("{base}/token")),
            ..Default::default()
        };
        save_tokens(&db, &secrets, &c.id, &tokens, "oauth").unwrap();

        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
        let replies = forward(&db, &secrets, "a1", "a1", &c.id, init).await.unwrap();
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18", "SSE replies come back as messages");

        let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let replies = forward(&db, &secrets, "a1", "a1", &c.id, list).await.unwrap();
        assert_eq!(replies, [json!({ "jsonrpc": "2.0", "id": 2, "result": { "tools": [] } })]);

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(), ["/mcp", "/mcp", "/token", "/mcp"]);
        assert!(seen[0].headers.contains("authorization: bearer tok-1"));
        assert!(!seen[0].headers.contains("mcp-session-id"), "a new session starts without one");
        assert!(seen[1].headers.contains("mcp-session-id: sess-1") && seen[1].headers.contains("mcp-protocol-version: 2025-06-18"));
        assert!(seen[2].body.contains("grant_type=refresh_token") && seen[2].body.contains("refresh_token=ref-1") && seen[2].body.contains("client_id=client-1"));
        assert!(seen[3].headers.contains("authorization: bearer tok-2"), "the retry uses the refreshed token");
        let saved = load_tokens(&secrets, &c.id).unwrap().unwrap();
        assert_eq!((saved.access_token.as_str(), saved.refresh_token.as_deref()), ("tok-2", Some("ref-1")), "a refresh that doesn't rotate keeps the old refresh token");
    }

    #[test]
    fn refuses_sign_in_servers_that_are_not_https_or_name_another_issuer() {
        let server = Url::parse("https://mcp.example.com/mcp").unwrap();
        let issuer = Url::parse("https://auth.example.com").unwrap();
        let good = ["https://auth.example.com/authorize", "https://auth.example.com/token"];
        assert!(check_sign_in_server(&server, &issuer, Some("https://auth.example.com/"), &good).is_ok());
        assert!(check_sign_in_server(&server, &issuer, Some("https://evil.example"), &good).unwrap_err().contains("different server"));
        assert!(check_sign_in_server(&server, &issuer, None, &["https://auth.example.com/a", "http://auth.example.com/token"]).unwrap_err().contains("without https"));
        let local = Url::parse("http://127.0.0.1:9/mcp").unwrap();
        assert!(check_sign_in_server(&local, &local, None, &["http://127.0.0.1:9/token"]).is_ok());
        assert!(check_sign_in_server(&local, &local, None, &["http://other.example/token"]).is_err());
        assert!(curl_config("GET", "https://x", &[], None).contains("proto-redir = \"=https\""), "discovery only follows https redirects");
    }

    #[tokio::test]
    async fn uses_a_short_lived_token_with_no_refresh_until_it_expires() {
        let db = setup();
        let secrets = MemoryStore::new();
        let (base, seen) = fake_server(vec![response("200 OK", "Content-Type: application/json\r\n", "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}")]).await;
        let c = add(&db, NewConnector { url: Some(format!("{base}/mcp")), ..Default::default() }).unwrap();
        set_for_agent(&db, "a1", std::slice::from_ref(&c.id)).unwrap();
        save_tokens(&db, &secrets, &c.id, &Tokens { access_token: "short".into(), expires_at: Some(now_ms() + 30_000), ..Default::default() }, "oauth").unwrap();
        forward(&db, &secrets, "a1", "a1", &c.id, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await.unwrap();
        assert!(seen.lock().unwrap()[0].headers.contains("authorization: bearer short"));
        save_tokens(&db, &secrets, &c.id, &Tokens { access_token: "old".into(), expires_at: Some(now_ms() - 1), ..Default::default() }, "oauth").unwrap();
        let expired = forward(&db, &secrets, "a1", "a1", &c.id, json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" })).await.unwrap_err();
        assert!(expired.contains("sign in again"), "{expired}");
    }

    #[tokio::test]
    async fn opens_a_new_session_when_the_server_drops_one() {
        let db = setup();
        let secrets = MemoryStore::new();
        let ok = |id: u8| response("200 OK", "Content-Type: application/json\r\n", &format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}"));
        let (base, seen) = fake_server(vec![
            response("200 OK", "Content-Type: application/json\r\nMcp-Session-Id: s1\r\n", "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"),
            response("404 Not Found", "", ""),
            response("200 OK", "Content-Type: application/json\r\nMcp-Session-Id: s2\r\n", "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"),
            response("202 Accepted", "", ""),
            ok(2),
        ])
        .await;
        let c = add(&db, NewConnector { url: Some(format!("{base}/mcp")), ..Default::default() }).unwrap();
        set_for_agent(&db, "a1", std::slice::from_ref(&c.id)).unwrap();
        save_tokens(&db, &secrets, &c.id, &Tokens { access_token: "t".into(), ..Default::default() }, "token").unwrap();
        forward(&db, &secrets, "a1", "a1", &c.id, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })).await.unwrap();
        let replies = forward(&db, &secrets, "a1", "a1", &c.id, json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await.unwrap();
        assert_eq!(replies[0]["id"], 2);
        let seen = seen.lock().unwrap().clone();
        assert!(seen[2].body.contains("\"initialize\"") && !seen[2].headers.contains("mcp-session-id"), "the new session starts clean");
        assert!(seen[3].body.contains("notifications/initialized") && seen[3].headers.contains("mcp-session-id: s2"));
        assert!(seen[4].body.contains("tools/list") && seen[4].headers.contains("mcp-session-id: s2"));
    }

    #[tokio::test]
    async fn a_refresh_that_lands_after_sign_out_is_dropped() {
        use tokio::io::AsyncReadExt;
        let db = setup();
        let secrets = Arc::new(MemoryStore::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (got_request, request_seen) = tokio::sync::oneshot::channel::<()>();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await.unwrap();
            got_request.send(()).unwrap();
            released.await.unwrap();
            stream.write_all(response("200 OK", "Content-Type: application/json\r\n", "{\"access_token\":\"late\"}").as_bytes()).await.unwrap();
            let _ = stream.shutdown().await;
        });
        let c = add(&db, NewConnector { url: Some(format!("{base}/mcp")), ..Default::default() }).unwrap();
        let tokens = Tokens { access_token: "old".into(), refresh_token: Some("r".into()), client_id: Some("c".into()), token_endpoint: Some(format!("{base}/token")), ..Default::default() };
        save_tokens(&db, secrets.as_ref(), &c.id, &tokens, "oauth").unwrap();
        let db = Arc::new(db);
        let task = {
            let (db, secrets, id) = (db.clone(), secrets.clone(), c.id.clone());
            tokio::spawn(async move { refresh(&db, secrets.as_ref(), &id, "old").await })
        };
        request_seen.await.unwrap();
        sign_out(&db, secrets.as_ref(), &c.id).unwrap();
        release.send(()).unwrap();
        assert!(task.await.unwrap().unwrap().is_none());
        assert!(load_tokens(secrets.as_ref(), &c.id).unwrap().is_none(), "the sign-out stays");
        assert_eq!(get(&db, &c.id).unwrap().auth, None);
    }

    #[test]
    fn checks_the_callback() {
        assert_eq!(callback_code("code=abc&state=s1", "s1").unwrap(), "abc");
        assert!(callback_code("code=abc&state=s2", "s1").unwrap_err().contains("didn't match"));
        assert!(callback_code("code=abc", "s1").is_err());
        assert_eq!(callback_code("error=access_denied&state=s1", "s1").unwrap_err(), "The sign-in didn't finish (access_denied).");
        assert!(callback_code("state=s1", "s1").unwrap_err().contains("no code"));
    }

    fn json_ok(body: &str) -> String {
        response("200 OK", "Content-Type: application/json\r\n", body)
    }

    /// What a server sends during discovery: the 401 to the probe, its resource metadata, and its
    /// sign-in server's metadata with `extra` fields, all at `base`.
    fn discovery(base: &str, extra: &str) -> Vec<String> {
        vec![
            response("401 Unauthorized", &format!("WWW-Authenticate: Bearer resource_metadata=\"{base}/resource\"\r\n"), ""),
            json_ok(&format!(r#"{{"resource":"{base}/mcp","authorization_servers":["{base}"]}}"#)),
            json_ok(&format!(r#"{{"issuer":"{base}","authorization_endpoint":"{base}/authorize","token_endpoint":"{base}/token"{extra}}}"#)),
        ]
    }

    #[tokio::test]
    async fn reads_how_a_server_gives_out_client_ids() {
        let cases = [
            (r#","registration_endpoint":"{base}/register""#, "dynamic"),
            (r#","client_id_metadata_document_supported":true"#, "document"),
            (r#","client_id_metadata_document_supported":"true""#, "token"),
            ("", "token"),
        ];
        for (extra, want) in cases {
            let (base, _) = fake_server_at(|base| discovery(base, &extra.replace("{base}", base))).await;
            let plan = discover(&Url::parse(&format!("{base}/mcp")).unwrap()).await.unwrap();
            assert_eq!(plan.issuer, format!("{base}/"));
            let got = match registration(&plan) {
                Registration::Dynamic(endpoint) if endpoint == format!("{base}/register") => "dynamic",
                Registration::Dynamic(endpoint) => panic!("registers at {endpoint}"),
                Registration::Document => "document",
                Registration::Token => "token",
            };
            assert_eq!(got, want, "{extra}");
        }
        let both = Plan {
            issuer: "https://a.example".into(),
            authorization_endpoint: "https://a.example/authorize".into(),
            token_endpoint: "https://a.example/token".into(),
            registration_endpoint: Some("https://a.example/register".into()),
            client_metadata_documents: true,
            resource: "https://a.example/mcp".into(),
            scope: None,
        };
        let want = if PREFER_CLIENT_METADATA { Registration::Document } else { Registration::Dynamic("https://a.example/register".into()) };
        assert_eq!(registration(&both), want, "one constant decides between the two");
    }

    #[test]
    fn the_client_metadata_document_matches_how_grimoire_signs_in() {
        let doc: Value = serde_json::from_str(include_str!("../../docs/oauth/client-metadata.json")).unwrap();
        assert_eq!(doc["client_id"], CLIENT_METADATA_URL, "the document names its own URL");
        assert_eq!(doc["redirect_uris"], json!([format!("{REDIRECT_HOST}{REDIRECT_PATH}")]));
        assert_eq!(redirect_uri(5555).replace(":5555", ""), format!("{REDIRECT_HOST}{REDIRECT_PATH}"), "only the port differs");
        assert_eq!(doc["token_endpoint_auth_method"], "none");
        assert_eq!(doc["grant_types"], json!(["authorization_code", "refresh_token"]));
        let url = Url::parse(CLIENT_METADATA_URL).unwrap();
        assert!(url.scheme() == "https" && url.path().len() > 1, "a client id URL is https with a path");
    }

    /// Signs in to a fake server with `sign_in`, playing the browser: it checks the authorize URL and
    /// comes back to the redirect with a code. Returns the authorize URL's query.
    async fn sign_in_as_browser(db: &Db, secrets: &dyn SecretStore, id: &str, document: &str) -> Result<HashMap<String, String>, String> {
        let query = Arc::new(Mutex::new(HashMap::new()));
        let seen = query.clone();
        let open = move |url: String| {
            let q: HashMap<String, String> = Url::parse(&url).unwrap().query_pairs().into_owned().collect();
            let back = format!("{}?code=code-1&state={}", q["redirect_uri"], q["state"]);
            *seen.lock().unwrap() = q;
            tokio::spawn(async move { fetch("GET", &back, &[], None).await });
            Ok(())
        };
        sign_in(open, db, secrets, id, document).await?;
        let q = query.lock().unwrap().clone();
        Ok(q)
    }

    #[tokio::test]
    async fn signs_in_with_the_metadata_document_and_refreshes_without_a_secret() {
        let db = setup();
        let secrets = MemoryStore::new();
        let (base, seen) = fake_server_at(|base| {
            let mut r = discovery(base, r#","client_id_metadata_document_supported":true"#);
            r.push(json_ok(&format!(r#"{{"client_id":"{base}/client.json","client_name":"Grimoire"}}"#)));
            r.push(json_ok(r#"{"access_token":"a1","refresh_token":"r1","expires_in":3600}"#));
            r.push(json_ok(r#"{"access_token":"a2","expires_in":3600}"#));
            r
        })
        .await;
        let document = format!("{base}/client.json");
        let c = add(&db, NewConnector { url: Some(format!("{base}/mcp")), ..Default::default() }).unwrap();
        let q = sign_in_as_browser(&db, &secrets, &c.id, &document).await.unwrap();
        assert_eq!(q["client_id"], document, "the document's URL is the client id");
        assert!(q["redirect_uri"].starts_with("http://127.0.0.1:") && q["redirect_uri"].ends_with("/callback"));
        assert_eq!(q["code_challenge_method"], "S256");

        let saved = load_tokens(&secrets, &c.id).unwrap().unwrap();
        assert_eq!((saved.access_token.as_str(), saved.client_id.as_deref(), saved.client_secret.as_deref()), ("a1", Some(document.as_str()), None));
        assert_eq!(get(&db, &c.id).unwrap().auth.as_deref(), Some("oauth"));

        let fresh = refresh(&db, &secrets, &c.id, "a1").await.unwrap().unwrap();
        assert_eq!((fresh.access_token.as_str(), fresh.refresh_token.as_deref()), ("a2", Some("r1")));

        let seen = seen.lock().unwrap().clone();
        let paths: Vec<&str> = seen.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["/mcp", "/resource", "/.well-known/oauth-authorization-server", "/client.json", "/token", "/token"], "no registration");
        let doc_id = form_body(&[("client_id", &document)]);
        for exchange in [&seen[4], &seen[5]] {
            assert!(exchange.body.contains(&doc_id) && !exchange.body.contains("client_secret"), "{}", exchange.body);
        }
        assert!(seen[4].body.contains("grant_type=authorization_code") && seen[4].body.contains("code=code-1") && seen[4].body.contains("code_verifier="));
        assert!(seen[5].body.contains("grant_type=refresh_token") && seen[5].body.contains("refresh_token=r1"));
    }

    #[tokio::test]
    async fn says_so_when_a_server_needs_the_unpublished_document() {
        let db = setup();
        let secrets = MemoryStore::new();
        let (base, seen) = fake_server_at(|base| {
            let mut r = discovery(base, r#","client_id_metadata_document_supported":true"#);
            r.push(response("404 Not Found", "", "Not found"));
            r
        })
        .await;
        let c = add(&db, NewConnector { url: Some(format!("{base}/mcp")), name: Some("Dovetail".into()), ..Default::default() }).unwrap();
        let err = sign_in(|_| panic!("the browser stays closed"), &db, &secrets, &c.id, &format!("{base}/client.json")).await.unwrap_err();
        assert_eq!(err, "Dovetail needs Grimoire's client document, which isn't published yet, so Grimoire can't sign in there. Paste an access token instead.");
        assert_eq!(seen.lock().unwrap().len(), 4);
        assert!(load_tokens(&secrets, &c.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn registers_when_the_server_offers_registration() {
        let db = setup();
        let secrets = MemoryStore::new();
        let (base, seen) = fake_server_at(|base| {
            let both = format!(r#","registration_endpoint":"{base}/register","client_id_metadata_document_supported":true"#);
            let mut r = discovery(base, &both);
            r.push(json_ok(r#"{"client_id":"dcr-1","client_secret":"s1"}"#));
            r.push(json_ok(r#"{"access_token":"a1"}"#));
            r
        })
        .await;
        if PREFER_CLIENT_METADATA {
            return; // This server would get the document instead; the test above covers that.
        }
        let c = add(&db, NewConnector { url: Some(format!("{base}/mcp")), ..Default::default() }).unwrap();
        let q = sign_in_as_browser(&db, &secrets, &c.id, "https://unused.example/client.json").await.unwrap();
        assert_eq!(q["client_id"], "dcr-1");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[3].path, "/register");
        let registered: Value = serde_json::from_str(&seen[3].body).unwrap();
        assert_eq!(registered["redirect_uris"], json!([q["redirect_uri"]]));
        assert_eq!(registered["application_type"], "native");
        assert!(seen[4].body.contains("client_id=dcr-1") && seen[4].body.contains("client_secret=s1"));
        assert_eq!(load_tokens(&secrets, &c.id).unwrap().unwrap().client_secret.as_deref(), Some("s1"));
    }

    /// Checks every catalog server's OAuth metadata again. It sends what discovery sends: an
    /// initialize without a token, then unauthenticated metadata GETs. It never registers or signs in.
    #[tokio::test]
    #[ignore = "reaches the real servers"]
    async fn catalog_servers_offer_sign_in() {
        let checks: Vec<_> = CATALOG
            .iter()
            .map(|e| {
                tokio::spawn(async move {
                    let plan = discover(&Url::parse(e.url).unwrap()).await;
                    // Discovery guesses /register for a server with no metadata, so look for real metadata.
                    let published = match &plan {
                        Ok(p) => {
                            let mut found = false;
                            for url in issuer_metadata_urls(&Url::parse(&p.issuer).unwrap()) {
                                found = found || get_json(&url).await.is_some();
                            }
                            found
                        }
                        Err(_) => false,
                    };
                    (e, plan, published)
                })
            })
            .collect();
        let mut failed = vec![];
        println!("| Connector | Registration | Metadata document | Sign-in server | Result |\n|---|---|---|---|---|");
        for check in checks {
            let (e, plan, published) = check.await.unwrap();
            let (dcr, cimd, issuer, way) = match &plan {
                Ok(p) if published => (p.registration_endpoint.is_some(), p.client_metadata_documents, p.issuer.clone(), Some(registration(p))),
                Ok(p) => (false, false, format!("{} (no metadata)", p.issuer), None),
                Err(why) => (false, false, why.clone(), None),
            };
            let result = match (way, e.token_help.is_some()) {
                (Some(Registration::Token) | None, true) => "PASS (token)",
                (Some(Registration::Dynamic(_)), false) => "PASS (registers)",
                (Some(Registration::Document), false) => "PASS (document)",
                (_, true) => "PASS, but no longer needs a token",
                _ => {
                    failed.push(e.id);
                    "FAIL"
                }
            };
            let yes = |b: bool| if b { "yes" } else { "no" };
            println!("| {} | {} | {} | {} | {} |", e.name, yes(dcr), yes(cimd), issuer, result);
        }
        assert!(failed.is_empty(), "no longer offer sign-in: {failed:?}");
    }
}
