//! Files and images attached to a message. The composer stages each file's bytes first; sending
//! moves them to `attachments/<message id>/` in the app data folder, with one row per file.

use crate::db::{Db, Message};
use crate::AppState;
use rusqlite::params;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

pub const MAX_FILES: usize = 10;
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
const STAGING: &str = "staging";
/// The header that carries a staged file's name, percent-encoded.
const NAME_HEADER: &str = "x-file-name";
/// Serves stored images to the window at `attachment://localhost/<id>`.
pub const SCHEME: &str = "attachment";

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    File,
}

impl Kind {
    fn of(bytes: &[u8]) -> Self {
        if image_type(bytes).is_some() {
            Kind::Image
        } else {
            Kind::File
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::File => "file",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: i64,
    pub message_id: i64,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StagedFile {
    pub token: String,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
}

/// The image formats both Codex and Claude accept, by their first bytes.
pub fn image_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("attachments")
}

fn app_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map(|dir| root(&dir)).map_err(|e| e.to_string())
}

/// A file name that stays inside its folder: no separators, no leading dots, no control characters.
pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let cleaned: String = cleaned.chars().take(120).collect();
    if cleaned.is_empty() {
        "file".into()
    } else {
        cleaned
    }
}

/// Picks `name`, or `name-2.ext` and up when another attachment in the message already has it.
fn unique_name(taken: &mut HashSet<String>, name: &str) -> String {
    let (stem, ext) = match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    let mut candidate = name.to_string();
    let mut n = 2;
    while !taken.insert(candidate.to_lowercase()) {
        candidate = format!("{stem}-{n}{ext}");
        n += 1;
    }
    candidate
}

fn decode_name(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Saves bytes from the composer until the message they belong to is sent.
pub fn stage(root: &Path, name: &str, bytes: &[u8]) -> Result<StagedFile, String> {
    let name = clean_name(name);
    let kind = Kind::of(bytes);
    let size = bytes.len() as u64;
    if size > MAX_FILE_BYTES {
        return Err(format!("{name} is over 20 MB. Attachments can be up to 20 MB."));
    }
    let token = uuid::Uuid::new_v4().to_string();
    let dir = root.join(STAGING).join(&token);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not save {name}: {e}"))?;
    std::fs::write(dir.join(&name), bytes).map_err(|e| format!("Could not save {name}: {e}"))?;
    Ok(StagedFile { token, name, kind, size })
}

/// Drops files staged for messages that were never sent.
pub fn clear_staging(root: &Path) {
    let _ = std::fs::remove_dir_all(root.join(STAGING));
}

/// Staged files on their way into a message.
#[derive(Debug, Default)]
pub struct Staged {
    root: PathBuf,
    tokens: Vec<String>,
}

impl Staged {
    pub fn new(root: PathBuf, tokens: Vec<String>) -> Result<Self, String> {
        if tokens.len() > MAX_FILES {
            return Err(format!("Attach up to {MAX_FILES} files at a time."));
        }
        if tokens.iter().any(|t| uuid::Uuid::parse_str(t).is_err()) {
            return Err("That attachment is no longer available. Attach it again.".into());
        }
        Ok(Staged { root, tokens })
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Moves the staged files into `attachments/<message id>/` and records them. On failure it
    /// leaves nothing behind for this message.
    pub fn store(&self, db: &Db, message_id: i64) -> Result<Vec<Attachment>, String> {
        if self.is_empty() {
            return Ok(Vec::new());
        }
        let dir = self.root.join(message_id.to_string());
        let stored = self.move_into(db, message_id, &dir);
        if stored.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
            let _ = db.with(|c| c.execute("DELETE FROM attachments WHERE message_id = ?1", params![message_id]));
        }
        stored
    }

    fn move_into(&self, db: &Db, message_id: i64, dir: &Path) -> Result<Vec<Attachment>, String> {
        let missing = || "An attachment went missing before it was sent. Attach it again.".to_string();
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let mut taken = HashSet::new();
        let mut stored = Vec::new();
        for token in &self.tokens {
            let staged = self.root.join(STAGING).join(token);
            let file = std::fs::read_dir(&staged).map_err(|_| missing())?.flatten().next().ok_or_else(missing)?;
            let name = unique_name(&mut taken, &file.file_name().to_string_lossy());
            let path = dir.join(&name);
            std::fs::rename(file.path(), &path).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_dir(&staged);
            let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            let kind = Kind::of(&read_head(&path));
            let path = path.to_string_lossy().into_owned();
            let id = db.with(|c| {
                c.execute(
                    "INSERT INTO attachments (message_id, name, kind, size, path) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![message_id, name, kind.as_str(), size as i64, path],
                )?;
                Ok(c.last_insert_rowid())
            })?;
            stored.push(Attachment { id, message_id, name, kind, size, path });
        }
        Ok(stored)
    }
}

fn read_head(path: &Path) -> Vec<u8> {
    use std::io::Read;
    let mut head = Vec::with_capacity(16);
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(16).read_to_end(&mut head);
    }
    head
}

/// Removes a message whose attachments could not be stored.
pub fn drop_message(db: &Db, message_id: i64) {
    let _ = db.with(|c| c.execute("DELETE FROM messages WHERE id = ?1", params![message_id]));
}

fn row_to_attachment(r: &rusqlite::Row) -> rusqlite::Result<Attachment> {
    let kind: String = r.get(3)?;
    Ok(Attachment {
        id: r.get(0)?,
        message_id: r.get(1)?,
        name: r.get(2)?,
        kind: if kind == "image" { Kind::Image } else { Kind::File },
        size: r.get::<_, i64>(4)? as u64,
        path: r.get(5)?,
    })
}

/// Fills in each message's attachments.
pub fn attach(db: &Db, messages: &mut [Message]) -> Result<(), String> {
    let (Some(first), Some(last)) = (messages.first().map(|m| m.id), messages.last().map(|m| m.id)) else {
        return Ok(());
    };
    let rows = db.with(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT id, message_id, name, kind, size, path FROM attachments WHERE message_id BETWEEN ?1 AND ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![first.min(last), first.max(last)], row_to_attachment)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    let mut by_message: HashMap<i64, Vec<Attachment>> = HashMap::new();
    for row in rows {
        by_message.entry(row.message_id).or_default().push(row);
    }
    for message in messages {
        if let Some(found) = by_message.remove(&message.id) {
            message.attachments = found;
        }
    }
    Ok(())
}

fn find(db: &Db, id: i64) -> Result<Option<Attachment>, String> {
    use rusqlite::OptionalExtension;
    db.with(|c| {
        c.prepare_cached("SELECT id, message_id, name, kind, size, path FROM attachments WHERE id = ?1")?
            .query_row(params![id], row_to_attachment)
            .optional()
    })
}

/// Deletes the stored files of a familiar's messages. Its rows go with the messages.
pub fn remove_for_agent(db: &Db, root: &Path, agent_id: &str) {
    let ids = db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT DISTINCT a.message_id FROM attachments a JOIN messages m ON m.id = a.message_id WHERE m.agent_id = ?1",
        )?;
        let rows = stmt.query_map(params![agent_id], |r| r.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    });
    for id in ids.unwrap_or_default() {
        let _ = std::fs::remove_dir_all(root.join(id.to_string()));
    }
}

/// What a turn sends to the model.
#[derive(Debug, Default, PartialEq)]
pub struct TurnInput {
    pub text: String,
    /// Images that go to the model as images.
    pub images: Vec<PathBuf>,
}

/// Images go to the model as they are. Other files are copied into the familiar's workspace under
/// `attachments/<message id>/`, and a note at the end of the text gives their paths, so the familiar
/// reads them with its own tools.
pub fn turn_input(workspace: &Path, text: &str, attachments: &[Attachment]) -> Result<TurnInput, String> {
    let mut input = TurnInput { text: text.to_string(), images: Vec::new() };
    let mut copied = Vec::new();
    for attachment in attachments {
        let path = PathBuf::from(&attachment.path);
        if attachment.kind == Kind::Image {
            input.images.push(path);
            continue;
        }
        let dir = workspace.join("attachments").join(attachment.message_id.to_string());
        let target = dir.join(&attachment.name);
        std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::copy(&path, &target))
            .map_err(|e| format!("Could not copy {} into the workspace: {e}", attachment.name))?;
        copied.push(target);
    }
    if !copied.is_empty() {
        let list: Vec<String> = copied.iter().map(|p| format!("- {}", p.display())).collect();
        let note = format!("Attached files, copied into your workspace:\n{}", list.join("\n"));
        input.text = if input.text.is_empty() { note } else { format!("{}\n\n{note}", input.text) };
    }
    Ok(input)
}

#[tauri::command]
pub fn stage_attachment(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<StagedFile, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("Send the file's bytes as the request body.".into());
    };
    let name = request.headers().get(NAME_HEADER).and_then(|v| v.to_str().ok()).map(decode_name).unwrap_or_default();
    stage(&app_root(&app)?, &name, bytes)
}

/// Turns the ids from `stage_attachment` into files ready to store with a message.
pub fn staged(app: &AppHandle, tokens: Vec<String>) -> Result<Staged, String> {
    if tokens.is_empty() {
        return Ok(Staged::default());
    }
    Staged::new(app_root(app)?, tokens)
}

#[tauri::command]
pub fn open_attachment(app: AppHandle, state: State<AppState>, id: i64) -> Result<(), String> {
    let attachment = find(&state.db, id)?.ok_or("That attachment no longer exists.")?;
    if !Path::new(&attachment.path).exists() {
        return Err(format!("{} is no longer on disk.", attachment.name));
    }
    app.opener().open_path(&attachment.path, None::<&str>).map_err(|e| e.to_string())
}

/// Answers `attachment://localhost/<id>` with that attachment's bytes, for thumbnails.
pub fn protocol(app: &AppHandle, request: tauri::http::Request<Vec<u8>>, responder: tauri::UriSchemeResponder) {
    let id = request.uri().path().trim_start_matches('/').parse::<i64>().ok();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::http::{header, Response, StatusCode};
        let file = id
            .and_then(|id| find(&app.state::<AppState>().db, id).ok().flatten())
            .and_then(|a| std::fs::read(&a.path).ok());
        let response = match file {
            Some(bytes) => Response::builder()
                .header(header::CONTENT_TYPE, image_type(&bytes).unwrap_or("application/octet-stream"))
                .body(bytes),
            None => Response::builder().status(StatusCode::NOT_FOUND).body(Vec::new()),
        };
        if let Ok(response) = response {
            responder.respond(response);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{now_ms, Access, Agent, Avatar, Provider};

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("grimoire-attach-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&Agent {
            id: "a".into(),
            name: "Ada".into(),
            provider: Provider::Claude,
            model: None,
            workspace: "/tmp".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: None,
            created_at: now_ms(),
        })
        .unwrap();
        db
    }

    #[test]
    fn tells_images_from_other_files() {
        assert_eq!(image_type(PNG), Some("image/png"));
        assert_eq!(image_type(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(image_type(b"GIF89a"), Some("image/gif"));
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_type(b"%PDF-1.7"), None);
        assert_eq!(image_type(b""), None);
    }

    #[test]
    fn keeps_names_inside_their_folder() {
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("C:\\x\\report.pdf"), "report.pdf");
        assert_eq!(clean_name(".env"), "env");
        assert_eq!(clean_name("a\nb.txt"), "ab.txt");
        assert_eq!(clean_name(".."), "file");
        assert_eq!(clean_name(""), "file");
        let mut taken = HashSet::new();
        assert_eq!(unique_name(&mut taken, "image.png"), "image.png");
        assert_eq!(unique_name(&mut taken, "Image.png"), "Image-2.png");
        assert_eq!(unique_name(&mut taken, "image.png"), "image-3.png");
        assert_eq!(unique_name(&mut taken, "notes"), "notes");
        assert_eq!(unique_name(&mut taken, "notes"), "notes-2");
        assert_eq!(decode_name("Screen%20Shot%20%E2%9C%93.png"), "Screen Shot \u{2713}.png");
        assert_eq!(decode_name("100%.txt"), "100%.txt");
    }

    #[test]
    fn stores_staged_files_with_their_message() {
        let db = setup();
        let root = temp_dir();
        let image = stage(&root, "image.png", PNG).unwrap();
        let shot = stage(&root, "image.png", PNG).unwrap();
        let doc = stage(&root, "../notes.txt", b"hello").unwrap();
        assert_eq!((image.kind, doc.kind, doc.name.as_str(), doc.size), (Kind::Image, Kind::File, "notes.txt", 5));

        let message = db.insert_message("a", "user", "look", "done").unwrap();
        let staged = Staged::new(root.clone(), vec![image.token, shot.token, doc.token]).unwrap();
        let stored = staged.store(&db, message.id).unwrap();
        let names: Vec<&str> = stored.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["image.png", "image-2.png", "notes.txt"]);
        let folder = root.join(message.id.to_string());
        assert_eq!(stored[2].path, folder.join("notes.txt").to_string_lossy());
        assert_eq!(std::fs::read(folder.join("notes.txt")).unwrap(), b"hello");
        assert!(std::fs::read_dir(root.join(STAGING)).unwrap().next().is_none(), "staging should be empty");

        let mut listed = db.list_messages("a", None, 10).unwrap();
        attach(&db, &mut listed).unwrap();
        assert_eq!(listed[0].attachments, stored);
        assert_eq!(find(&db, stored[0].id).unwrap().unwrap().kind, Kind::Image);

        remove_for_agent(&db, &root, "a");
        assert!(!folder.exists());
        db.delete_agent("a").unwrap();
        assert!(find(&db, stored[0].id).unwrap().is_none(), "rows go with the message");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_big_files_bad_tokens_and_too_many_files() {
        let root = temp_dir();
        let mut big = PNG.to_vec();
        big.resize(MAX_FILE_BYTES as usize + 1, 0);
        assert!(stage(&root, "big.png", &big).unwrap_err().contains("20 MB"));
        let video = vec![0u8; MAX_FILE_BYTES as usize + 1];
        assert_eq!(stage(&root, "video.mov", &video).unwrap_err(), "video.mov is over 20 MB. Attachments can be up to 20 MB.");
        assert!(stage(&root, "notes.pdf", &vec![0u8; MAX_FILE_BYTES as usize]).is_ok(), "exactly 20 MB is fine");
        assert!(Staged::new(root.clone(), vec!["../../x".into()]).is_err());
        let many = (0..=MAX_FILES).map(|_| uuid::Uuid::new_v4().to_string()).collect();
        assert!(Staged::new(root.clone(), many).is_err());

        let db = setup();
        let message = db.insert_message("a", "user", "x", "done").unwrap();
        let gone = Staged::new(root.clone(), vec![uuid::Uuid::new_v4().to_string()]).unwrap();
        assert!(gone.store(&db, message.id).is_err());
        assert!(!root.join(message.id.to_string()).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sends_images_as_images_and_copies_other_files() {
        let workspace = temp_dir();
        let store = temp_dir();
        std::fs::write(store.join("report.pdf"), b"%PDF").unwrap();
        let attachment = |id, name: &str, kind| Attachment {
            id,
            message_id: 7,
            name: name.into(),
            kind,
            size: 4,
            path: store.join(name).to_string_lossy().into_owned(),
        };
        let input = turn_input(&workspace, "Summarize these.", &[attachment(1, "shot.png", Kind::Image), attachment(2, "report.pdf", Kind::File)]).unwrap();
        let copy = workspace.join("attachments/7/report.pdf");
        assert_eq!(input.images, [store.join("shot.png")]);
        assert_eq!(input.text, format!("Summarize these.\n\nAttached files, copied into your workspace:\n- {}", copy.display()));
        assert_eq!(std::fs::read(&copy).unwrap(), b"%PDF");

        let only = turn_input(&workspace, "", &[attachment(2, "report.pdf", Kind::File)]).unwrap();
        assert!(only.text.starts_with("Attached files"));
        assert_eq!(turn_input(&workspace, "hi", &[]).unwrap(), TurnInput { text: "hi".into(), images: vec![] });
        std::fs::remove_dir_all(workspace).unwrap();
        std::fs::remove_dir_all(store).unwrap();
    }
}
