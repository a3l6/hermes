use crate::Result;
use crate::config::{Key, hex};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders (
    name        TEXT PRIMARY KEY,
    role        TEXT NOT NULL DEFAULT '',
    uidvalidity INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS messages (
    folder     TEXT NOT NULL,
    uid        INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    sender     TEXT NOT NULL,
    recipients TEXT NOT NULL,
    cc         TEXT NOT NULL,
    subject    TEXT NOT NULL,
    date       INTEGER NOT NULL,
    seen       INTEGER NOT NULL,
    flagged    INTEGER NOT NULL,
    answered   INTEGER NOT NULL,
    raw        BLOB,
    PRIMARY KEY (folder, uid)
);
CREATE TABLE IF NOT EXISTS settings (
    name  TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Flag {
    Seen,
    Flagged,
    Answered,
}

impl Flag {
    pub fn imap(self) -> &'static str {
        match self {
            Flag::Seen => "\\Seen",
            Flag::Flagged => "\\Flagged",
            Flag::Answered => "\\Answered",
        }
    }

    fn column(self) -> &'static str {
        match self {
            Flag::Seen => "seen",
            Flag::Flagged => "flagged",
            Flag::Answered => "answered",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Folder {
    pub name: String,
    /// Special use: "trash", "sent", "drafts", "archive", "all", "junk" or "".
    pub role: String,
    pub unread: u32,
    pub total: u32,
}

/// Cached headers and flags of one message; the body lives in `raw`.
#[derive(Debug, Clone, Default)]
pub struct Message {
    pub uid: u32,
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    /// Unix timestamp.
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) the SQLCipher database encrypted with `key`.
    /// Pages are decrypted in memory as they are read.
    pub fn open(path: &Path, key: &Key) -> Result<Store> {
        let conn = Connection::open(path)?;
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex(key)))?;
        // The TUI and its sync worker each hold a connection to this file.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn setting(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, name: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (name, value) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET value = excluded.value",
            [name, value],
        )?;
        Ok(())
    }

    /// Runs `f` inside one transaction.
    pub fn batch<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN")?;
        let result = f();
        self.conn
            .execute_batch(if result.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
        result
    }

    /// Replaces the folder list with `folders` (name, role), dropping the
    /// cache of any folder that no longer exists.
    pub fn set_folders(&self, folders: &[(String, String)]) -> Result<()> {
        self.batch(|| {
            for existing in self.folders()? {
                if !folders.iter().any(|(name, _)| *name == existing.name) {
                    self.conn
                        .execute("DELETE FROM messages WHERE folder = ?1", [&existing.name])?;
                    self.conn
                        .execute("DELETE FROM folders WHERE name = ?1", [&existing.name])?;
                }
            }
            for (name, role) in folders {
                self.conn.execute(
                    "INSERT INTO folders (name, role) VALUES (?1, ?2)
                     ON CONFLICT(name) DO UPDATE SET role = excluded.role",
                    [name, role],
                )?;
            }
            Ok(())
        })
    }

    /// INBOX first, then alphabetical.
    pub fn folders(&self) -> Result<Vec<Folder>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, role,
                    (SELECT COUNT(*) FROM messages m WHERE m.folder = f.name AND m.seen = 0),
                    (SELECT COUNT(*) FROM messages m WHERE m.folder = f.name)
             FROM folders f ORDER BY name != 'INBOX', name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Folder {
                name: r.get(0)?,
                role: r.get(1)?,
                unread: r.get(2)?,
                total: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn folder_with_role(&self, role: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT name FROM folders WHERE role = ?1", [role], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Records the folder's UIDVALIDITY; if it changed, the cached UIDs are
    /// meaningless and the folder's messages are dropped.
    pub fn reset_folder(&self, folder: &str, uidvalidity: u32) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE folders SET uidvalidity = ?2 WHERE name = ?1 AND uidvalidity != ?2",
            params![folder, uidvalidity],
        )?;
        if changed > 0 {
            self.conn
                .execute("DELETE FROM messages WHERE folder = ?1", [folder])?;
        }
        Ok(())
    }

    /// Cached UIDs of a folder, ascending.
    pub fn uids(&self, folder: &str) -> Result<Vec<u32>> {
        let mut stmt = self
            .conn
            .prepare("SELECT uid FROM messages WHERE folder = ?1 ORDER BY uid")?;
        let rows = stmt.query_map([folder], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Up to `limit` UIDs whose body has not been downloaded, newest first.
    pub fn missing_bodies(&self, folder: &str, limit: u32) -> Result<Vec<u32>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid FROM messages WHERE folder = ?1 AND raw IS NULL
             ORDER BY uid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![folder, limit], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Inserts a message, or refreshes its headers and flags (keeping any
    /// downloaded body).
    pub fn upsert_message(&self, folder: &str, m: &Message) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages (folder, uid, message_id, sender, recipients, cc, subject,
                                   date, seen, flagged, answered)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(folder, uid) DO UPDATE SET
                message_id = excluded.message_id, sender = excluded.sender,
                recipients = excluded.recipients, cc = excluded.cc,
                subject = excluded.subject, date = excluded.date, seen = excluded.seen,
                flagged = excluded.flagged, answered = excluded.answered",
            params![
                folder,
                m.uid,
                m.message_id,
                m.from,
                m.to,
                m.cc,
                m.subject,
                m.date,
                m.seen,
                m.flagged,
                m.answered
            ],
        )?;
        Ok(())
    }

    /// Newest first.
    pub fn messages(&self, folder: &str) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid, message_id, sender, recipients, cc, subject, date, seen, flagged,
                    answered
             FROM messages WHERE folder = ?1 ORDER BY date DESC, uid DESC",
        )?;
        let rows = stmt.query_map([folder], |r| {
            Ok(Message {
                uid: r.get(0)?,
                message_id: r.get(1)?,
                from: r.get(2)?,
                to: r.get(3)?,
                cc: r.get(4)?,
                subject: r.get(5)?,
                date: r.get(6)?,
                seen: r.get(7)?,
                flagged: r.get(8)?,
                answered: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_flags(
        &self,
        folder: &str,
        uid: u32,
        seen: bool,
        flagged: bool,
        answered: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET seen = ?3, flagged = ?4, answered = ?5
             WHERE folder = ?1 AND uid = ?2",
            params![folder, uid, seen, flagged, answered],
        )?;
        Ok(())
    }

    pub fn set_flag(&self, folder: &str, uid: u32, flag: Flag, on: bool) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE messages SET {} = ?3 WHERE folder = ?1 AND uid = ?2",
                flag.column()
            ),
            params![folder, uid, on],
        )?;
        Ok(())
    }

    pub fn delete_message(&self, folder: &str, uid: u32) -> Result<()> {
        self.conn.execute(
            "DELETE FROM messages WHERE folder = ?1 AND uid = ?2",
            params![folder, uid],
        )?;
        Ok(())
    }

    /// Stores the full RFC 5322 message.
    pub fn set_raw(&self, folder: &str, uid: u32, raw: &[u8]) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET raw = ?3 WHERE folder = ?1 AND uid = ?2",
            params![folder, uid, raw],
        )?;
        Ok(())
    }

    /// Date and full text of every downloaded message in a folder.
    pub fn bodies(&self, folder: &str) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT date, raw FROM messages WHERE folder = ?1 AND raw IS NOT NULL")?;
        let rows = stmt.query_map([folder], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The full message, if it has been downloaded.
    pub fn raw(&self, folder: &str, uid: u32) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT raw FROM messages WHERE folder = ?1 AND uid = ?2",
                params![folder, uid],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let store = Store::open(Path::new(":memory:"), &[7; 32]).unwrap();
        store
            .set_folders(&[
                ("Trash".to_string(), "trash".to_string()),
                ("INBOX".to_string(), String::new()),
            ])
            .unwrap();
        store
    }

    fn message(uid: u32, date: i64) -> Message {
        Message {
            uid,
            subject: format!("subject {uid}"),
            date,
            ..Default::default()
        }
    }

    #[test]
    fn folders_list_inbox_first_with_counts() {
        let store = store();
        store.upsert_message("INBOX", &message(1, 10)).unwrap();
        store.upsert_message("INBOX", &message(2, 20)).unwrap();
        store.set_flag("INBOX", 1, Flag::Seen, true).unwrap();

        let folders = store.folders().unwrap();
        assert_eq!(folders[0].name, "INBOX");
        assert_eq!((folders[0].unread, folders[0].total), (1, 2));
        assert_eq!(
            store.folder_with_role("trash").unwrap().as_deref(),
            Some("Trash")
        );
    }

    #[test]
    fn upsert_keeps_downloaded_body() {
        let store = store();
        store.upsert_message("INBOX", &message(1, 10)).unwrap();
        assert_eq!(store.raw("INBOX", 1).unwrap(), None);
        store.set_raw("INBOX", 1, b"raw").unwrap();
        store.upsert_message("INBOX", &message(1, 10)).unwrap();
        assert_eq!(store.raw("INBOX", 1).unwrap().as_deref(), Some(&b"raw"[..]));
    }

    #[test]
    fn database_file_is_encrypted_and_needs_the_key() {
        let path = std::env::temp_dir().join(format!("hermes-test-{}.db", std::process::id()));
        std::fs::remove_file(&path).ok();
        {
            let store = Store::open(&path, &[7; 32]).unwrap();
            store
                .set_folders(&[("INBOX".to_string(), String::new())])
                .unwrap();
            let mut m = message(1, 10);
            m.subject = "very secret subject".to_string();
            store.upsert_message("INBOX", &m).unwrap();
            store.set_raw("INBOX", 1, b"very secret body").unwrap();
            store
                .set_setting("imap_password", "very secret password")
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let on_disk = String::from_utf8_lossy(&bytes);
        assert!(!on_disk.contains("very secret"));
        assert!(!on_disk.contains("SQLite format"));
        assert!(Store::open(&path, &[8; 32]).is_err());
        let store = Store::open(&path, &[7; 32]).unwrap();
        assert_eq!(
            store.setting("imap_password").unwrap().as_deref(),
            Some("very secret password")
        );
        assert_eq!(
            store.missing_bodies("INBOX", 10).unwrap(),
            Vec::<u32>::new()
        );
        for suffix in ["", "-wal", "-shm"] {
            std::fs::remove_file(format!("{}{suffix}", path.display())).ok();
        }
    }

    #[test]
    fn messages_are_newest_first() {
        let store = store();
        store.upsert_message("INBOX", &message(1, 30)).unwrap();
        store.upsert_message("INBOX", &message(2, 10)).unwrap();
        store.upsert_message("INBOX", &message(3, 20)).unwrap();
        let uids: Vec<u32> = store
            .messages("INBOX")
            .unwrap()
            .iter()
            .map(|m| m.uid)
            .collect();
        assert_eq!(uids, [1, 3, 2]);
        assert_eq!(store.uids("INBOX").unwrap(), [1, 2, 3]);
    }

    #[test]
    fn uidvalidity_change_drops_cache() {
        let store = store();
        store.reset_folder("INBOX", 7).unwrap();
        store.upsert_message("INBOX", &message(1, 10)).unwrap();
        store.reset_folder("INBOX", 7).unwrap();
        assert_eq!(store.uids("INBOX").unwrap(), [1]);
        store.reset_folder("INBOX", 8).unwrap();
        assert!(store.uids("INBOX").unwrap().is_empty());
    }

    #[test]
    fn removed_folder_drops_its_messages() {
        let store = store();
        store.upsert_message("Trash", &message(1, 10)).unwrap();
        store
            .set_folders(&[("INBOX".to_string(), String::new())])
            .unwrap();
        assert_eq!(store.folders().unwrap().len(), 1);
        assert!(store.uids("Trash").unwrap().is_empty());
    }
}
