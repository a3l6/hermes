use crate::Result;
use crate::config::Config;
use crate::email_tools::format_addresses;
use crate::store::{Flag, Message, Store};
use imap::types::{Fetch, NameAttribute};
use mail_parser::MessageParser;
use native_tls::{TlsConnector, TlsStream};
use std::collections::HashSet;
use std::net::TcpStream;
use std::time::Duration;

const HEADERS: &str = "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER])";

/// A logged-in IMAP session. Every operation mirrors its effect into the
/// local store, and addresses messages by UID.
pub struct Remote {
    session: imap::Session<TlsStream<TcpStream>>,
    has_move: bool,
}

impl Remote {
    pub fn connect(config: &Config) -> Result<Remote> {
        let tcp = TcpStream::connect((config.imap_host.as_str(), config.imap_port))?;
        // A dead connection must fail instead of hanging the sync worker.
        tcp.set_read_timeout(Some(Duration::from_secs(60)))?;
        let tls = TlsConnector::builder()
            .danger_accept_invalid_certs(config.insecure_tls)
            .build()?;
        let mut client = imap::Client::new(tls.connect(&config.imap_host, tcp)?);
        client.read_greeting()?;
        let mut session = client
            .login(&config.username, &config.password)
            .map_err(|e| e.0)?;
        let has_move = session.capabilities()?.has_str("MOVE");
        Ok(Remote { session, has_move })
    }

    /// Refreshes the store's folder list from the server.
    pub fn list_folders(&mut self, store: &Store) -> Result<()> {
        let names = self.session.list(Some(""), Some("*"))?;
        let folders: Vec<(String, String)> = names
            .iter()
            .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
            .map(|n| (n.name().to_string(), role(n.name(), n.attributes())))
            .collect();
        store.set_folders(&folders)
    }

    /// Brings the store's view of `folder` in line with the server: drops
    /// expunged messages and refreshes flags. Returns the UIDs (ascending)
    /// the store does not have yet, for `fetch_headers`.
    pub fn sync(&mut self, store: &Store, folder: &str) -> Result<Vec<u32>> {
        while self.session.unsolicited_responses.try_recv().is_ok() {}
        let mailbox = self.session.select(folder)?;
        store.reset_folder(folder, mailbox.uid_validity.unwrap_or(0))?;
        let local = store.uids(folder)?;
        let remote = if mailbox.exists == 0 {
            HashSet::new()
        } else {
            self.session.uid_search("ALL")?
        };

        let flags = if local.iter().any(|uid| remote.contains(uid)) {
            Some(self.session.uid_fetch("1:*", "FLAGS")?)
        } else {
            None
        };
        store.batch(|| {
            for uid in local.iter().filter(|uid| !remote.contains(uid)) {
                store.delete_message(folder, *uid)?;
            }
            for fetch in flags.iter().flat_map(|f| f.iter()) {
                if let Some(uid) = fetch.uid {
                    let (seen, flagged, answered) = flags_of(fetch);
                    store.set_flags(folder, uid, seen, flagged, answered)?;
                }
            }
            Ok(())
        })?;

        let local: HashSet<u32> = local.into_iter().collect();
        let mut missing: Vec<u32> = remote.into_iter().filter(|u| !local.contains(u)).collect();
        missing.sort_unstable();
        Ok(missing)
    }

    /// Downloads and stores the headers of `uids` (ascending).
    pub fn fetch_headers(&mut self, store: &Store, folder: &str, uids: &[u32]) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.session.select(folder)?;
        let fetches = self.session.uid_fetch(uid_set(uids), HEADERS)?;
        store.batch(|| {
            for fetch in fetches.iter() {
                if let (Some(uid), Some(header)) = (fetch.uid, fetch.header()) {
                    let mut message = parse_headers(uid, header);
                    if message.date == 0 {
                        message.date = fetch.internal_date().map_or(0, |d| d.timestamp());
                    }
                    (message.seen, message.flagged, message.answered) = flags_of(fetch);
                    store.upsert_message(folder, &message)?;
                }
            }
            Ok(())
        })
    }

    /// Downloads and stores the full messages `uids` (ascending). A UID
    /// the server no longer has is dropped from the store.
    pub fn fetch_bodies(&mut self, store: &Store, folder: &str, uids: &[u32]) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.session.select(folder)?;
        let fetches = self.session.uid_fetch(uid_set(uids), "(UID BODY.PEEK[])")?;
        let mut gone: HashSet<u32> = uids.iter().copied().collect();
        store.batch(|| {
            for fetch in fetches.iter() {
                if let (Some(uid), Some(body)) = (fetch.uid, fetch.body()) {
                    store.set_raw(folder, uid, body)?;
                    gone.remove(&uid);
                }
            }
            for uid in &gone {
                store.delete_message(folder, *uid)?;
            }
            Ok(())
        })
    }

    pub fn set_flag(
        &mut self,
        store: &Store,
        folder: &str,
        uid: u32,
        flag: Flag,
        on: bool,
    ) -> Result<()> {
        self.session.select(folder)?;
        let sign = if on { '+' } else { '-' };
        self.session
            .uid_store(uid.to_string(), format!("{sign}FLAGS ({})", flag.imap()))?;
        store.set_flag(folder, uid, flag, on)
    }

    pub fn move_to(&mut self, store: &Store, folder: &str, uid: u32, dest: &str) -> Result<()> {
        self.session.select(folder)?;
        if self.has_move {
            self.session.uid_mv(uid.to_string(), dest)?;
        } else {
            self.session.uid_copy(uid.to_string(), dest)?;
            self.expunge(uid)?;
        }
        store.delete_message(folder, uid)
    }

    pub fn copy_to(&mut self, folder: &str, uid: u32, dest: &str) -> Result<()> {
        self.session.select(folder)?;
        self.session.uid_copy(uid.to_string(), dest)?;
        Ok(())
    }

    /// Undoes a move: finds the message with `message_id` in `folder` and
    /// moves it to `dest`.
    pub fn move_back(
        &mut self,
        store: &Store,
        folder: &str,
        message_id: &str,
        dest: &str,
    ) -> Result<()> {
        self.session.select(folder)?;
        let query = format!("HEADER Message-ID {}", quote(&format!("<{message_id}>")));
        let found = self.session.uid_search(query)?;
        let uid = found
            .into_iter()
            .max()
            .ok_or(format!("cannot undo: the message is no longer in {folder}"))?;
        self.move_to(store, folder, uid, dest)
    }

    /// Moves the message to the trash folder; deletes it for good when it
    /// is already there or the server has no trash folder.
    pub fn delete(&mut self, store: &Store, folder: &str, uid: u32) -> Result<()> {
        match store.folder_with_role("trash")? {
            Some(trash) if trash != folder => self.move_to(store, folder, uid, &trash),
            _ => {
                self.session.select(folder)?;
                self.expunge(uid)?;
                store.delete_message(folder, uid)
            }
        }
    }

    pub fn archive(&mut self, store: &Store, folder: &str, uid: u32) -> Result<()> {
        let archive = match store.folder_with_role("archive")? {
            Some(archive) => archive,
            None => store
                .folder_with_role("all")?
                .ok_or("this account has no archive folder")?,
        };
        self.move_to(store, folder, uid, &archive)
    }

    /// Server-side full-text search; returns matching UIDs.
    pub fn search(&mut self, folder: &str, query: &str) -> Result<Vec<u32>> {
        self.session.select(folder)?;
        let found = self.session.uid_search(format!("TEXT {}", quote(query)))?;
        Ok(found.into_iter().collect())
    }

    pub fn create_folder(&mut self, store: &Store, name: &str) -> Result<()> {
        self.session.create(name)?;
        self.list_folders(store)
    }

    pub fn delete_folder(&mut self, store: &Store, name: &str) -> Result<()> {
        // A selected mailbox cannot be deleted on some servers.
        self.session.select("INBOX")?;
        self.session.delete(name)?;
        self.list_folders(store)
    }

    pub fn rename_folder(&mut self, store: &Store, from: &str, to: &str) -> Result<()> {
        self.session.select("INBOX")?;
        self.session.rename(from, to)?;
        self.list_folders(store)
    }

    fn expunge(&mut self, uid: u32) -> Result<()> {
        self.session
            .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")?;
        self.session.expunge()?;
        Ok(())
    }
}

/// An IMAP quoted string.
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn flags_of(fetch: &Fetch) -> (bool, bool, bool) {
    let flags = fetch.flags();
    (
        flags.contains(&imap::types::Flag::Seen),
        flags.contains(&imap::types::Flag::Flagged),
        flags.contains(&imap::types::Flag::Answered),
    )
}

/// Special use of a folder, from its LIST attributes (RFC 6154) or, for
/// servers that do not send them, its name.
fn role(name: &str, attributes: &[NameAttribute]) -> String {
    for attribute in attributes {
        if let NameAttribute::Custom(custom) = attribute {
            match custom.to_ascii_lowercase().as_str() {
                "\\trash" => return "trash".to_string(),
                "\\sent" => return "sent".to_string(),
                "\\drafts" => return "drafts".to_string(),
                "\\all" => return "all".to_string(),
                "\\archive" => return "archive".to_string(),
                "\\junk" => return "junk".to_string(),
                _ => {}
            }
        }
    }
    match name.to_ascii_lowercase().as_str() {
        "trash" | "deleted items" | "deleted messages" => "trash",
        "archive" => "archive",
        "sent" | "sent items" | "sent messages" => "sent",
        "drafts" | "draft" => "drafts",
        _ => "",
    }
    .to_string()
}

fn parse_headers(uid: u32, header: &[u8]) -> Message {
    let Some(parsed) = MessageParser::default().parse(header) else {
        return Message {
            uid,
            ..Default::default()
        };
    };
    Message {
        uid,
        message_id: parsed.message_id().unwrap_or_default().to_string(),
        from: format_addresses(parsed.from()),
        to: format_addresses(parsed.to()),
        cc: format_addresses(parsed.cc()),
        subject: parsed.subject().unwrap_or_default().to_string(),
        date: parsed.date().map_or(0, |d| d.to_timestamp()),
        ..Default::default()
    }
}

/// IMAP sequence set for ascending `uids`, with runs collapsed: 1:3,7.
fn uid_set(uids: &[u32]) -> String {
    let mut set = String::new();
    let mut i = 0;
    while i < uids.len() {
        let start = i;
        while i + 1 < uids.len() && uids[i + 1] == uids[i] + 1 {
            i += 1;
        }
        if !set.is_empty() {
            set.push(',');
        }
        set.push_str(&uids[start].to_string());
        if i > start {
            set.push_str(&format!(":{}", uids[i]));
        }
        i += 1;
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_set_collapses_runs() {
        assert_eq!(uid_set(&[4]), "4");
        assert_eq!(uid_set(&[1, 2, 3, 7, 9, 10]), "1:3,7,9:10");
    }

    #[test]
    fn headers_are_decoded() {
        let message = parse_headers(
            5,
            b"From: =?UTF-8?Q?Zo=C3=AB?= <zoe@test.com>\r\nTo: a@test.com, B <b@test.com>\r\n\
              Subject: =?UTF-8?B?w6dh?=\r\nMessage-ID: <id@test.com>\r\n\
              Date: Thu, 01 Jan 2026 00:00:10 +0000\r\n\r\n",
        );
        assert_eq!(message.uid, 5);
        assert_eq!(message.from, "Zo\u{eb} <zoe@test.com>");
        assert_eq!(message.to, "a@test.com, B <b@test.com>");
        assert_eq!(message.subject, "\u{e7}a");
        assert_eq!(message.message_id, "id@test.com");
        assert_eq!(message.date, 1767225610);
    }

    #[test]
    fn role_prefers_attributes_over_names() {
        let trash = [NameAttribute::Custom("\\Trash".into())];
        assert_eq!(role("[Gmail]/Bin", &trash), "trash");
        assert_eq!(role("Deleted Items", &[]), "trash");
        assert_eq!(role("Work", &[]), "");
    }
}
