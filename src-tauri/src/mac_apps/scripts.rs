//! Mail through JavaScript for Automation and Shortcuts through `/usr/bin/shortcuts`. Mail has no
//! framework for other apps, and AppleScript is how the codebase already reads it (see triggers.rs).
//! Arguments go to each script as one JSON argument, never spliced into its source.

use super::{denied, App, MailFound, MailMessage, MailQuery, MailRef, Shortcut};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

const MAIL_WAIT: Duration = Duration::from_secs(45);
const SHORTCUT_LIST_WAIT: Duration = Duration::from_secs(20);
/// Longer than any approval wait would leave room for, and short enough for the tool call's timeout.
const SHORTCUT_RUN_WAIT: Duration = Duration::from_secs(120);

/// Searches one or more mailboxes for messages newer than `since`, newest first, and fetches the
/// mailbox and account of the `limit` it returns. `whose` runs inside Mail, so only matches cross over.
pub(super) const MAIL_SEARCH: &str = r#"
function run(argv) {
  var a = JSON.parse(argv[0]);
  var Mail = Application('Mail');
  if (!Mail.running()) return JSON.stringify({ error: 'not_running' });
  var want = (a.mailbox || 'INBOX').toLowerCase();
  var boxes = [];
  if (want === 'inbox' && !a.account) {
    boxes.push(Mail.inbox);
  } else {
    Mail.accounts().forEach(function (acc) {
      if (a.account && acc.name().toLowerCase() !== a.account.toLowerCase()) return;
      acc.mailboxes().forEach(function (box) {
        if (box.name().toLowerCase() === want) boxes.push(box);
      });
    });
    if (!a.account) {
      Mail.mailboxes().forEach(function (box) {
        if (box.name().toLowerCase() === want) boxes.push(box);
      });
    }
    if (boxes.length === 0) return JSON.stringify({ error: 'no_mailbox' });
  }
  var conds = [{ dateReceived: { '>=': new Date(a.since * 1000) } }];
  if (a.until) conds.push({ dateReceived: { '<': new Date(a.until * 1000) } });
  if (a.query) conds.push({ _or: [{ subject: { _contains: a.query } }, { sender: { _contains: a.query } }] });
  var found = [];
  boxes.forEach(function (box) {
    // JXA refuses an _and with one condition.
    var msgs = box.messages.whose(conds.length === 1 ? conds[0] : { _and: conds });
    var dates = msgs.dateReceived();
    for (var i = 0; i < dates.length; i++) found.push({ msgs: msgs, i: i, date: dates[i] });
  });
  found.sort(function (x, y) { return y.date - x.date; });
  var out = found.slice(0, a.limit).map(function (f) {
    var m = f.msgs[f.i];
    var box = m.mailbox();
    var account = '';
    try { account = box.account().name(); } catch (e) {}
    return { id: m.id(), subject: m.subject() || '', from: m.sender() || '', date: Math.floor(f.date.getTime() / 1000), read: m.readStatus(), mailbox: box.name(), account: account };
  });
  return JSON.stringify({ messages: out, total: found.length });
}
"#;

/// Reads one message's headers and up to `cap` characters of its plain text.
pub(super) const MAIL_READ: &str = r#"
function run(argv) {
  var a = JSON.parse(argv[0]);
  var Mail = Application('Mail');
  if (!Mail.running()) return JSON.stringify({ error: 'not_running' });
  var parent = Mail;
  if (a.account) {
    var accounts = Mail.accounts.whose({ name: a.account })();
    if (accounts.length === 0) return JSON.stringify({ error: 'no_mailbox' });
    parent = accounts[0];
  }
  var boxes = parent.mailboxes.whose({ name: a.mailbox })();
  if (boxes.length === 0) return JSON.stringify({ error: 'no_mailbox' });
  var msgs = boxes[0].messages.whose({ id: a.id })();
  if (msgs.length === 0) return JSON.stringify({ error: 'no_message' });
  var m = msgs[0];
  // Counted in characters, as Rust counts them, so an emoji is one and is never cut in half.
  var chars = Array.from(m.content() || '');
  var people = function (list) {
    return list.slice(0, 50).map(function (r) {
      var name = r.name(), address = r.address();
      return name ? name + ' <' + address + '>' : address;
    });
  };
  return JSON.stringify({
    subject: m.subject() || '', from: m.sender() || '', to: people(m.toRecipients()), cc: people(m.ccRecipients()),
    date: Math.floor(m.dateReceived().getTime() / 1000), body: chars.slice(0, a.cap).join(''), length: chars.length
  });
}
"#;

/// Runs `program` with `args`, and returns its standard output, or an error naming what went wrong.
/// Stops it after `wait`.
fn run(program: &str, args: &[&str], wait: Duration) -> Result<String, String> {
    let child = Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Grimoire couldn't start {program}: {e}"))?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(wait) {
        Ok(out) => out.map_err(|e| e.to_string())?,
        Err(_) => {
            // Only the child this call started.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            return Err(Timeout.to_string());
        }
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("it failed without saying why").trim();
        return Err(line.chars().take(300).collect());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct Timeout;

impl std::fmt::Display for Timeout {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("timed out")
    }
}

/// What osascript's error means for the familiar.
fn mail_error(error: String) -> String {
    if error.contains("-1743") || error.contains("Not authorized") {
        denied(App::Mail)
    } else if error == Timeout.to_string() {
        format!("Mail took longer than {} seconds. Search a shorter date range, or add a query.", MAIL_WAIT.as_secs())
    } else {
        format!("Mail didn't answer: {error}")
    }
}

fn osascript(script: &str, input: &Value) -> Result<String, String> {
    run("/usr/bin/osascript", &["-l", "JavaScript", "-e", script, &input.to_string()], MAIL_WAIT).map_err(mail_error)
}

/// The script's own errors, which it returns as `{ "error": ... }`.
fn script_error(value: &Value) -> Result<(), String> {
    match value.get("error").and_then(Value::as_str) {
        None => Ok(()),
        Some("not_running") => Err("Mail isn't open. Ask the person to open the Mail app, then try again.".into()),
        Some("no_mailbox") => Err("Mail has no mailbox by that name. Leave mailbox out to search every inbox.".into()),
        Some("no_message") => Err("That message isn't there any more. Search again for its new id.".into()),
        Some(other) => Err(format!("Mail said: {other}")),
    }
}

pub(super) fn parse_mail_search(out: &str) -> Result<MailFound, String> {
    let value: Value = serde_json::from_str(out.trim()).map_err(|_| "Mail sent back something Grimoire can't read.".to_string())?;
    script_error(&value)?;
    serde_json::from_value(value).map_err(|e| format!("Mail sent back something Grimoire can't read: {e}"))
}

pub(super) fn parse_mail_read(out: &str) -> Result<MailMessage, String> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        subject: String,
        #[serde(default)]
        from: String,
        #[serde(default)]
        to: Vec<String>,
        #[serde(default)]
        cc: Vec<String>,
        date: i64,
        #[serde(default)]
        body: String,
        #[serde(default)]
        length: usize,
    }
    let value: Value = serde_json::from_str(out.trim()).map_err(|_| "Mail sent back something Grimoire can't read.".to_string())?;
    script_error(&value)?;
    let r: Raw = serde_json::from_value(value).map_err(|e| format!("Mail sent back something Grimoire can't read: {e}"))?;
    Ok(MailMessage { subject: r.subject, from: r.from, to: r.to, cc: r.cc, date: r.date, length: r.length.max(r.body.chars().count()), body: r.body })
}

pub(super) fn mail_search(query: &MailQuery) -> Result<MailFound, String> {
    let input = json!({
        "query": query.query,
        "mailbox": query.mailbox,
        "account": query.account,
        "since": query.since,
        "until": query.until,
        "limit": query.limit,
    });
    parse_mail_search(&osascript(MAIL_SEARCH, &input)?)
}

pub(super) fn mail_read(message: &MailRef, cap: usize) -> Result<MailMessage, String> {
    let input = json!({ "id": message.id, "mailbox": message.mailbox, "account": message.account, "cap": cap });
    parse_mail_read(&osascript(MAIL_READ, &input)?)
}

/// Reads `shortcuts list --show-identifiers`, one "Name (IDENTIFIER)" a line.
pub(super) fn parse_shortcuts(out: &str) -> Vec<Shortcut> {
    out.lines()
        .filter_map(|line| {
            let line = line.trim_end();
            let open = line.rfind(" (")?;
            let id = line[open + 2..].strip_suffix(')')?;
            let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
            valid.then(|| Shortcut { name: line[..open].to_string(), id: id.to_string() })
        })
        .collect()
}

pub(super) fn shortcuts() -> Result<Vec<Shortcut>, String> {
    let out = run("/usr/bin/shortcuts", &["list", "--show-identifiers"], SHORTCUT_LIST_WAIT).map_err(|e| format!("Shortcuts didn't answer: {e}"))?;
    let mut list = parse_shortcuts(&out);
    list.sort_by_key(|s| s.name.to_lowercase());
    Ok(list)
}

/// A file only this user can read, removed when dropped.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(suffix: &str) -> Self {
        Scratch(std::env::temp_dir().join(format!("grimoire-shortcut-{}{suffix}", uuid::Uuid::new_v4().simple())))
    }

    fn write(&self, text: &str) -> Result<(), String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&self.0).map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Runs a Shortcut by its identifier, so a name can't be read as an option, and returns up to `cap`
/// characters of its text output.
pub(super) fn run_shortcut(shortcut: &Shortcut, input: Option<&str>, cap: usize) -> Result<String, String> {
    let output = Scratch::new(".txt");
    let given = Scratch::new(".txt");
    let out_path = output.0.to_string_lossy().into_owned();
    let in_path = given.0.to_string_lossy().into_owned();
    let mut args = vec!["run", shortcut.id.as_str(), "--output-path", &out_path, "--output-type", "public.plain-text"];
    if let Some(text) = input {
        given.write(text)?;
        args.extend(["--input-path", &in_path]);
    }
    run("/usr/bin/shortcuts", &args, SHORTCUT_RUN_WAIT).map_err(|e| {
        if e == Timeout.to_string() {
            format!("\"{}\" was still running after {} seconds, so Grimoire stopped it.", shortcut.name, SHORTCUT_RUN_WAIT.as_secs())
        } else {
            format!("\"{}\" failed: {e}", shortcut.name)
        }
    })?;
    let Ok(file) = std::fs::File::open(&output.0) else { return Ok(String::new()) };
    // Four bytes a character at most, plus one more to know it was cut.
    let mut bytes = Vec::new();
    file.take((cap as u64 + 1) * 4).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).chars().take(cap + 1).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mail_search_output() {
        let out = r#"{"messages":[{"id":512,"subject":"Invoice","from":"Billing <b@x.com>","date":1795876200,"read":false,"mailbox":"INBOX","account":"Google"},{"id":7,"subject":"","from":"a@x.com","date":1,"read":true,"mailbox":"Archive","account":""}],"total":2}"#;
        let found = parse_mail_search(out).unwrap();
        assert_eq!(found.total, 2);
        let list = found.messages;
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].id, list[0].account.as_str(), list[0].read), (512, "Google", false));
        assert_eq!(list[1].mailbox, "Archive");
        assert!(parse_mail_search(r#"{"messages":[],"total":0}"#).unwrap().messages.is_empty());
        assert_eq!(parse_mail_search(r#"{"error":"not_running"}"#).unwrap_err(), "Mail isn't open. Ask the person to open the Mail app, then try again.");
        assert!(parse_mail_search(r#"{"error":"no_mailbox"}"#).unwrap_err().contains("no mailbox"));
        assert!(parse_mail_search("garbage").unwrap_err().contains("can't read"));
    }

    #[test]
    fn reads_one_message() {
        let out = r#"{"subject":"Hi","from":"Sam <s@x.com>","to":["Me <me@x.com>"],"cc":[],"date":1795876200,"body":"Hello there","length":5000}"#;
        let message = parse_mail_read(out).unwrap();
        assert_eq!(message.to, ["Me <me@x.com>"]);
        assert_eq!((message.body.as_str(), message.length), ("Hello there", 5000));
        assert!(parse_mail_read(r#"{"error":"no_message"}"#).unwrap_err().contains("isn't there"));
    }

    #[test]
    fn explains_mail_errors() {
        assert!(mail_error("execution error: Not authorized to send Apple events to Mail. (-1743)".into()).contains("Privacy & Security, Automation"));
        assert!(mail_error(Timeout.to_string()).contains("45 seconds"));
        assert_eq!(mail_error("boom".into()), "Mail didn't answer: boom");
    }

    #[test]
    fn reads_the_shortcuts_list() {
        let out = "Log Water (4F7A2C1E-9B1D-4C8E-A2F3-112233445566)\nText Mum (Home) (0A1B2C3D-0000-1111-2222-333344445555)\nnot a shortcut line\nOdd (zz)\n\n";
        let list = parse_shortcuts(out);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], Shortcut { name: "Log Water".into(), id: "4F7A2C1E-9B1D-4C8E-A2F3-112233445566".into() });
        assert_eq!(list[1].name, "Text Mum (Home)", "a name may hold parentheses itself");
        assert!(parse_shortcuts("").is_empty());
    }

    /// The scripts compile as JavaScript for Automation. Compiling runs nothing and talks to no app.
    #[test]
    fn mail_scripts_compile() {
        for script in [MAIL_SEARCH, MAIL_READ] {
            let path = std::env::temp_dir().join(format!("grimoire-mail-{}.scpt", uuid::Uuid::new_v4().simple()));
            let path_text = path.to_string_lossy().into_owned();
            let compiled = run("/usr/bin/osacompile", &["-l", "JavaScript", "-o", &path_text, "-e", script], Duration::from_secs(30));
            let _ = std::fs::remove_file(&path);
            compiled.unwrap();
        }
    }

    /// A stand-in for Mail inside osascript that answers like JXA does, including refusing an `_and`
    /// with one condition. The scripts run against it, so no app is ever reached.
    const FAKE_MAIL: &str = r#"
function spec(items) {
  var f = function () { return items; };
  items.forEach(function (m, i) { f[i] = m; });
  f.whose = function (p) {
    if (p._and && p._and.length < 2) throw new Error('Whose clause "_and" array requires at least two elements.');
    return spec(items);
  };
  f.dateReceived = function () { return items.map(function (m) { return m.dateReceived(); }); };
  return f;
}
function value(v) { return function () { return v; }; }
var BODY = 'a'.repeat(9) + '😀' + ' and more';
var box = { name: value('INBOX'), account: function () { return { name: value('iCloud') }; } };
var msg = { id: value(42), subject: value('Hi'), sender: value('Sam <s@x.com>'), readStatus: value(false), dateReceived: value(new Date(1795876200000)),
  mailbox: value(box), content: value(BODY), toRecipients: value([{ name: value('Me'), address: value('me@x.com') }]), ccRecipients: value([]) };
box.messages = spec([msg]);
var account = { name: value('iCloud'), mailboxes: spec([box]) };
var FAKE = { running: value(true), inbox: { messages: spec([msg]) }, accounts: spec([account]), mailboxes: spec([]) };
"#;

    fn run_against_fake(script: &str, input: Value) -> String {
        let script = format!("{FAKE_MAIL}\n{}", script.replace("Application('Mail')", "FAKE"));
        run("/usr/bin/osascript", &["-l", "JavaScript", "-e", &script, &input.to_string()], Duration::from_secs(30)).unwrap()
    }

    #[test]
    fn mail_scripts_run_against_a_stand_in_for_mail() {
        // One condition, the default search, and three conditions.
        for input in [json!({ "since": 0, "limit": 5 }), json!({ "since": 0, "until": 2_000_000_000, "query": "Hi", "limit": 5 })] {
            let found = parse_mail_search(&run_against_fake(MAIL_SEARCH, input)).unwrap();
            assert_eq!(found.total, 1);
            assert_eq!((found.messages[0].id, found.messages[0].account.as_str(), found.messages[0].mailbox.as_str()), (42, "iCloud", "INBOX"));
        }
        // A cap that lands inside the emoji drops its first half instead of breaking the JSON.
        let message = parse_mail_read(&run_against_fake(MAIL_READ, json!({ "id": 42, "mailbox": "INBOX", "account": "iCloud", "cap": 10 }))).unwrap();
        assert_eq!(message.body, "aaaaaaaaa😀", "the emoji counts as one character");
        assert_eq!(message.to, ["Me <me@x.com>"]);
        assert_eq!(message.length, 19);
    }

    #[test]
    fn stops_a_program_that_runs_too_long() {
        assert_eq!(run("/bin/sleep", &["5"], Duration::from_millis(200)).unwrap_err(), "timed out");
        assert_eq!(run("/bin/echo", &["hi"], Duration::from_secs(5)).unwrap(), "hi\n");
        assert!(run("/bin/ls", &["/no/such/place"], Duration::from_secs(5)).unwrap_err().contains("No such file"));
    }

    /// Reads the real Mail app. Run by hand with Mail open: `cargo test mail_search_live -- --ignored`.
    #[test]
    #[ignore = "reads the real Mail app"]
    fn mail_search_live() {
        let query = MailQuery { query: None, mailbox: None, account: None, since: chrono::Utc::now().timestamp() - 3 * 86_400, until: None, limit: 3 };
        let found = mail_search(&query).unwrap();
        println!("{found:?}");
        if let Some(first) = found.messages.first() {
            let message = mail_read(&MailRef { id: first.id, mailbox: first.mailbox.clone(), account: first.account.clone() }, 200).unwrap();
            println!("{}: {}", message.subject, message.body);
        }
    }

    /// Lists the real Shortcuts without running any. `cargo test shortcuts_list_live -- --ignored`.
    #[test]
    #[ignore = "lists the real Shortcuts"]
    fn shortcuts_list_live() {
        println!("{:?}", shortcuts().unwrap());
    }
}
