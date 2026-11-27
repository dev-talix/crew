//! Calendar and Reminders through EventKit, and Contacts through the Contacts framework. Both read
//! the local stores directly, so a call takes milliseconds, and macOS shows its own permission prompt
//! the first time. Every function blocks; callers run them off the async threads.

use super::{denied, App, Calendar, Contact, ContactField, Event, NewEvent, NewReminder, Reminder};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObjectProtocol, ProtocolObject};
use objc2::sel;
use objc2_contacts::{
    CNAuthorizationStatus, CNContact, CNContactEmailAddressesKey, CNContactFamilyNameKey, CNContactGivenNameKey, CNContactNicknameKey, CNContactOrganizationNameKey,
    CNContactPhoneNumbersKey, CNContactStore, CNEntityType, CNKeyDescriptor, CNPhoneNumber,
};
use objc2_event_kit::{EKAuthorizationStatus, EKCalendar, EKEntityType, EKEvent, EKEventStore, EKReminder, EKSpan};
use objc2_foundation::{NSArray, NSBundle, NSCalendar, NSCalendarUnit, NSDate, NSDateComponents, NSError, NSString};
use std::sync::mpsc;
use std::time::Duration;

/// How long Grimoire waits for the person to answer macOS's permission prompt.
const PROMPT_WAIT: Duration = Duration::from_secs(120);
/// NSDateComponents' value for a field that isn't set.
const UNDEFINED: isize = isize::MAX;

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn date(secs: i64) -> Retained<NSDate> {
    NSDate::dateWithTimeIntervalSince1970(secs as f64)
}

fn secs(date: &NSDate) -> i64 {
    date.timeIntervalSince1970().floor() as i64
}

/// macOS ends the app when it asks for access without saying why in Info.plist, so a build without
/// the text gets an error instead.
fn has_usage_text(key: &str) -> bool {
    NSBundle::mainBundle().objectForInfoDictionaryKey(&ns(key)).is_some()
}

fn no_usage_text(app: App) -> String {
    format!("This copy of Grimoire can't ask for access to {}. Use the installed Grimoire app.", app.name())
}

/// Waits for a permission prompt's answer, which macOS gives on another thread.
fn wait_for_answer(ask: impl FnOnce(&block2::DynBlock<dyn Fn(Bool, *mut NSError)>)) -> bool {
    let (tx, rx) = mpsc::channel();
    let done = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
        let _ = tx.send(granted.as_bool());
    });
    ask(&done);
    rx.recv_timeout(PROMPT_WAIT).unwrap_or(false)
}

// ---- EventKit ----

fn entity(app: App) -> EKEntityType {
    if app == App::Reminders {
        EKEntityType::Reminder
    } else {
        EKEntityType::Event
    }
}

/// An event store with full access to `app`'s items, asking the person the first time.
fn event_store(app: App) -> Result<Retained<EKEventStore>, String> {
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(entity(app)) };
    if status == EKAuthorizationStatus::FullAccess {
        return Ok(unsafe { EKEventStore::new() });
    }
    if status != EKAuthorizationStatus::NotDetermined {
        return Err(denied(app));
    }
    let store = unsafe { EKEventStore::new() };
    // macOS 14 split calendar access into full and write-only, with new keys and methods.
    let modern = store.respondsToSelector(sel!(requestFullAccessToEventsWithCompletion:));
    let key = match (app, modern) {
        (App::Reminders, true) => "NSRemindersFullAccessUsageDescription",
        (App::Reminders, false) => "NSRemindersUsageDescription",
        (_, true) => "NSCalendarsFullAccessUsageDescription",
        (_, false) => "NSCalendarsUsageDescription",
    };
    if !has_usage_text(key) {
        return Err(no_usage_text(app));
    }
    let granted = wait_for_answer(|done| unsafe {
        let done = done as *const _ as *mut _;
        match (app, modern) {
            (App::Reminders, true) => store.requestFullAccessToRemindersWithCompletion(done),
            (_, true) => store.requestFullAccessToEventsWithCompletion(done),
            #[allow(deprecated)]
            (_, false) => store.requestAccessToEntityType_completion(entity(app), done),
        }
    });
    if !granted {
        return Err(denied(app));
    }
    // A store made before access was granted stays empty.
    Ok(unsafe { EKEventStore::new() })
}

fn all_calendars(store: &EKEventStore, app: App) -> Vec<Retained<EKCalendar>> {
    unsafe { store.calendarsForEntityType(entity(app)) }.to_vec()
}

/// Every calendar or list with this title, for reading; two accounts can use the same one.
fn named(store: &EKEventStore, app: App, name: &str) -> Result<Retained<NSArray<EKCalendar>>, String> {
    let found: Vec<_> = all_calendars(store, app).into_iter().filter(|c| unsafe { c.title() }.to_string().eq_ignore_ascii_case(name)).collect();
    if found.is_empty() {
        return Err(format!("There's no {} called \"{name}\".", if app == App::Reminders { "list" } else { "calendar" }));
    }
    Ok(NSArray::from_retained_slice(&found))
}

/// The calendar or list the person approved, by its identifier.
fn approved(store: &EKEventStore, id: Option<&str>) -> Result<Retained<EKCalendar>, String> {
    let id = id.ok_or("Grimoire didn't pick a calendar.")?;
    unsafe { store.calendarWithIdentifier(&ns(id)) }.ok_or_else(|| "That calendar is gone. Try again.".into())
}

pub fn calendars(app: App) -> Result<Vec<Calendar>, String> {
    let store = event_store(app)?;
    let default = unsafe {
        match app {
            App::Reminders => store.defaultCalendarForNewReminders(),
            _ => store.defaultCalendarForNewEvents(),
        }
    }
    .map(|c| unsafe { c.calendarIdentifier() }.to_string());
    Ok(all_calendars(&store, app)
        .iter()
        .map(|c| unsafe {
            let id = c.calendarIdentifier().to_string();
            Calendar {
                default: default.as_deref() == Some(id.as_str()),
                id,
                title: c.title().to_string(),
                account: c.source().map(|s| s.title().to_string()).unwrap_or_default(),
                writable: c.allowsContentModifications(),
            }
        })
        .collect())
}

fn optional_text(text: Option<Retained<NSString>>) -> Option<String> {
    text.map(|t| t.to_string()).filter(|t| !t.trim().is_empty())
}

pub fn events(from: i64, to: i64, calendar: Option<&str>) -> Result<Vec<Event>, String> {
    let store = event_store(App::Calendar)?;
    let only = calendar.map(|name| named(&store, App::Calendar, name)).transpose()?;
    let events = unsafe {
        let predicate = store.predicateForEventsWithStartDate_endDate_calendars(&date(from), &date(to), only.as_deref());
        store.eventsMatchingPredicate(&predicate)
    };
    Ok(events
        .iter()
        .map(|e| unsafe {
            Event {
                title: e.title().to_string(),
                start: secs(&e.startDate()),
                end: secs(&e.endDate()),
                all_day: e.isAllDay(),
                calendar: e.calendar().map(|c| c.title().to_string()).unwrap_or_default(),
                location: optional_text(e.location()),
                notes: optional_text(e.notes()),
            }
        })
        .collect())
}

fn save_error(error: Retained<NSError>) -> String {
    format!("macOS didn't save it: {}", error.localizedDescription())
}

pub fn create_event(new: &NewEvent) -> Result<(), String> {
    let store = event_store(App::Calendar)?;
    let calendar = approved(&store, new.calendar.as_deref())?;
    unsafe {
        let event = EKEvent::eventWithEventStore(&store);
        event.setTitle(Some(&ns(&new.title)));
        event.setStartDate(Some(&date(new.start)));
        event.setEndDate(Some(&date(new.end)));
        event.setAllDay(new.all_day);
        event.setCalendar(Some(&calendar));
        event.setLocation(new.location.as_deref().map(ns).as_deref());
        event.setNotes(new.notes.as_deref().map(ns).as_deref());
        store.saveEvent_span_commit_error(&event, EKSpan::ThisEvent, true).map_err(save_error)
    }
}

fn due_of(reminder: &EKReminder) -> Option<(i64, bool)> {
    let components = unsafe { reminder.dueDateComponents() }?;
    let with_time = components.hour() != UNDEFINED;
    let at = NSCalendar::currentCalendar().dateFromComponents(&components)?;
    Some((secs(&at), with_time))
}

pub fn reminders(list: Option<&str>, include_completed: bool) -> Result<Vec<Reminder>, String> {
    let store = event_store(App::Reminders)?;
    let only = list.map(|name| named(&store, App::Reminders, name)).transpose()?;
    let predicate = unsafe {
        if include_completed {
            store.predicateForRemindersInCalendars(only.as_deref())
        } else {
            store.predicateForIncompleteRemindersWithDueDateStarting_ending_calendars(None, None, only.as_deref())
        }
    };
    let (tx, rx) = mpsc::channel();
    // The completion runs on EventKit's queue; it copies what Grimoire needs into plain values there.
    let done = RcBlock::new(move |found: *mut NSArray<EKReminder>| {
        let list = unsafe { found.as_ref() }
            .map(|found| {
                found
                    .iter()
                    .map(|r| unsafe {
                        Reminder {
                            title: r.title().to_string(),
                            due: due_of(&r),
                            completed: r.isCompleted(),
                            list: r.calendar().map(|c| c.title().to_string()).unwrap_or_default(),
                            notes: optional_text(r.notes()),
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let _ = tx.send(list);
    });
    unsafe { store.fetchRemindersMatchingPredicate_completion(&predicate, &done) };
    rx.recv_timeout(Duration::from_secs(30)).map_err(|_| "Reminders took too long to answer.".to_string())
}

pub fn add_reminder(new: &NewReminder) -> Result<(), String> {
    let store = event_store(App::Reminders)?;
    let list = approved(&store, new.list.as_deref())?;
    unsafe {
        let reminder = EKReminder::reminderWithEventStore(&store);
        reminder.setTitle(Some(&ns(&new.title)));
        reminder.setCalendar(Some(&list));
        reminder.setNotes(new.notes.as_deref().map(ns).as_deref());
        if let Some((at, with_time)) = new.due {
            let mut units = NSCalendarUnit::Year | NSCalendarUnit::Month | NSCalendarUnit::Day;
            if with_time {
                units |= NSCalendarUnit::Hour | NSCalendarUnit::Minute;
            }
            let components: Retained<NSDateComponents> = NSCalendar::currentCalendar().components_fromDate(units, &date(at));
            reminder.setDueDateComponents(Some(&components));
        }
        store.saveReminder_commit_error(&reminder, true).map_err(save_error)
    }
}

// ---- Contacts ----

fn contact_store() -> Result<Retained<CNContactStore>, String> {
    let status = unsafe { CNContactStore::authorizationStatusForEntityType(CNEntityType::Contacts) };
    let store = unsafe { CNContactStore::new() };
    if status == CNAuthorizationStatus::Authorized || status == CNAuthorizationStatus::Limited {
        return Ok(store);
    }
    if status != CNAuthorizationStatus::NotDetermined {
        return Err(denied(App::Contacts));
    }
    if !has_usage_text("NSContactsUsageDescription") {
        return Err(no_usage_text(App::Contacts));
    }
    if wait_for_answer(|done| unsafe { store.requestAccessForEntityType_completionHandler(CNEntityType::Contacts, done) }) {
        Ok(store)
    } else {
        Err(denied(App::Contacts))
    }
}

pub fn contacts(field: ContactField, query: &str) -> Result<Vec<Contact>, String> {
    let store = contact_store()?;
    let predicate = unsafe {
        match field {
            ContactField::Name => CNContact::predicateForContactsMatchingName(&ns(query)),
            ContactField::Email => CNContact::predicateForContactsMatchingEmailAddress(&ns(query)),
            ContactField::Phone => {
                let number = CNPhoneNumber::phoneNumberWithStringValue(&ns(query)).ok_or("That doesn't look like a phone number.")?;
                CNContact::predicateForContactsMatchingPhoneNumber(&number)
            }
        }
    };
    let keys: Vec<&ProtocolObject<dyn CNKeyDescriptor>> = unsafe {
        [CNContactGivenNameKey, CNContactFamilyNameKey, CNContactNicknameKey, CNContactOrganizationNameKey, CNContactEmailAddressesKey, CNContactPhoneNumbersKey]
            .into_iter()
            .map(ProtocolObject::from_ref)
            .collect()
    };
    let found = unsafe { store.unifiedContactsMatchingPredicate_keysToFetch_error(&predicate, &NSArray::from_slice(&keys)) }
        .map_err(|e| format!("Contacts didn't answer: {}", e.localizedDescription()))?;
    Ok(found
        .iter()
        .map(|c| unsafe {
            let name = [c.givenName().to_string(), c.familyName().to_string()].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
            let organization = optional_text(Some(c.organizationName()));
            let name = match (name.is_empty(), optional_text(Some(c.nickname()))) {
                (false, _) => name,
                (true, Some(nick)) => nick,
                (true, None) => organization.clone().unwrap_or_default(),
            };
            Contact {
                name,
                organization,
                emails: c.emailAddresses().iter().map(|e| e.value().to_string()).collect(),
                phones: c.phoneNumbers().iter().map(|p| p.value().stringValue().to_string()).collect(),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checking access never prompts and reads nothing, so it runs everywhere.
    #[test]
    fn checks_access_without_asking() {
        let calendar = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
        let contacts = unsafe { CNContactStore::authorizationStatusForEntityType(CNEntityType::Contacts) };
        assert!((0..=4).contains(&calendar.0), "{calendar:?}");
        assert!((0..=4).contains(&contacts.0), "{contacts:?}");
    }

    /// macOS ends an app that asks without a reason in Info.plist, and a hardened app needs the
    /// entitlement as well, so both files carry every key this module asks with.
    #[test]
    fn info_plist_and_entitlements_name_every_key() {
        let plist = include_str!("../../Info.plist");
        for key in ["NSCalendarsFullAccessUsageDescription", "NSCalendarsUsageDescription", "NSRemindersFullAccessUsageDescription", "NSRemindersUsageDescription", "NSContactsUsageDescription", "NSAppleEventsUsageDescription"] {
            assert!(plist.contains(&format!("<key>{key}</key>")), "{key}");
        }
        let entitlements = include_str!("../../Entitlements.plist");
        for key in ["com.apple.security.personal-information.calendars", "com.apple.security.personal-information.addressbook", "com.apple.security.automation.apple-events"] {
            assert!(entitlements.contains(&format!("<key>{key}</key>")), "{key}");
        }
    }

    /// Reads the real calendars and contacts after macOS asks. Run by hand from the bundled app's
    /// context only: `cargo test native_live -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the real calendars, reminders, and contacts"]
    fn native_live() {
        let now = chrono::Utc::now().timestamp();
        println!("{:?}", calendars(App::Calendar));
        println!("{:?}", events(now, now + 86_400, None).map(|e| e.len()));
        println!("{:?}", reminders(None, false).map(|r| r.len()));
        println!("{:?}", contacts(ContactField::Name, "a").map(|c| c.len()));
    }
}
