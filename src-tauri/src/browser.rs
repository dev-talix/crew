//! A browser of each familiar's own. Grimoire starts a headless Chromium with a profile folder per
//! familiar, so it never opens a window on the person's screen or touches their browser profile, and
//! sign-ins stay between sessions. Grimoire drives it over the DevTools protocol on a private pipe
//! (`--remote-debugging-pipe`), so no debugging port is open for other programs to reach.
//!
//! The familiar uses it through the browser_* tools. The person watches a live view in the chat and
//! can take over, for a sign-in or a 2FA code; the familiar's tools wait until they hand it back.

use crate::db::Db;
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

pub const TOOL_NAMES: [&str; 5] = ["browser_open", "browser_read", "browser_click", "browser_type", "browser_close"];
pub const CHANGED: &str = "browser_changed";
pub const FRAME: &str = "browser_frame";
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
/// Each browser takes a few hundred MB, so only this many run at once.
const MAX_RUNNING: usize = 4;
/// A browser nobody has used for this long closes, unless the person has control.
const IDLE: Duration = Duration::from_secs(15 * 60);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);
const TEXT_LIMIT: usize = 8000;
const TYPE_LIMIT: usize = 5000;
const CLOSED: &str = "The browser closed.";
const TAKEN_OVER: &str = "The person has taken over your browser, for a sign-in or a code. Wait for them to hand it back, then try again.";
/// Session cookies kept in the profile between runs.
const SESSION_COOKIES: &str = "grimoire-session-cookies.json";
const NO_BROWSER: &str = "Grimoire couldn't find Chrome, Chromium, Edge, Brave, or Helium. Install one, or set GRIMOIRE_BROWSER to a Chromium-based browser's executable.";

type Notify = Arc<dyn Fn(&'static str, Value) + Send + Sync>;

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn definitions() -> Vec<Value> {
    let element = json!({ "type": "integer", "minimum": 1, "description": "The element's number from the latest page read." });
    vec![
        json!({
            "name": "browser_open",
            "description": "Open a web page in your own browser. It runs out of sight with a profile only you use, so it never touches the person's windows, and sign-ins stay between sessions. The person can watch it from your chat. Returns the page's text and its numbered elements. Only http and https pages. When a page needs a sign-in, a password, or a 2FA code, ask the person to take over your browser from the Browser button in your chat, and don't ask them for the password.",
            "inputSchema": {
                "type": "object",
                "properties": { "url": { "type": "string", "description": "The address, like https://example.com." } },
                "required": ["url"]
            }
        }),
        json!({
            "name": "browser_read",
            "description": "Read the page open in your browser again: its title, address, text, and numbered elements for browser_click and browser_type. Numbers change on each read.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "browser_click",
            "description": "Click a numbered element from the latest page read. Returns the page after the click.",
            "inputSchema": { "type": "object", "properties": { "element": element.clone() }, "required": ["element"] }
        }),
        json!({
            "name": "browser_type",
            "description": "Type into a numbered field from the latest page read, replacing what's in it. Returns the page after. Never type passwords or codes; the person enters those when they take over.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "element": element,
                    "text": { "type": "string" },
                    "submit": { "type": "boolean", "description": "Press Enter after typing." }
                },
                "required": ["element", "text"]
            }
        }),
        json!({
            "name": "browser_close",
            "description": "Close your browser when you're done. Sign-ins stay for next time. It also closes by itself after 15 idle minutes.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

/// What the chat's Browser panel shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub agent_id: String,
    pub running: bool,
    /// "familiar", or "person" while the person has taken over.
    pub control: &'static str,
    pub url: Option<String>,
    pub title: Option<String>,
    pub browser: Option<String>,
}

fn off(agent_id: &str) -> Status {
    Status { agent_id: agent_id.into(), running: false, control: "familiar", url: None, title: None, browser: None }
}

/// The person's input while they have control. Coordinates are page CSS pixels.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Input {
    Click { x: f64, y: f64 },
    Scroll {
        x: f64,
        y: f64,
        #[serde(rename = "deltaY")]
        delta_y: f64,
    },
    Text { text: String },
    Key {
        key: String,
        #[serde(default)]
        modifiers: u8,
    },
}

/// Turns an address into one the browser may open: http or https only, so a familiar can't read local
/// files or browser internals. A bare host gets https.
pub fn check_url(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Give a web address, like https://example.com.".into());
    }
    let lower = text.to_ascii_lowercase();
    let url = if lower.starts_with("http://") || lower.starts_with("https://") {
        text.to_string()
    } else if lower.contains("://") || lower.split(':').next().is_some_and(|s| ["javascript", "data", "file", "about", "chrome", "blob", "view-source"].contains(&s)) {
        return Err("Only http and https pages can open in the browser.".into());
    } else {
        format!("https://{text}")
    };
    let host = url.split("://").nth(1).unwrap_or_default().split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() || host.contains('@') {
        return Err(format!("\"{text}\" isn't a web address."));
    }
    Ok(url)
}

/// Where a familiar's profile lives. Callers pass an id they looked up in the database.
fn profile_dir(root: &Path, agent_id: &str) -> PathBuf {
    let name: String = agent_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    root.join(name)
}

/// The Chromium-based browser to run, and its name for the panel.
fn find_browser() -> Option<(PathBuf, String)> {
    let named = |path: PathBuf| {
        let name = path.file_name()?.to_string_lossy().into_owned();
        path.is_file().then_some((path, name))
    };
    if let Some(path) = std::env::var_os("GRIMOIRE_BROWSER") {
        return named(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let apps = ["Google Chrome", "Chromium", "Microsoft Edge", "Brave Browser"];
    for dir in [PathBuf::from("/Applications"), home.join("Applications")] {
        for app in apps {
            if let Some(found) = named(dir.join(format!("{app}.app/Contents/MacOS/{app}"))) {
                return Some(found);
            }
        }
    }
    // Chrome for Testing, as Playwright installs it, newest first.
    let cache = home.join("Library/Caches/ms-playwright");
    let mut builds: Vec<(u32, PathBuf)> = std::fs::read_dir(&cache)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            Some((name.strip_prefix("chromium-")?.parse().ok()?, entry.path()))
        })
        .collect();
    builds.sort_by_key(|b| std::cmp::Reverse(b.0));
    for (_, build) in builds {
        for arch in ["chrome-mac-arm64", "chrome-mac"] {
            if let Some(found) = named(build.join(format!("{arch}/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"))) {
                return Some(found);
            }
        }
    }
    ["/Applications", &home.join("Applications").to_string_lossy()]
        .iter()
        .find_map(|dir| named(PathBuf::from(format!("{dir}/Helium.app/Contents/MacOS/Helium"))))
}

fn chrome_args(profile: &Path) -> Vec<String> {
    vec![
        "--headless=new".into(),
        "--remote-debugging-pipe".into(),
        format!("--user-data-dir={}", profile.display()),
        format!("--window-size={WIDTH},{HEIGHT}"),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // Keeps cookies readable across runs without a Keychain prompt for the profile's key.
        "--use-mock-keychain".into(),
        "--mute-audio".into(),
        "about:blank".into(),
    ]
}

fn pipe() -> std::io::Result<(i32, i32)> {
    let mut fds = [0; 2];
    // SAFETY: fds has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Close-on-exec, so CLIs Grimoire starts later don't hold the pipe open after Chrome quits.
    for fd in fds {
        // SAFETY: fd was just opened above.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok((fds[0], fds[1]))
}

/// Starts Chrome with its DevTools pipe on fds 3 (commands in) and 4 (replies out).
fn spawn_chrome(exe: &Path, profile: &Path) -> std::io::Result<(Child, File, File)> {
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;
    let (commands_in, commands_out) = pipe()?;
    let (replies_in, replies_out) = pipe()?;
    let mut command = Command::new(exe);
    command.args(chrome_args(profile)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: only async-signal-safe calls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            // Copy both ends above 10 first, so the first dup2 can't overwrite the other end.
            let read = libc::fcntl(commands_in, libc::F_DUPFD, 10);
            let write = libc::fcntl(replies_out, libc::F_DUPFD, 10);
            if read < 0 || write < 0 || libc::dup2(read, 3) < 0 || libc::dup2(write, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn();
    // SAFETY: the child has its copies; these ends belong to it and nothing else here uses them.
    unsafe {
        libc::close(commands_in);
        libc::close(replies_out);
    }
    let child = child.inspect_err(|_| unsafe {
        libc::close(commands_out);
        libc::close(replies_in);
    })?;
    // SAFETY: each fd is open and owned by nothing else from here on.
    Ok((child, unsafe { File::from_raw_fd(commands_out) }, unsafe { File::from_raw_fd(replies_in) }))
}

#[derive(Debug, Clone)]
struct Page {
    target: String,
    session: String,
    url: String,
    title: String,
}

pub struct Session {
    agent_id: String,
    browser: String,
    child: Mutex<Option<Child>>,
    writer: Mutex<File>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    /// Open tabs, newest last. The last one is the one shown and driven; a popup, like a sign-in
    /// window, goes on top until it closes.
    pages: Mutex<Vec<Page>>,
    adopted: Mutex<HashSet<String>>,
    person: AtomicBool,
    closed: AtomicBool,
    last_used: Mutex<Instant>,
    /// One tool action at a time per browser.
    busy: tokio::sync::Mutex<()>,
    notify: Notify,
    profile: PathBuf,
    /// The newest live view frame, for a panel that opens after the page stopped changing.
    frame: Mutex<Option<Value>>,
    /// Marks the page read that numbered the elements, so a number from another tab or an older
    /// read is refused instead of clicking whatever has that number now.
    read_id: Mutex<String>,
}

impl Session {
    async fn start(exe: &Path, browser: &str, profile: &Path, agent_id: &str, notify: Notify) -> Result<Arc<Session>, String> {
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(profile).map_err(|e| format!("Couldn't make the browser profile folder: {e}"))?;
        }
        let (child, writer, reader) = spawn_chrome(exe, profile).map_err(|e| format!("Couldn't start {browser}: {e}"))?;
        let session = Arc::new(Session {
            agent_id: agent_id.into(),
            browser: browser.into(),
            child: Mutex::new(Some(child)),
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            pending: Mutex::default(),
            pages: Mutex::default(),
            adopted: Mutex::default(),
            person: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            last_used: Mutex::new(Instant::now()),
            busy: tokio::sync::Mutex::new(()),
            notify,
            profile: profile.to_path_buf(),
            frame: Mutex::default(),
            read_id: Mutex::default(),
        });
        let reading = session.clone();
        std::thread::spawn(move || read_loop(reading, reader));
        match session.first_page().await {
            Ok(()) => {
                session.restore_session_cookies().await;
                Ok(session)
            }
            Err(e) => {
                session.shutdown().await;
                Err(format!("{browser} didn't start: {e}"))
            }
        }
    }

    /// Reports every tab, which attaches to them, and waits for the first.
    async fn first_page(&self) -> Result<(), String> {
        self.send("Target.setDiscoverTargets", json!({ "discover": true }), None).await?;
        for attempt in 0..100 {
            if self.top().is_some() {
                return Ok(());
            }
            if attempt == 20 {
                self.send("Target.createTarget", json!({ "url": "about:blank" }), None).await?;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("it opened no page.".into())
    }

    /// Chromium drops cookies that last only for a session, like many sign-ins, when it quits.
    /// Grimoire keeps them in the profile between runs, owner-only, and puts them back at start.
    fn cookie_file(&self) -> PathBuf {
        self.profile.join(SESSION_COOKIES)
    }

    async fn save_session_cookies(&self) {
        let Ok(reply) = self.send("Storage.getCookies", json!({}), None).await else { return };
        let cookies: Vec<Value> = reply["cookies"].as_array().into_iter().flatten().filter(|c| c["session"] == true).map(cookie_param).collect();
        let path = self.cookie_file();
        if cookies.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        // Written beside it and renamed over it, so a crash mid-write keeps the last good copy.
        let partial = path.with_extension("partial");
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&partial)
            .and_then(|mut file| file.write_all(Value::from(cookies).to_string().as_bytes()))
            .and_then(|_| std::fs::rename(&partial, &path));
        if let Err(e) = written {
            eprintln!("Couldn't keep the browser's session cookies: {e}");
        }
    }

    async fn restore_session_cookies(&self) {
        let path = self.cookie_file();
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(cookies) = serde_json::from_str::<Value>(&text) else {
            let _ = std::fs::remove_file(&path);
            return;
        };
        // The file stays until Chromium takes the cookies, so a failed start can try again.
        match self.send("Storage.setCookies", json!({ "cookies": cookies }), None).await {
            Ok(_) => {
                let _ = std::fs::remove_file(&path);
            }
            Err(e) => eprintln!("Couldn't restore the browser's session cookies: {e}"),
        }
    }

    fn top(&self) -> Option<Page> {
        guard(&self.pages).last().cloned()
    }

    pub fn status(&self) -> Status {
        let page = self.top();
        Status {
            agent_id: self.agent_id.clone(),
            running: !self.closed.load(Ordering::Relaxed),
            control: if self.person.load(Ordering::Relaxed) { "person" } else { "familiar" },
            url: page.as_ref().map(|p| p.url.clone()).filter(|u| !u.is_empty()),
            title: page.map(|p| p.title).filter(|t| !t.is_empty()),
            browser: Some(self.browser.clone()),
        }
    }

    fn changed(&self) {
        (self.notify)(CHANGED, serde_json::to_value(self.status()).unwrap_or_default());
    }

    fn touch(&self) {
        *guard(&self.last_used) = Instant::now();
    }

    fn write(&self, id: u64, method: &str, params: Value, session: Option<&str>) -> Result<(), String> {
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
        bytes.push(0);
        guard(&self.writer).write_all(&bytes).map_err(|_| CLOSED.to_string())
    }

    /// Sends a command without waiting for its reply.
    fn post(&self, method: &str, params: Value, session: Option<&str>) {
        let _ = self.write(self.next_id.fetch_add(1, Ordering::Relaxed), method, params, session);
    }

    async fn send(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(CLOSED.into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        guard(&self.pending).insert(id, tx);
        if let Err(e) = self.write(id, method, params, session) {
            guard(&self.pending).remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CLOSED.into()),
            Err(_) => {
                guard(&self.pending).remove(&id);
                Err(format!("The browser didn't answer {method} in time."))
            }
        }
    }

    fn top_session(&self) -> Result<String, String> {
        self.top().map(|p| p.session).ok_or_else(|| "The browser has no page open.".to_string())
    }

    async fn evaluate(&self, session: &str, expression: &str) -> Result<Value, String> {
        let reply = self.send("Runtime.evaluate", json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }), Some(session)).await?;
        if let Some(details) = reply.get("exceptionDetails") {
            let text = details["exception"]["description"].as_str().or(details["text"].as_str()).unwrap_or("The page threw an error.");
            return Err(text.to_string());
        }
        Ok(reply["result"]["value"].clone())
    }

    fn dispatch(self: &Arc<Self>, message: Value) {
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            if let Some(tx) = guard(&self.pending).remove(&id) {
                let result = match message.get("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("The browser refused that.").to_string()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(result);
            }
            return;
        }
        let params = &message["params"];
        let from = message["sessionId"].as_str();
        match message["method"].as_str().unwrap_or_default() {
            "Page.screencastFrame" => {
                let Some(from) = from else { return };
                self.post("Page.screencastFrameAck", json!({ "sessionId": params["sessionId"] }), Some(from));
                if self.top().is_some_and(|p| p.session == from) {
                    let frame = frame_payload(&self.agent_id, params);
                    *guard(&self.frame) = Some(frame.clone());
                    (self.notify)(FRAME, frame);
                }
            }
            "Target.targetCreated" => {
                let info = params["targetInfo"].clone();
                if info["type"] == "page" {
                    let session = self.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = session.adopt(&info).await {
                            eprintln!("Couldn't follow a new browser tab: {e}");
                        }
                    });
                }
            }
            "Target.targetInfoChanged" => {
                let info = &params["targetInfo"];
                let on_top = {
                    let mut pages = guard(&self.pages);
                    let top = pages.len().saturating_sub(1);
                    match pages.iter().position(|p| info["targetId"] == p.target.as_str()) {
                        Some(at) => {
                            pages[at].url = info["url"].as_str().unwrap_or_default().into();
                            pages[at].title = info["title"].as_str().unwrap_or_default().into();
                            at == top
                        }
                        None => false,
                    }
                };
                if on_top {
                    self.changed();
                }
            }
            "Target.targetDestroyed" | "Target.detachedFromTarget" => {
                let target = params["targetId"].as_str().unwrap_or_default();
                let new_top = {
                    let mut pages = guard(&self.pages);
                    let was_top = pages.last().is_some_and(|p| p.target == target);
                    pages.retain(|p| p.target != target);
                    if was_top {
                        pages.last().cloned()
                    } else {
                        None
                    }
                };
                if let Some(page) = new_top {
                    self.post("Page.startScreencast", screencast_params(), Some(&page.session));
                    self.changed();
                }
            }
            _ => {}
        }
    }

    /// Attaches to a tab and makes it the one shown.
    async fn adopt(self: &Arc<Self>, info: &Value) -> Result<(), String> {
        let target = info["targetId"].as_str().ok_or("A tab had no id.")?.to_string();
        if !guard(&self.adopted).insert(target.clone()) {
            return Ok(());
        }
        let attached = self.send("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None).await?;
        let session = attached["sessionId"].as_str().ok_or("The browser didn't attach to the tab.")?.to_string();
        self.send("Page.enable", json!({}), Some(&session)).await?;
        let previous = self.top();
        guard(&self.pages).push(Page {
            target,
            session: session.clone(),
            url: info["url"].as_str().unwrap_or_default().into(),
            title: info["title"].as_str().unwrap_or_default().into(),
        });
        if let Some(previous) = previous {
            self.post("Page.stopScreencast", json!({}), Some(&previous.session));
        }
        self.send("Page.startScreencast", screencast_params(), Some(&session)).await?;
        self.changed();
        Ok(())
    }

    /// Chrome closed its end: the browser quit or crashed.
    fn ended(self: &Arc<Self>) {
        self.closed.store(true, Ordering::Relaxed);
        for (_, tx) in guard(&self.pending).drain() {
            let _ = tx.send(Err(CLOSED.into()));
        }
        if let Some(mut child) = guard(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(browsers) = BROWSERS.get() {
            guard(&browsers.sessions).retain(|_, s| !Arc::ptr_eq(s, self));
        }
        (self.notify)(CHANGED, serde_json::to_value(off(&self.agent_id)).unwrap_or_default());
    }

    /// Closes the browser gracefully, so it saves cookies, then makes sure it's gone.
    async fn shutdown(&self) {
        if !self.closed.load(Ordering::Relaxed) {
            self.save_session_cookies().await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.send("Browser.close", json!({}), None)).await;
        let child = guard(&self.child).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = tokio::task::spawn_blocking(move || child.wait()).await;
        }
    }

    /// Waits for the page to finish loading, or gives up quietly after LOAD_TIMEOUT.
    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let started = Instant::now();
        while started.elapsed() < LOAD_TIMEOUT {
            let Ok(session) = self.top_session() else { return };
            if self.evaluate(&session, "document.readyState").await.is_ok_and(|state| state == "complete") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Sends input to one tab, so a popup that opens halfway doesn't get the rest.
    async fn run_events(&self, session: &str, events: Vec<(&'static str, Value)>) -> Result<(), String> {
        for (method, params) in events {
            self.send(method, params, Some(session)).await?;
        }
        Ok(())
    }

    async fn navigate(&self, url: &str) -> Result<(), String> {
        let reply = self.send("Page.navigate", json!({ "url": url }), Some(&self.top_session()?)).await?;
        if let Some(error) = reply["errorText"].as_str().filter(|e| !e.is_empty()) {
            return Err(format!("Couldn't open {url}: {error}"));
        }
        self.settle().await;
        Ok(())
    }

    /// Scrolls a numbered element into view and returns its center.
    async fn locate(&self, session: &str, element: u32) -> Result<(f64, f64), String> {
        let read = guard(&self.read_id).clone();
        let point = self
            .evaluate(
                session,
                &format!(
                "(() => {{ if (window.__grimoireRead !== '{read}') return null; \
                 const el = document.querySelector('[data-grimoire-ref=\"{element}\"]'); if (!el) return null; \
                 el.scrollIntoView({{ block: 'center', inline: 'center' }}); const r = el.getBoundingClientRect(); \
                 return {{ x: r.left + r.width / 2, y: r.top + r.height / 2 }}; }})()"
                ),
            )
            .await?;
        match (point["x"].as_f64(), point["y"].as_f64()) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => Err(format!("There's no element {element} on the page now. Call browser_read and use a number from it.")),
        }
    }

    async fn read(&self) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let page = self.evaluate(&self.top_session()?, &READ_SCRIPT.replace("READ_ID", &id)).await?;
        // Chromium keeps web pages from opening local files or its own pages, and the script refuses
        // anything else, like a blob: popup a page wrote itself, before it numbers anything.
        if page["refused"] == true {
            return Err("The open tab isn't a web page, so Grimoire won't read it. Open an http or https page with browser_open.".into());
        }
        *guard(&self.read_id) = id;
        Ok(format_read(&page))
    }

    /// Runs one browser tool for the familiar.
    async fn act(&self, tool: &str, arguments: &Value) -> Result<String, String> {
        let _busy = self.busy.lock().await;
        if self.person.load(Ordering::Relaxed) {
            return Err(TAKEN_OVER.into());
        }
        self.touch();
        let element = || -> Result<u32, String> {
            arguments.get("element").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| "Give the element's number from browser_read.".to_string())
        };
        match tool {
            "browser_open" => {
                let url = check_url(arguments.get("url").and_then(Value::as_str).unwrap_or_default())?;
                self.navigate(&url).await?;
            }
            "browser_read" => {}
            "browser_click" => {
                let session = self.top_session()?;
                let (x, y) = self.locate(&session, element()?).await?;
                self.run_events(&session, input_events(&Input::Click { x, y })?).await?;
                self.settle().await;
            }
            "browser_type" => {
                let text = arguments.get("text").and_then(Value::as_str).ok_or("Give the text to type.")?;
                let session = self.top_session()?;
                let (x, y) = self.locate(&session, element()?).await?;
                let mut events = input_events(&Input::Click { x, y })?;
                events.extend(input_events(&Input::Key { key: "a".into(), modifiers: META })?);
                if !text.is_empty() {
                    events.extend(input_events(&Input::Text { text: text.into() })?);
                } else {
                    events.extend(input_events(&Input::Key { key: "Backspace".into(), modifiers: 0 })?);
                }
                if arguments.get("submit").and_then(Value::as_bool).unwrap_or(false) {
                    events.extend(input_events(&Input::Key { key: "Enter".into(), modifiers: 0 })?);
                }
                self.run_events(&session, events).await?;
                self.settle().await;
            }
            other => return Err(format!("Grimoire has no tool named {other}.")),
        }
        self.read().await
    }
}

fn read_loop(session: Arc<Session>, reader: File) {
    let mut reader = BufReader::new(reader);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(0, &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buffer.last() == Some(&0) {
            buffer.pop();
        }
        if let Ok(message) = serde_json::from_slice::<Value>(&buffer) {
            session.dispatch(message);
        }
    }
    session.ended();
}

/// The fields of a cookie that Storage.setCookies takes back.
fn cookie_param(cookie: &Value) -> Value {
    let keep = ["name", "value", "domain", "path", "secure", "httpOnly", "sameSite", "priority", "sourceScheme", "sourcePort", "partitionKey"];
    Value::Object(keep.iter().filter_map(|k| cookie.get(*k).map(|v| (k.to_string(), v.clone()))).collect())
}

fn screencast_params() -> Value {
    json!({ "format": "jpeg", "quality": 60, "maxWidth": WIDTH, "maxHeight": HEIGHT, "everyNthFrame": 1 })
}

fn frame_payload(agent_id: &str, params: &Value) -> Value {
    let metadata = &params["metadata"];
    json!({
        "agentId": agent_id,
        "data": params["data"],
        "width": metadata["deviceWidth"].as_f64().unwrap_or(WIDTH.into()),
        "height": metadata["deviceHeight"].as_f64().unwrap_or(HEIGHT.into()),
    })
}

/// Numbers the page's visible controls with data-grimoire-ref and returns them with the page text.
/// Field values are left out, so nothing the person typed, like a password, reaches the model.
const READ_SCRIPT: &str = r#"(() => {
  if (!['http:', 'https:'].includes(location.protocol) && location.href !== 'about:blank') return { refused: true };
  window.__grimoireRead = 'READ_ID';
  document.querySelectorAll('[data-grimoire-ref]').forEach(e => e.removeAttribute('data-grimoire-ref'));
  const selector = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=tab],[role=menuitem],[contenteditable=""],[contenteditable=true]';
  const elements = [];
  for (const el of document.querySelectorAll(selector)) {
    if (elements.length >= 150) break;
    const r = el.getBoundingClientRect();
    const style = getComputedStyle(el);
    if ((r.width === 0 && r.height === 0) || style.visibility === 'hidden' || style.display === 'none' || el.disabled) continue;
    const ref = elements.length + 1;
    el.setAttribute('data-grimoire-ref', String(ref));
    const tag = el.tagName.toLowerCase();
    const type = tag === 'input' ? (el.type || 'text') : '';
    const buttonValue = ['submit', 'button', 'reset'].includes(type) ? el.value : '';
    const label = (el.getAttribute('aria-label') || (tag === 'input' || tag === 'textarea' || tag === 'select' || el.isContentEditable ? '' : el.innerText) || buttonValue || el.placeholder || el.title || el.name || '').replace(/\s+/g, ' ').trim().slice(0, 80);
    elements.push({ ref, tag, type, label, href: tag === 'a' ? String(el.href).slice(0, 200) : null });
  }
  const text = (document.body ? document.body.innerText : '').replace(/\n{3,}/g, '\n\n').trim();
  return { url: location.href, title: document.title, text, elements };
})()"#;

fn format_read(page: &Value) -> String {
    let text = page["text"].as_str().unwrap_or_default();
    let text = match text.char_indices().nth(TEXT_LIMIT) {
        Some((at, _)) => format!("{}\n[The page goes on.]", &text[..at]),
        None => text.to_string(),
    };
    let mut out = format!("Title: {}\nAddress: {}\n\n{}\n", page["title"].as_str().unwrap_or_default(), page["url"].as_str().unwrap_or_default(), text);
    let elements = page["elements"].as_array().map(Vec::as_slice).unwrap_or_default();
    if elements.is_empty() {
        out.push_str("\nNo clickable elements.");
        return out;
    }
    out.push_str("\nElements, for browser_click and browser_type:\n");
    for e in elements {
        let kind = match (e["tag"].as_str().unwrap_or_default(), e["type"].as_str().unwrap_or_default()) {
            ("a", _) => "link".to_string(),
            ("input", kind) => format!("{kind} field"),
            ("textarea", _) => "text area".to_string(),
            ("select", _) => "menu".to_string(),
            (tag, _) => tag.to_string(),
        };
        out.push_str(&format!("[{}] {kind}", e["ref"]));
        if let Some(label) = e["label"].as_str().filter(|l| !l.is_empty()) {
            out.push_str(&format!(" \"{label}\""));
        }
        if let Some(href) = e["href"].as_str() {
            out.push_str(&format!(" -> {href}"));
        }
        out.push('\n');
    }
    out
}

const META: u8 = 4;

/// The DevTools input events for one input. Letters only go with Ctrl or Meta, for shortcuts like select all.
fn input_events(input: &Input) -> Result<Vec<(&'static str, Value)>, String> {
    let point = |x: f64, y: f64| -> Result<(f64, f64), String> {
        let ok = |v: f64| v.is_finite() && (0.0..=10_000.0).contains(&v);
        if ok(x) && ok(y) {
            Ok((x, y))
        } else {
            Err("That point is outside the page.".into())
        }
    };
    Ok(match input {
        Input::Click { x, y } => {
            let (x, y) = point(*x, *y)?;
            vec![
                ("Input.dispatchMouseEvent", json!({ "type": "mouseMoved", "x": x, "y": y })),
                ("Input.dispatchMouseEvent", json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1 })),
                ("Input.dispatchMouseEvent", json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1 })),
            ]
        }
        Input::Scroll { x, y, delta_y } => {
            let (x, y) = point(*x, *y)?;
            let delta = if delta_y.is_finite() { delta_y.clamp(-5000.0, 5000.0) } else { 0.0 };
            vec![("Input.dispatchMouseEvent", json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": delta }))]
        }
        Input::Text { text } => {
            if text.chars().count() > TYPE_LIMIT {
                return Err(format!("Type at most {TYPE_LIMIT} characters at a time."));
            }
            vec![("Input.insertText", json!({ "text": text }))]
        }
        Input::Key { key, modifiers } => {
            let modifiers = modifiers & 0b1111;
            let named = match key.as_str() {
                "Enter" => Some((13, "\r")),
                "Backspace" => Some((8, "")),
                "Tab" => Some((9, "")),
                "Escape" => Some((27, "")),
                "Delete" => Some((46, "")),
                "PageUp" => Some((33, "")),
                "PageDown" => Some((34, "")),
                "End" => Some((35, "")),
                "Home" => Some((36, "")),
                "ArrowLeft" => Some((37, "")),
                "ArrowUp" => Some((38, "")),
                "ArrowRight" => Some((39, "")),
                "ArrowDown" => Some((40, "")),
                _ => None,
            };
            let (down, code, vk, text, commands) = if let Some((vk, text)) = named {
                (if text.is_empty() { "rawKeyDown" } else { "keyDown" }, key.clone(), vk, text.to_string(), vec![])
            } else {
                let letter = match key.as_bytes() {
                    [c] if c.is_ascii_lowercase() && modifiers & (2 | META) != 0 => *c as char,
                    _ => return Err(format!("Grimoire can't send the key \"{key}\".")),
                };
                // Headless Chrome on macOS doesn't turn ⌘ shortcuts into editing commands by itself.
                let command = match (letter, modifiers & 8 != 0) {
                    ('a', _) => Some("selectAll"),
                    ('c', _) => Some("copy"),
                    ('x', _) => Some("cut"),
                    ('z', false) => Some("undo"),
                    ('z', true) => Some("redo"),
                    _ => None,
                };
                ("rawKeyDown", format!("Key{}", letter.to_ascii_uppercase()), letter.to_ascii_uppercase() as i64, String::new(), command.into_iter().collect())
            };
            let mut press = json!({ "type": down, "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers });
            if !text.is_empty() {
                press["text"] = json!(text);
            }
            if !commands.is_empty() {
                press["commands"] = json!(commands);
            }
            vec![
                ("Input.dispatchKeyEvent", press),
                ("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers })),
            ]
        }
    })
}

struct Browsers {
    root: PathBuf,
    notify: Notify,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    starting: tokio::sync::Mutex<()>,
}

static BROWSERS: OnceLock<Browsers> = OnceLock::new();

fn browsers() -> Result<&'static Browsers, String> {
    BROWSERS.get().ok_or_else(|| "The browser isn't ready yet.".to_string())
}

/// Sets up browsers under `data_dir/browser` and closes idle ones every minute.
pub fn install(app: &AppHandle, data_dir: &Path) {
    let app = app.clone();
    let notify: Notify = Arc::new(move |event, payload| {
        let _ = app.emit(event, payload);
    });
    let _ = BROWSERS.set(Browsers { root: data_dir.join("browser"), notify, sessions: Mutex::default(), starting: tokio::sync::Mutex::new(()) });
    tauri::async_runtime::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let Ok(browsers) = browsers() else { return };
            let idle: Vec<String> = guard(&browsers.sessions)
                .iter()
                .filter(|(_, s)| !s.person.load(Ordering::Relaxed) && guard(&s.last_used).elapsed() > IDLE)
                .map(|(id, _)| id.clone())
                .collect();
            for id in idle {
                let _ = browsers.close_unless_person(&id, true).await;
            }
        }
    });
}

impl Browsers {
    fn get(&self, agent_id: &str) -> Option<Arc<Session>> {
        guard(&self.sessions).get(agent_id).cloned()
    }

    async fn open(&self, agent_id: &str) -> Result<Arc<Session>, String> {
        if let Some(session) = self.get(agent_id) {
            return Ok(session);
        }
        let _starting = self.starting.lock().await;
        if let Some(session) = self.get(agent_id) {
            return Ok(session);
        }
        if guard(&self.sessions).len() >= MAX_RUNNING {
            return Err(format!("{MAX_RUNNING} familiars already have a browser open. Close one first."));
        }
        let (exe, name) = find_browser().ok_or(NO_BROWSER)?;
        let session = Session::start(&exe, &name, &profile_dir(&self.root, agent_id), agent_id, self.notify.clone()).await?;
        guard(&self.sessions).insert(agent_id.into(), session.clone());
        session.changed();
        Ok(session)
    }

    /// Closes the familiar's browser unless the person has control, checked under the session's
    /// action lock, which taking over also waits for. With `idle_only`, only an idle one closes.
    async fn close_unless_person(&self, agent_id: &str, idle_only: bool) -> Result<(), String> {
        let Some(session) = self.get(agent_id) else { return Ok(()) };
        let _busy = session.busy.lock().await;
        if session.person.load(Ordering::Relaxed) {
            return Err(TAKEN_OVER.into());
        }
        if idle_only && guard(&session.last_used).elapsed() <= IDLE {
            return Ok(());
        }
        self.close(agent_id).await;
        Ok(())
    }

    async fn close(&self, agent_id: &str) {
        // Waits out a start in progress, so Stop or deleting the familiar can't miss it.
        let _starting = self.starting.lock().await;
        let session = guard(&self.sessions).remove(agent_id);
        if let Some(session) = session {
            session.shutdown().await;
        }
        (self.notify)(CHANGED, serde_json::to_value(off(agent_id)).unwrap_or_default());
    }
}

/// Runs a browser tool for `caller`, which the tool server has already matched to its token.
pub async fn call_tool(db: &Db, caller: &str, tool: &str, arguments: Value) -> Result<String, String> {
    let agent = db.get_agent(caller)?.ok_or("Only a familiar can use the browser.")?;
    if agent.host.is_some() {
        return Err("Familiars on another Mac don't get a browser.".into());
    }
    let browsers = browsers()?;
    let session = match tool {
        "browser_close" => {
            browsers.close_unless_person(caller, false).await?;
            return Ok("Closed your browser. Your sign-ins stay for next time.".into());
        }
        "browser_open" => browsers.open(caller).await?,
        _ => browsers.get(caller).ok_or("Your browser isn't open. Open a page with browser_open first.")?,
    };
    session.act(tool, &arguments).await
}

/// Closes every browser when Grimoire quits.
pub async fn close_all() {
    let Ok(browsers) = browsers() else { return };
    let ids: Vec<String> = guard(&browsers.sessions).keys().cloned().collect();
    for id in ids {
        browsers.close(&id).await;
    }
}

/// Stops a deleted familiar's browser and removes its profile, with its cookies.
pub async fn forget(agent_id: &str) {
    let Ok(browsers) = browsers() else { return };
    browsers.close(agent_id).await;
    let _ = std::fs::remove_dir_all(profile_dir(&browsers.root, agent_id));
}

/// Checks that the window asked about a familiar that exists and runs on this Mac.
fn local_familiar(state: &AppState, agent_id: &str) -> Result<(), String> {
    crate::link::local_only(agent_id)?;
    let agent = state.db.get_agent(agent_id)?.ok_or("That familiar no longer exists.")?;
    if agent.host.is_some() {
        return Err("This familiar runs on another Mac, so its browser can't run here.".into());
    }
    Ok(())
}

fn running(agent_id: &str) -> Result<Arc<Session>, String> {
    browsers()?.get(agent_id).ok_or_else(|| "The browser isn't running.".to_string())
}

/// Runs one of the person's actions under the session's action lock, while they still have control.
async fn as_person<F, Fut>(agent_id: &str, action: F) -> Result<(), String>
where
    F: FnOnce(Arc<Session>) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let session = running(agent_id)?;
    let _busy = session.busy.lock().await;
    if !session.person.load(Ordering::Relaxed) {
        return Err("Take over the browser first.".into());
    }
    session.touch();
    action(session.clone()).await
}

#[tauri::command]
pub fn browser_status(state: State<AppState>, agent_id: String) -> Result<Status, String> {
    local_familiar(&state, &agent_id)?;
    Ok(running(&agent_id).map(|s| s.status()).unwrap_or_else(|_| off(&agent_id)))
}

#[tauri::command]
pub async fn browser_start(state: State<'_, AppState>, agent_id: String) -> Result<Status, String> {
    local_familiar(&state, &agent_id)?;
    Ok(browsers()?.open(&agent_id).await?.status())
}

#[tauri::command]
pub async fn browser_stop(state: State<'_, AppState>, agent_id: String) -> Result<(), String> {
    local_familiar(&state, &agent_id)?;
    browsers()?.close(&agent_id).await;
    Ok(())
}

/// The newest live view frame, or null before the first.
#[tauri::command]
pub fn browser_frame(state: State<AppState>, agent_id: String) -> Result<Option<Value>, String> {
    local_familiar(&state, &agent_id)?;
    Ok(running(&agent_id).ok().and_then(|s| guard(&s.frame).clone()))
}

/// The person takes over the browser (`person` true) or hands it back to the familiar.
#[tauri::command]
pub async fn browser_control(state: State<'_, AppState>, agent_id: String, person: bool) -> Result<Status, String> {
    local_familiar(&state, &agent_id)?;
    let session = running(&agent_id)?;
    // Lets a familiar action already under way finish first, so it can't act after the person takes over.
    let _busy = session.busy.lock().await;
    session.person.store(person, Ordering::Relaxed);
    session.touch();
    session.changed();
    Ok(session.status())
}

#[tauri::command]
pub async fn browser_navigate(state: State<'_, AppState>, agent_id: String, url: String) -> Result<(), String> {
    local_familiar(&state, &agent_id)?;
    let url = check_url(&url)?;
    as_person(&agent_id, |session| async move { session.navigate(&url).await }).await
}

#[tauri::command]
pub async fn browser_input(state: State<'_, AppState>, agent_id: String, input: Input) -> Result<(), String> {
    local_familiar(&state, &agent_id)?;
    let events = input_events(&input)?;
    as_person(&agent_id, |session| async move { session.run_events(&session.top_session()?, events).await }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_only_web_addresses() {
        assert_eq!(check_url("https://example.com/a?b").unwrap(), "https://example.com/a?b");
        assert_eq!(check_url(" HTTP://Example.com ").unwrap(), "HTTP://Example.com");
        assert_eq!(check_url("example.com/path").unwrap(), "https://example.com/path");
        assert_eq!(check_url("localhost:3000").unwrap(), "https://localhost:3000");
        for bad in ["", "file:///etc/passwd", "javascript:alert(1)", "data:text/html,hi", "chrome://settings", "about:blank", "ftp://x.com", "https://", "https://user@evil.com", "a b.com", "view-source:https://x.com"] {
            assert!(check_url(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn keeps_profiles_inside_the_root() {
        let root = Path::new("/data/browser");
        assert_eq!(profile_dir(root, "5f0c-ab12"), root.join("5f0c-ab12"));
        assert_eq!(profile_dir(root, "../../etc"), root.join("______etc"));
        assert_eq!(profile_dir(root, "a/b"), root.join("a_b"));
    }

    #[test]
    fn runs_headless_with_its_own_profile_and_no_port() {
        let args = chrome_args(Path::new("/p/agent"));
        assert!(args.contains(&"--headless=new".to_string()));
        assert!(args.contains(&"--remote-debugging-pipe".to_string()));
        assert!(args.contains(&"--user-data-dir=/p/agent".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--remote-debugging-port")));
    }

    #[test]
    fn turns_clicks_and_scrolls_into_mouse_events() {
        let click = input_events(&Input::Click { x: 10.0, y: 20.5 }).unwrap();
        let kinds: Vec<&str> = click.iter().map(|(_, p)| p["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["mouseMoved", "mousePressed", "mouseReleased"]);
        assert_eq!(click[1].1["x"], 10.0);
        assert_eq!(click[1].1["button"], "left");
        let scroll = input_events(&Input::Scroll { x: 1.0, y: 1.0, delta_y: 1e9 }).unwrap();
        assert_eq!(scroll[0].1["deltaY"], 5000.0);
        assert!(input_events(&Input::Click { x: f64::NAN, y: 0.0 }).is_err());
        assert!(input_events(&Input::Click { x: -1.0, y: 0.0 }).is_err());
    }

    #[test]
    fn maps_keys_and_shortcuts() {
        let enter = input_events(&Input::Key { key: "Enter".into(), modifiers: 0 }).unwrap();
        assert_eq!(enter[0].1["type"], "keyDown");
        assert_eq!(enter[0].1["text"], "\r");
        assert_eq!(enter[0].1["windowsVirtualKeyCode"], 13);
        assert_eq!(enter[1].1["type"], "keyUp");
        let back = input_events(&Input::Key { key: "Backspace".into(), modifiers: 0 }).unwrap();
        assert_eq!(back[0].1["type"], "rawKeyDown");
        assert!(back[0].1.get("text").is_none());
        let all = input_events(&Input::Key { key: "a".into(), modifiers: META }).unwrap();
        assert_eq!(all[0].1["commands"], json!(["selectAll"]));
        assert_eq!(all[0].1["code"], "KeyA");
        let redo = input_events(&Input::Key { key: "z".into(), modifiers: META | 8 }).unwrap();
        assert_eq!(redo[0].1["commands"], json!(["redo"]));
        assert!(input_events(&Input::Key { key: "a".into(), modifiers: 0 }).is_err());
        assert!(input_events(&Input::Key { key: "F13".into(), modifiers: 0 }).is_err());
        assert!(input_events(&Input::Text { text: "x".repeat(TYPE_LIMIT + 1) }).is_err());
    }

    #[test]
    fn reads_input_from_the_window() {
        let click: Input = serde_json::from_value(json!({ "kind": "click", "x": 1, "y": 2 })).unwrap();
        assert_eq!(click, Input::Click { x: 1.0, y: 2.0 });
        let scroll: Input = serde_json::from_value(json!({ "kind": "scroll", "x": 1, "y": 2, "deltaY": -40 })).unwrap();
        assert_eq!(scroll, Input::Scroll { x: 1.0, y: 2.0, delta_y: -40.0 });
        let key: Input = serde_json::from_value(json!({ "kind": "key", "key": "Tab" })).unwrap();
        assert_eq!(key, Input::Key { key: "Tab".into(), modifiers: 0 });
        assert!(serde_json::from_value::<Input>(json!({ "kind": "drag" })).is_err());
    }

    #[test]
    fn formats_a_page_for_the_familiar() {
        let page = json!({
            "url": "https://example.com/login",
            "title": "Sign in",
            "text": "Welcome\nSign in to go on",
            "elements": [
                { "ref": 1, "tag": "input", "type": "email", "label": "Email", "href": null },
                { "ref": 2, "tag": "button", "type": "", "label": "Continue", "href": null },
                { "ref": 3, "tag": "a", "type": "", "label": "Help", "href": "https://example.com/help" }
            ]
        });
        let text = format_read(&page);
        assert!(text.starts_with("Title: Sign in\nAddress: https://example.com/login\n\nWelcome\nSign in to go on\n"));
        assert!(text.contains("[1] email field \"Email\"\n[2] button \"Continue\"\n[3] link \"Help\" -> https://example.com/help\n"));
        let long = format_read(&json!({ "text": "é".repeat(TEXT_LIMIT + 5), "elements": [] }));
        assert!(long.contains("[The page goes on.]"));
        assert!(long.ends_with("No clickable elements."));
    }

    #[test]
    fn keeps_only_the_cookie_fields_chromium_takes_back() {
        let cookie = json!({ "name": "sid", "value": "v", "domain": ".x.com", "path": "/", "expires": -1, "size": 4, "session": true, "httpOnly": true, "secure": true, "sameSite": "Lax" });
        assert_eq!(cookie_param(&cookie), json!({ "name": "sid", "value": "v", "domain": ".x.com", "path": "/", "httpOnly": true, "secure": true, "sameSite": "Lax" }));
    }

    #[test]
    fn frames_carry_the_page_size() {
        let frame = frame_payload("a1", &json!({ "data": "abc", "sessionId": 3, "metadata": { "deviceWidth": 1280, "deviceHeight": 720 } }));
        assert_eq!(frame, json!({ "agentId": "a1", "data": "abc", "width": 1280.0, "height": 720.0 }));
    }

    #[test]
    fn tool_definitions_match_their_names() {
        let names: Vec<String> = definitions().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, TOOL_NAMES);
    }

    /// Drives a real headless browser against a local page. Runs only with GRIMOIRE_BROWSER_TEST=1,
    /// since it needs a Chromium installed; it costs no tokens.
    #[tokio::test(flavor = "multi_thread")]
    async fn drives_a_real_browser_and_keeps_cookies() {
        if std::env::var("GRIMOIRE_BROWSER_TEST").as_deref() != Ok("1") {
            return;
        }
        let Some((exe, name)) = find_browser() else { panic!("{NO_BROWSER}") };
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", server.local_addr().unwrap());
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let page = r#"<!doctype html><title>Test page</title>
<p id="seen"></p>
<input aria-label="Your name" id="name"><button onclick="document.getElementById('out').textContent = 'Hello ' + document.getElementById('name').value">Greet</button>
<div id="out"></div>
<script>document.getElementById('seen').textContent = document.cookie.includes('seen=yes') ? 'Welcome back' : 'First visit'; document.cookie = 'seen=yes; max-age=86400; path=/'; document.getElementById('seen').textContent += document.cookie.includes('signed=in') ? ', still signed in' : ''; document.cookie = 'signed=in; path=/';</script>"#;
            while let Ok((mut stream, _)) = server.accept().await {
                let mut buf = [0; 4096];
                let _ = stream.read(&mut buf).await;
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        let profile = std::env::temp_dir().join(format!("grimoire-browser-test-{}", uuid::Uuid::new_v4()));
        let frames = Arc::new(AtomicU64::new(0));
        let counted = frames.clone();
        let notify: Notify = Arc::new(move |event, _| {
            if event == FRAME {
                counted.fetch_add(1, Ordering::Relaxed);
            }
        });

        let session = Session::start(&exe, &name, &profile, "test", notify.clone()).await.unwrap();
        let page = session.act("browser_open", &json!({ "url": url })).await.unwrap();
        assert!(page.contains("Title: Test page"), "{page}");
        assert!(page.contains("First visit"), "{page}");
        assert!(page.contains("[1] text field \"Your name\"") && page.contains("[2] button \"Greet\""), "{page}");
        session.act("browser_type", &json!({ "element": 1, "text": "Ada" })).await.unwrap();
        let page = session.act("browser_click", &json!({ "element": 2 })).await.unwrap();
        assert!(page.contains("Hello Ada"), "{page}");
        // A number from an older read, here after reloading the page, is refused rather than reused.
        let stale = guard(&session.read_id).clone();
        session.act("browser_open", &json!({ "url": url })).await.unwrap();
        *guard(&session.read_id) = stale;
        assert!(session.act("browser_click", &json!({ "element": 2 })).await.unwrap_err().contains("no element 2"));
        assert!(frames.load(Ordering::Relaxed) > 0, "no live view frames arrived");
        session.person.store(true, Ordering::Relaxed);
        assert_eq!(session.act("browser_read", &json!({})).await.unwrap_err(), TAKEN_OVER);
        session.person.store(false, Ordering::Relaxed);
        session.shutdown().await;

        let session = Session::start(&exe, &name, &profile, "test", notify).await.unwrap();
        let page = session.act("browser_open", &json!({ "url": url })).await.unwrap();
        assert!(page.contains("Welcome back, still signed in"), "a cookie didn't survive a restart: {page}");
        session.shutdown().await;
        let _ = std::fs::remove_dir_all(&profile);
    }
}
