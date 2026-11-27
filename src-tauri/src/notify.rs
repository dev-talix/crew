//! macOS notifications: a familiar finished or failed, a familiar needs an OK, a ritual ran, or a
//! spell ended. Clicking one opens the main window on that familiar; an approval also gets Allow and
//! Deny buttons. The decisions and the text are plain functions; `mac` talks to
//! UNUserNotificationCenter, which only works from a bundled Grimoire.app.

use crate::approvals::{Approvals, Decision, Request};
use crate::db::Db;
use crate::runtime::TurnOutcome;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

/// The settings key for `Prefs`, stored as JSON.
const KEY: &str = "notifications";
const BODY_CHARS: usize = 180;
/// Action identifiers on an approval notification.
const ALLOW: &str = "allow";
const DENY: &str = "deny";
const DISMISS: &str = "com.apple.UNNotificationDismissActionIdentifier";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    pub replies: bool,
    pub approvals: bool,
    /// Rituals and spells.
    pub runs: bool,
    /// Only while the main window is hidden, minimized, or not focused.
    pub background_only: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { replies: true, approvals: true, runs: true, background_only: true }
    }
}

pub fn load(db: &Db) -> Prefs {
    db.setting(KEY).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Reply,
    Approval,
    Run,
}

/// Whether a notification of `kind` should go out, given the settings and whether Grimoire's main
/// window is in the background.
pub fn wanted(prefs: &Prefs, kind: Kind, in_background: bool) -> bool {
    let on = match kind {
        Kind::Reply => prefs.replies,
        Kind::Approval => prefs.approvals,
        Kind::Run => prefs.runs,
    };
    on && (in_background || !prefs.background_only)
}

/// Where clicking a notification goes. It travels in the notification's identifier, so a click
/// after a restart still finds its way.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Agent(String),
    Approval { agent_id: String, id: String },
}

impl Target {
    /// Approvals get a fixed identifier so answering in the window can take the notification down.
    fn identifier(&self) -> String {
        match self {
            Target::Agent(agent_id) => format!("agent|{agent_id}|{}", uuid::Uuid::new_v4()),
            Target::Approval { agent_id, id } => format!("approval|{agent_id}|{id}"),
        }
    }

    fn parse(identifier: &str) -> Option<Target> {
        let mut parts = identifier.splitn(3, '|');
        let (kind, agent_id, rest) = (parts.next()?, parts.next()?.to_string(), parts.next()?.to_string());
        match kind {
            "agent" => Some(Target::Agent(agent_id)),
            "approval" => Some(Target::Approval { agent_id, id: rest }),
            _ => None,
        }
    }

    fn agent_id(&self) -> &str {
        match self {
            Target::Agent(agent_id) | Target::Approval { agent_id, .. } => agent_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub title: String,
    pub body: String,
    pub target: Target,
}

/// What a click or a button on a notification asks for.
#[derive(Debug, PartialEq)]
enum Response {
    Open(String),
    Answer(String, Decision),
    Nothing,
}

fn response_for(identifier: &str, action: &str) -> Response {
    let Some(target) = Target::parse(identifier) else { return Response::Nothing };
    match (&target, action) {
        (_, DISMISS) => Response::Nothing,
        (Target::Approval { id, .. }, ALLOW) => Response::Answer(id.clone(), Decision::Once),
        (Target::Approval { id, .. }, DENY) => Response::Answer(id.clone(), Decision::Deny),
        _ => Response::Open(target.agent_id().to_string()),
    }
}

/// The first line with text, without Markdown heading or quote marks, cut to fit a notification.
fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_start_matches(['#', '>']).trim();
    clip(line)
}

fn clip(text: &str) -> String {
    if text.chars().count() <= BODY_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(BODY_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

fn reply_text(name: &str, outcome: &TurnOutcome) -> Option<(String, String)> {
    match outcome {
        TurnOutcome::Done(text) => {
            let body = first_line(text);
            Some((format!("{name} finished"), if body.is_empty() { "The reply is ready.".into() } else { body }))
        }
        TurnOutcome::Failed(reason) => Some((format!("{name}'s turn failed"), first_line(reason))),
        TurnOutcome::Stopped => None,
    }
}

pub(crate) fn approval_text(name: &str, request: &Request) -> (String, String) {
    let body = match request.kind {
        // The title reads "Researcher wants to use the password for github.com".
        "vault" => match request.title.strip_prefix(&format!("{name} wants to ")) {
            Some(rest) => capitalize(rest),
            None => request.title.clone(),
        },
        _ => clip(request.detail.trim()),
    };
    (format!("{name} needs your OK"), body)
}

pub(crate) fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub(crate) fn ritual_text(ritual: &str, name: &str) -> (String, String) {
    (format!("{ritual} ran"), format!("{name} is working on it."))
}

/// `step` is the step a spell stopped on; `detail` is the last reply when it finished, or the error.
pub(crate) fn spell_text(spell: &str, state: &str, step: usize, total: usize, detail: Option<&str>) -> Option<(String, String)> {
    let line = detail.map(first_line).filter(|l| !l.is_empty());
    match state {
        "done" => Some((format!("{spell} finished"), line.unwrap_or_else(|| format!("All {total} steps are done.")))),
        "failed" => Some((format!("{spell} failed at step {step}"), line.unwrap_or_else(|| "The familiar couldn't finish.".into()))),
        _ => None,
    }
}

pub struct Notifier {
    db: Arc<Db>,
    approvals: Arc<Approvals>,
}

impl Notifier {
    pub fn new(db: Arc<Db>, approvals: Arc<Approvals>) -> Self {
        Notifier { db, approvals }
    }

    fn agent_name(&self, agent_id: &str) -> String {
        self.db.get_agent(agent_id).ok().flatten().map_or_else(|| "A familiar".into(), |a| a.name)
    }
}

/// Hidden, minimized, or behind another app's window.
pub fn in_background(app: &AppHandle) -> bool {
    let Some(main) = app.get_webview_window("main") else { return true };
    // The window keeps reporting focus after Grimoire opens in the background, so the app has to be
    // frontmost too.
    let up = mac::active() && main.is_visible().unwrap_or(false) && !main.is_minimized().unwrap_or(false) && main.is_focused().unwrap_or(false);
    !up
}

fn send(app: &AppHandle, kind: Kind, make: impl FnOnce(&Notifier) -> Option<Note>) {
    let Some(notifier) = app.try_state::<Notifier>() else { return };
    if !wanted(&load(&notifier.db), kind, in_background(app)) {
        return;
    }
    if let Some(note) = make(&notifier) {
        mac::post(note);
    }
}

/// A familiar's turn ended. Spell steps stay quiet; the spell says when it ends.
pub fn turn_ended(app: &AppHandle, agent_id: &str, source: Option<&str>, outcome: &TurnOutcome) {
    if crate::runtime::origin_of(source) == "spell" {
        return;
    }
    send(app, Kind::Reply, |n| {
        let (title, body) = reply_text(&n.agent_name(agent_id), outcome)?;
        Some(Note { title, body, target: Target::Agent(agent_id.into()) })
    });
}

pub fn approval_requested(app: &AppHandle, request: &Request) {
    crate::telegram::approval_requested(request);
    send(app, Kind::Approval, |n| {
        let (title, body) = approval_text(&n.agent_name(&request.agent_id), request);
        Some(Note { title, body, target: Target::Approval { agent_id: request.agent_id.clone(), id: request.id.clone() } })
    });
}

/// Takes down an approval's notification once it's answered or timed out.
pub fn approval_closed(request: &Request) {
    mac::withdraw(&Target::Approval { agent_id: request.agent_id.clone(), id: request.id.clone() }.identifier());
}

/// A familiar drafted a message for the person to send. `what` reads like "email draft to a@example.com".
pub fn draft_ready(app: &AppHandle, agent_id: &str, what: &str, body: &str) {
    send(app, Kind::Approval, |n| {
        let body = clip(&format!("{}: {}", capitalize(what), first_line(body)));
        Some(Note { title: format!("{} wrote a draft for you to send", n.agent_name(agent_id)), body, target: Target::Agent(agent_id.into()) })
    });
}

pub fn ritual_fired(app: &AppHandle, ritual: &str, agent_id: &str) {
    crate::telegram::ritual_fired(ritual, agent_id);
    send(app, Kind::Run, |n| {
        let (title, body) = ritual_text(ritual, &n.agent_name(agent_id));
        Some(Note { title, body, target: Target::Agent(agent_id.into()) })
    });
}

pub fn spell_ended(app: &AppHandle, spell: &str, state: &str, step: usize, total: usize, agent_id: &str, detail: Option<&str>) {
    crate::telegram::spell_ended(spell, state, step, total, detail);
    send(app, Kind::Run, |_| {
        let (title, body) = spell_text(spell, state, step, total, detail)?;
        Some(Note { title, body, target: Target::Agent(agent_id.into()) })
    });
}

/// Acts on a click or a button press on one of Grimoire's notifications.
fn respond(app: &AppHandle, identifier: &str, action: &str) {
    match response_for(identifier, action) {
        Response::Open(agent_id) => {
            let _ = crate::dock::show_main(app, Some(agent_id));
        }
        Response::Answer(id, decision) => {
            if let Some(notifier) = app.try_state::<Notifier>() {
                let _ = notifier.approvals.answer(&id, decision);
            }
        }
        Response::Nothing => {}
    }
}

pub fn install(app: &AppHandle) {
    mac::install(app);
}

#[tauri::command]
pub fn notification_settings(notifier: State<Notifier>) -> Prefs {
    load(&notifier.db)
}

#[tauri::command]
pub fn save_notification_settings(notifier: State<Notifier>, prefs: Prefs) -> Result<Prefs, String> {
    notifier.db.set_setting(KEY, &serde_json::to_string(&prefs).map_err(|e| e.to_string())?)?;
    Ok(prefs)
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{Note, Target, ALLOW, DENY};
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AnyThread};
    use objc2_foundation::{NSArray, NSBundle, NSError, NSSet, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction, UNNotificationActionOptions,
        UNNotificationCategory, UNNotificationCategoryOptions, UNNotificationPresentationOptions, UNNotificationRequest,
        UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use std::sync::{Mutex, OnceLock};
    use tauri::AppHandle;

    const CATEGORY: &str = "approval";

    /// Set once the notification center is usable.
    static APP: OnceLock<AppHandle> = OnceLock::new();
    static AUTH: Mutex<Auth> = Mutex::new(Auth::Unasked);

    enum Auth {
        Unasked,
        /// Notes that wait for the person to answer the permission prompt.
        Asking(Vec<Note>),
        Asked,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "GrimoireNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            /// Shows banners even while Grimoire is in front, for when "only in the background" is off.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, handler: &block2::DynBlock<dyn Fn()>) {
                let identifier = response.notification().request().identifier().to_string();
                let action = response.actionIdentifier().to_string();
                if let Some(app) = APP.get() {
                    super::respond(app, &identifier, &action);
                }
                handler.call(());
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            unsafe { msg_send![super(this), init] }
        }
    }

    /// UNUserNotificationCenter throws, and takes the app down, outside an app bundle such as
    /// under `tauri dev`.
    fn bundled() -> bool {
        let bundle = NSBundle::mainBundle();
        bundle.bundleIdentifier().is_some() && bundle.bundlePath().to_string().ends_with(".app")
    }

    pub fn install(app: &AppHandle) {
        if !bundled() {
            eprintln!("Notifications are off: Grimoire isn't running from Grimoire.app.");
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = Delegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly, and this one lives as long as the app.
        std::mem::forget(delegate);
        let allow = UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(ALLOW),
            &NSString::from_str("Allow"),
            UNNotificationActionOptions::empty(),
        );
        let deny = UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(DENY),
            &NSString::from_str("Deny"),
            UNNotificationActionOptions::Destructive,
        );
        let category = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(CATEGORY),
            &NSArray::from_retained_slice(&[allow, deny]),
            &NSArray::new(),
            UNNotificationCategoryOptions::empty(),
        );
        center.setNotificationCategories(&NSSet::from_retained_slice(&[category]));
        let _ = APP.set(app.clone());
    }

    /// Posts `note`, asking for permission first if this is the first one since launch. macOS shows
    /// the prompt only once; later asks return the person's answer.
    pub fn post(note: Note) {
        if APP.get().is_none() {
            return;
        }
        let Ok(mut auth) = AUTH.lock() else { return };
        match &mut *auth {
            Auth::Asked => add(&note),
            Auth::Asking(waiting) => waiting.push(note),
            Auth::Unasked => {
                *auth = Auth::Asking(vec![note]);
                drop(auth);
                ask();
            }
        }
    }

    fn ask() {
        let done = RcBlock::new(|granted: Bool, _error: *mut NSError| {
            let Ok(mut auth) = AUTH.lock() else { return };
            if let Auth::Asking(waiting) = std::mem::replace(&mut *auth, Auth::Asked) {
                if granted.as_bool() {
                    waiting.iter().for_each(add);
                }
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &done);
    }

    fn add(note: &Note) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&note.title));
        content.setBody(&NSString::from_str(&note.body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        content.setThreadIdentifier(&NSString::from_str(note.target.agent_id()));
        if matches!(note.target, Target::Approval { .. }) {
            content.setCategoryIdentifier(&NSString::from_str(CATEGORY));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&note.target.identifier()), &content, None);
        UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, None);
    }

    pub fn active() -> bool {
        objc2_app_kit::NSRunningApplication::currentApplication().isActive()
    }

    pub fn withdraw(identifier: &str) {
        if APP.get().is_none() {
            return;
        }
        let ids = NSArray::from_retained_slice(&[NSString::from_str(identifier)]);
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.removeDeliveredNotificationsWithIdentifiers(&ids);
        center.removePendingNotificationRequestsWithIdentifiers(&ids);
    }
}

#[cfg(not(target_os = "macos"))]
mod mac {
    pub fn install(_app: &tauri::AppHandle) {}
    pub fn post(_note: super::Note) {}
    pub fn withdraw(_identifier: &str) {}
    pub fn active() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_the_toggles_and_the_background_rule() {
        let prefs = Prefs::default();
        assert!(wanted(&prefs, Kind::Reply, true));
        assert!(!wanted(&prefs, Kind::Reply, false), "background only is on by default");
        let anytime = Prefs { background_only: false, ..prefs };
        assert!(wanted(&anytime, Kind::Approval, false));
        let quiet = Prefs { replies: false, runs: false, ..prefs };
        assert!(!wanted(&quiet, Kind::Reply, true));
        assert!(!wanted(&quiet, Kind::Run, true));
        assert!(wanted(&quiet, Kind::Approval, true));
    }

    #[test]
    fn stored_settings_fill_in_missing_toggles() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(load(&db), Prefs::default());
        db.set_setting(KEY, r#"{"replies":false}"#).unwrap();
        assert_eq!(load(&db), Prefs { replies: false, ..Prefs::default() });
        db.set_setting(KEY, "not json").unwrap();
        assert_eq!(load(&db), Prefs::default());
    }

    #[test]
    fn finished_replies_show_their_first_line() {
        let done = TurnOutcome::Done("\n## Summary\nThree things changed.\nMore".into());
        assert_eq!(reply_text("Scribe", &done), Some(("Scribe finished".into(), "Summary".into())));
        assert_eq!(reply_text("Scribe", &TurnOutcome::Done(String::new())).unwrap().1, "The reply is ready.");
        let failed = TurnOutcome::Failed("Claude is not signed in.\nRun claude login".into());
        assert_eq!(reply_text("Scribe", &failed), Some(("Scribe's turn failed".into(), "Claude is not signed in.".into())));
        assert_eq!(reply_text("Scribe", &TurnOutcome::Stopped), None);
    }

    #[test]
    fn long_lines_are_cut() {
        let body = first_line(&"word ".repeat(100));
        assert_eq!(body.chars().count(), BODY_CHARS);
        assert!(body.ends_with('…'));
    }

    #[test]
    fn approvals_show_the_command_or_the_vault_item() {
        let mut request = Request {
            id: "r1".into(),
            agent_id: "a3".into(),
            kind: "command",
            title: "Warden wants to run a command".into(),
            detail: "  git push origin main\n".into(),
            can_remember: true,
        };
        assert_eq!(approval_text("Warden", &request), ("Warden needs your OK".into(), "git push origin main".into()));
        request.kind = "vault";
        request.title = "Warden wants to use the password for github.com".into();
        assert_eq!(approval_text("Warden", &request).1, "Use the password for github.com");
    }

    #[test]
    fn rituals_and_spells_say_what_happened() {
        assert_eq!(ritual_text("Morning briefing", "Scribe"), ("Morning briefing ran".into(), "Scribe is working on it.".into()));
        assert_eq!(spell_text("Topic brief", "done", 2, 2, Some("Brief: solar is up")), Some(("Topic brief finished".into(), "Brief: solar is up".into())));
        assert_eq!(spell_text("Topic brief", "done", 2, 2, None).unwrap().1, "All 2 steps are done.");
        assert_eq!(spell_text("Topic brief", "failed", 2, 3, Some("Timed out")), Some(("Topic brief failed at step 2".into(), "Timed out".into())));
        assert_eq!(spell_text("Topic brief", "stopped", 1, 2, None), None);
    }

    #[test]
    fn clicks_open_the_familiar_and_buttons_answer() {
        let approval = Target::Approval { agent_id: "a3".into(), id: "call|7".into() }.identifier();
        assert_eq!(Target::parse(&approval), Some(Target::Approval { agent_id: "a3".into(), id: "call|7".into() }));
        assert_eq!(response_for(&approval, ALLOW), Response::Answer("call|7".into(), Decision::Once));
        assert_eq!(response_for(&approval, DENY), Response::Answer("call|7".into(), Decision::Deny));
        assert_eq!(response_for(&approval, "com.apple.UNNotificationDefaultActionIdentifier"), Response::Open("a3".into()));
        let reply = Target::Agent("a2".into()).identifier();
        assert_eq!(response_for(&reply, "com.apple.UNNotificationDefaultActionIdentifier"), Response::Open("a2".into()));
        assert_eq!(response_for(&reply, DISMISS), Response::Nothing);
        assert_eq!(response_for("someone-else", "x"), Response::Nothing);
    }
}
