use crate::Result;
use crate::config::{Config, Key};
use crate::email_tools::remote::Remote;
use crate::email_tools::{
    Email, body_and_attachments, draft_template, format_date, parse_draft, reply_address,
    reply_draft, send_email,
};
use crate::store::{Flag, Folder, Message, Store};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

/// Work the UI asks the worker thread to do on the server.
pub enum Cmd {
    /// Refresh the folder list and resync every folder.
    SyncAll,
    /// Sync one folder now and give it priority in background downloads.
    Open(String),
    FetchBody(String, u32),
    // These act on any number of messages (folder, uids) in one request.
    SetFlag(String, Vec<u32>, Flag, bool),
    Delete(String, Vec<u32>),
    Archive(String, Vec<u32>),
    Move(String, Vec<u32>, String),
    /// Copy a message (folder, uid) into another folder.
    Copy(String, Vec<u32>, String),
    /// Undo a move: find the message by Message-ID in the first folder
    /// and move it to the second.
    MoveBack(String, Vec<String>, String),
    Search(String, String),
    CreateFolder(String),
    DeleteFolder(String),
    RenameFolder(String, String),
    /// Send a message; the second field is the message it replies to.
    Send(Email, Option<(String, u32)>),
}

/// What the worker reports back. The UI answers by re-reading the store.
enum Update {
    /// Folders or message lists changed.
    Changed,
    /// Only message bodies were downloaded.
    Bodies,
    Status(String),
    Error(String),
    Search(Vec<u32>),
}

pub fn run(terminal: &mut DefaultTerminal, config: Config, key: Key, store: Store) -> Result<()> {
    let (commands, worker_commands) = mpsc::channel();
    let (updates, ui_updates) = mpsc::channel();
    let user = config.username.clone();
    // The worker gets its own connection to the database.
    let worker = Worker {
        store: Store::open(&config.db_path()?, &key)?,
        config,
        remote: None,
        updates,
        current: "INBOX".to_string(),
        to_sync: VecDeque::new(),
        headers: Vec::new(),
    };
    std::thread::spawn(move || worker.run(worker_commands));

    let mut app = App {
        store,
        commands,
        user,
        folders: Vec::new(),
        folder_sel: 0,
        folder: "INBOX".to_string(),
        messages: Vec::new(),
        sel: 0,
        filter: None,
        drafts: HashMap::new(),
        focus: Focus::List,
        view: None,
        mode: Mode::Normal,
        count: None,
        prefix: None,
        confirm: None,
        moving: Vec::new(),
        offset: 0,
        search: String::new(),
        search_back: false,
        highlight: false,
        range: None,
        visual: (0, 0),
        undo: Vec::new(),
        yanked: None,
        alternate: None,
        help: false,
        page: 1,
        status: "Connecting... (:help for keys, :q to quit)".to_string(),
        error: false,
    };
    app.reload()?;
    app.run(terminal, ui_updates)
}

/// Owns the IMAP connection so the UI never waits on the network. Between
/// commands it mirrors the whole account: folder state first, then
/// headers, then bodies, one small batch at a time.
struct Worker {
    config: Config,
    store: Store,
    remote: Option<Remote>,
    updates: Sender<Update>,
    current: String,
    /// Folders waiting for a sync.
    to_sync: VecDeque<String>,
    /// Per folder, UIDs (ascending) whose headers are not stored yet.
    headers: Vec<(String, Vec<u32>)>,
}

impl Worker {
    fn run(mut self, commands: Receiver<Cmd>) {
        self.handle(Cmd::SyncAll);
        let mut busy = true;
        loop {
            let cmd = match commands.try_recv() {
                Ok(cmd) => cmd,
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {
                    match self.background() {
                        Ok(Some(update)) => {
                            busy = true;
                            self.send(update);
                            continue;
                        }
                        Ok(None) => {
                            if busy && self.remote.is_some() {
                                self.send(Update::Status("Up to date".to_string()));
                            }
                            busy = false;
                        }
                        Err(e) => {
                            // Start over from a fresh connection.
                            self.remote = None;
                            self.headers.clear();
                            self.to_sync = self.folder_names().into();
                            self.send(Update::Error(e.to_string()));
                        }
                    }
                    // Idle: wait for the UI, and poll for new mail meanwhile.
                    match commands.recv_timeout(Duration::from_secs(60)) {
                        Ok(cmd) => cmd,
                        Err(RecvTimeoutError::Timeout) => Cmd::Open(self.current.clone()),
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            };
            self.handle(cmd);
        }
    }

    fn send(&self, update: Update) {
        let _ = self.updates.send(update);
    }

    fn folder_names(&self) -> Vec<String> {
        let folders = self.store.folders().unwrap_or_default();
        folders.into_iter().map(|f| f.name).collect()
    }

    fn handle(&mut self, cmd: Cmd) {
        let mut result = self.execute(&cmd);
        // The server may have dropped an idle connection: reconnect once.
        // A send is never repeated.
        if result.is_err() && self.remote.is_some() && !matches!(cmd, Cmd::Send(..)) {
            self.remote = None;
            result = self.execute(&cmd);
        }
        match result {
            Ok(update) => {
                self.send(Update::Changed);
                if let Some(update) = update {
                    self.send(update);
                }
            }
            Err(e) => self.send(Update::Error(e.to_string())),
        }
    }

    fn execute(&mut self, cmd: &Cmd) -> Result<Option<Update>> {
        if let Cmd::Send(email, _) = cmd {
            send_email(email, &self.config)?;
        }
        if self.remote.is_none() {
            let mut remote = Remote::connect(&self.config)?;
            remote.list_folders(&self.store)?;
            self.remote = Some(remote);
        }
        let remote = self.remote.as_mut().ok_or("not connected")?;
        let store = &self.store;
        match cmd {
            Cmd::SyncAll => {
                remote.list_folders(store)?;
                let mut folders = vec![self.current.clone()];
                folders.extend(
                    self.folder_names()
                        .into_iter()
                        .filter(|f| *f != self.current),
                );
                self.to_sync = folders.into();
            }
            Cmd::Open(folder) => {
                self.current = folder.clone();
                let missing = remote.sync(store, folder)?;
                self.headers.retain(|(f, _)| f != folder);
                if !missing.is_empty() {
                    self.headers.push((folder.clone(), missing));
                }
            }
            Cmd::FetchBody(folder, uid) => remote.fetch_bodies(store, folder, &[*uid])?,
            Cmd::SetFlag(folder, uids, flag, on) => {
                remote.set_flag(store, folder, uids, *flag, *on)?
            }
            Cmd::Delete(folder, uids) => remote.delete(store, folder, uids)?,
            Cmd::Archive(folder, uids) => remote.archive(store, folder, uids)?,
            Cmd::Move(folder, uids, dest) => remote.move_to(store, folder, uids, dest)?,
            Cmd::Copy(folder, uids, dest) => {
                remote.copy_to(folder, uids, dest)?;
                self.to_sync.push_front(dest.clone());
            }
            Cmd::MoveBack(folder, message_ids, dest) => {
                remote.move_back(store, folder, message_ids, dest)?;
                self.to_sync.push_front(folder.clone());
                self.to_sync.push_front(dest.clone());
            }
            Cmd::Search(folder, query) => {
                return Ok(Some(Update::Search(remote.search(folder, query)?)));
            }
            Cmd::CreateFolder(name) => remote.create_folder(store, name)?,
            Cmd::DeleteFolder(name) => remote.delete_folder(store, name)?,
            Cmd::RenameFolder(from, to) => remote.rename_folder(store, from, to)?,
            Cmd::Send(_, replied) => {
                if let Some((folder, uid)) = replied {
                    // The mail is out; failing to mark the original is minor.
                    let _ = remote.set_flag(store, folder, &[*uid], Flag::Answered, true);
                }
                return Ok(Some(Update::Status("Message sent".to_string())));
            }
        }
        Ok(None)
    }

    /// Does one small piece of mirroring. `None` means nothing is left.
    fn background(&mut self) -> Result<Option<Update>> {
        let Some(remote) = self.remote.as_mut() else {
            return Ok(None);
        };
        let store = &self.store;
        let status = |text: String| {
            let _ = self.updates.send(Update::Status(text));
        };

        if let Some(folder) = self.to_sync.pop_front() {
            status(format!("Syncing {folder}"));
            let missing = remote.sync(store, &folder)?;
            self.headers.retain(|(f, _)| *f != folder);
            if !missing.is_empty() {
                self.headers.push((folder, missing));
            }
            return Ok(Some(Update::Changed));
        }

        if !self.headers.is_empty() {
            let current = self.headers.iter().position(|(f, _)| *f == self.current);
            let i = current.unwrap_or(0);
            let (folder, uids) = &mut self.headers[i];
            // Newest first.
            let chunk = uids.split_off(uids.len().saturating_sub(200));
            status(format!("Fetching {folder} ({} to go)", uids.len()));
            remote.fetch_headers(store, folder, &chunk)?;
            if uids.is_empty() {
                self.headers.remove(i);
            }
            return Ok(Some(Update::Changed));
        }

        let mut folders = vec![self.current.clone()];
        folders.extend(store.folders()?.into_iter().map(|f| f.name));
        for folder in folders {
            let mut uids = store.missing_bodies(&folder, 25)?;
            if uids.is_empty() {
                continue;
            }
            uids.sort_unstable();
            status(format!("Downloading {folder}"));
            remote.fetch_bodies(store, &folder, &uids)?;
            return Ok(Some(Update::Bodies));
        }
        Ok(None)
    }
}

const HELP: &str = "\
Hermes is modal and follows vim. Folders are buffers, messages are lines.

MOVING
  j k  5j           down / up, with counts
  gg G  5G  :5      first / last / row 5
  Ctrl-d Ctrl-u     half a page       Ctrl-f Ctrl-b   a page
  H M L             top / middle / bottom of the window
  zz zt zb          scroll the cursor row to middle / top / bottom
  h l  Ctrl-w h/l/w folders pane / messages pane
  l  Enter          open the folder or message under the cursor
  gt gT  :bn :bp    next / previous folder      Ctrl-o   previous folder
  /text  ?text      search this list (or the open message); n N repeat

EDITING
  dd  3dd  x        delete (to Trash); in the folders pane dd deletes the folder
  yy  p             yank messages, paste a copy into the open folder
  u                 undo the last delete, archive, move or flag change
  ~                 toggle read          s   toggle star
  a                 archive              m   move: pick a folder, Enter
  o  r              compose / reply in $EDITOR
  V                 visual mode: select rows, then d y a m ~ s or :
  R  :w             sync every folder

LEADER (Space)      press Space to see the list

COMMANDS            ranges work: :%d  :2,5m Work  :'<,'>star
  :q  :qa           close one layer (message, search results, hermes) / quit
  :e <folder>       open a folder (also :b; Tab completes folder names)
  :d  :y  :m <folder>  :archive   delete, yank, move, archive
  :read :unread :star :unstar
  :mkdir <name>  :rename <name>  :rmdir     folders
  :search <text>    full-text search on the server; :q clears the results
  :noh  :undo  :compose  :reply  :help

Press any key to close this help.";

/// The leader key, which starts the `<leader>` shortcuts.
const LEADER: char = ' ';

/// `<leader>s`, stored as the prefix of its own group of shortcuts.
const LEADER_S: char = '\u{1}';

/// `Ctrl-w`, stored as the prefix of a window command.
const CTRL_W: char = '\u{17}';

/// The keys that can follow `prefix`, with what each does. Shown in a
/// popup while the sequence is half typed; `on_normal_key` acts on them.
fn which_key(prefix: char) -> &'static [(&'static str, &'static str)] {
    match prefix {
        LEADER => &[
            ("f", "[F]older: open by name"),
            ("m", "[M]ove to folder by name"),
            ("s", "+[S]ync / search"),
            ("c", "[C]ompose"),
            ("r", "[R]eply"),
            ("u", "[U]ndo"),
            ("h", "[H]elp"),
        ],
        LEADER_S => &[
            ("m", "Sync [M]ail in every folder"),
            ("f", "[F]ind: search on the server"),
        ],
        'g' => &[
            ("g", "First row"),
            ("t", "Next folder"),
            ("T", "Previous folder"),
        ],
        'd' => &[("d", "Delete")],
        'y' => &[("y", "Yank")],
        'z' => &[
            ("z", "Cursor row to middle"),
            ("t", "Cursor row to top"),
            ("b", "Cursor row to bottom"),
        ],
        'Z' => &[("Z", "Quit one layer (:q)"), ("Q", "Quit hermes (:qa)")],
        CTRL_W => &[
            ("h", "Folders pane"),
            ("l", "Messages pane"),
            ("w", "Other pane"),
        ],
        _ => &[],
    }
}

#[derive(PartialEq)]
enum Focus {
    Folders,
    List,
}

enum Mode {
    Normal,
    /// Selecting a range of messages; holds the row where it started.
    Visual(usize),
    /// Typing after `:`.
    Command(String),
    /// Typing after `/`.
    Search(String),
}

/// The message being read.
struct View {
    uid: u32,
    /// `None` until the body has been downloaded.
    body: Option<String>,
    attachments: Vec<String>,
    scroll: usize,
}

struct App {
    store: Store,
    commands: Sender<Cmd>,
    user: String,
    folders: Vec<Folder>,
    folder_sel: usize,
    /// The folder whose messages are listed.
    folder: String,
    messages: Vec<Message>,
    sel: usize,
    /// UIDs matching the active search.
    filter: Option<Vec<u32>>,
    /// Reply drafts, by the Message-ID they answer: date and first line.
    drafts: HashMap<String, (i64, String)>,
    focus: Focus,
    view: Option<View>,
    mode: Mode,
    /// Count typed before a key, as in `5j`.
    count: Option<usize>,
    /// First key of a two-key sequence: `g`, `d`, `Z` or the leader.
    prefix: Option<char>,
    /// A yes/no question and the command a yes sends.
    confirm: Option<(String, Cmd)>,
    /// Messages waiting for a destination folder to be picked.
    moving: Vec<u32>,
    /// Top row of the message list window.
    offset: usize,
    /// The last `/` or `?` pattern, and whether it was `?`.
    search: String,
    search_back: bool,
    highlight: bool,
    /// Rows a `:` command applies to, while it runs.
    range: Option<(usize, usize)>,
    /// The last visual selection, for `'<,'>`.
    visual: (usize, usize),
    /// Per undoable action, the commands that reverse it.
    undo: Vec<Vec<Cmd>>,
    /// Yanked messages: their folder and UIDs.
    yanked: Option<(String, Vec<u32>)>,
    /// The previously open folder.
    alternate: Option<String>,
    help: bool,
    /// Rows visible in the message list, for page motions.
    page: usize,
    status: String,
    error: bool,
}

impl App {
    fn run(&mut self, terminal: &mut DefaultTerminal, updates: Receiver<Update>) -> Result<()> {
        loop {
            while let Ok(update) = updates.try_recv() {
                match update {
                    Update::Changed => self.reload()?,
                    Update::Bodies => {
                        self.load_drafts()?;
                        self.load_body()?;
                    }
                    Update::Status(text) => self.set_status(text, false),
                    Update::Error(text) => self.set_status(text, true),
                    Update::Search(uids) => {
                        self.set_status(format!("{} matches, Esc to clear", uids.len()), false);
                        self.filter = Some(uids);
                        self.sel = 0;
                        self.reload()?;
                    }
                }
            }
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(Duration::from_millis(200))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && self.on_key(key, terminal)?
            {
                return Ok(());
            }
        }
    }

    fn send(&mut self, cmd: Cmd) {
        // A new request makes the last error stale.
        if self.error {
            self.set_status(String::new(), false);
        }
        let _ = self.commands.send(cmd);
    }

    fn set_status(&mut self, text: String, error: bool) {
        self.status = text;
        self.error = error;
    }

    /// Re-reads folders and the message list from the store.
    fn reload(&mut self) -> Result<()> {
        self.folders = self.store.folders()?;
        self.folder_sel = self.folder_sel.min(self.folders.len().saturating_sub(1));
        if !self.folders.is_empty() && !self.folders.iter().any(|f| f.name == self.folder) {
            self.folder = "INBOX".to_string();
        }

        let selected = self.messages.get(self.sel).map(|m| m.uid);
        self.messages = self.store.messages(&self.folder)?;
        if let Some(filter) = &self.filter {
            self.messages.retain(|m| filter.contains(&m.uid));
        }
        // Stay on the same message when new mail arrives above it.
        if let Some(i) = self.messages.iter().position(|m| Some(m.uid) == selected) {
            self.sel = i;
        }
        self.sel = self.sel.min(self.messages.len().saturating_sub(1));

        if let Some(view) = &self.view
            && !self.messages.iter().any(|m| m.uid == view.uid)
        {
            self.view = None;
        }
        self.load_drafts()?;
        self.load_body()
    }

    /// Indexes the drafts that reply to a message, to list each one under
    /// the message it answers.
    fn load_drafts(&mut self) -> Result<()> {
        self.drafts.clear();
        if let Some(folder) = self.role_folder(&["drafts"])
            && folder != self.folder
        {
            for (date, raw) in self.store.bodies(&folder)? {
                if let Some((replied, preview)) = reply_draft(&raw) {
                    self.drafts.insert(replied, (date, preview));
                }
            }
        }
        Ok(())
    }

    /// Fills in the open message's body once it is in the store.
    fn load_body(&mut self) -> Result<()> {
        if let Some(view) = &mut self.view
            && view.body.is_none()
            && let Some(raw) = self.store.raw(&self.folder, view.uid)?
        {
            let (body, attachments) = body_and_attachments(&raw);
            view.body = Some(body);
            view.attachments = attachments;
        }
        Ok(())
    }

    /// The messages an action applies to: the open one, else the range of
    /// the running `:` command, else the visual selection, else `count`
    /// rows from the cursor.
    fn targets(&self, count: usize) -> Vec<Message> {
        if let Some(view) = &self.view {
            let open = self.messages.iter().find(|m| m.uid == view.uid);
            return open.cloned().into_iter().collect();
        }
        let (start, end) = match (self.range, &self.mode) {
            (Some(range), _) => range,
            _ if self.focus == Focus::Folders => return Vec::new(),
            (None, Mode::Visual(anchor)) => (*anchor.min(&self.sel), *anchor.max(&self.sel)),
            _ => (self.sel, self.sel + count.max(1) - 1),
        };
        let rows = self.messages.iter().skip(start).take(end - start + 1);
        rows.cloned().collect()
    }

    /// The first folder with one of the special-use `roles`.
    fn role_folder(&self, roles: &[&str]) -> Option<String> {
        let found = roles
            .iter()
            .find_map(|role| self.folders.iter().find(|f| f.role == *role));
        found.map(|f| f.name.clone())
    }

    /// The folder a folder command applies to.
    fn folder_under_cursor(&self) -> String {
        match self.folders.get(self.folder_sel) {
            Some(folder) if self.focus == Focus::Folders => folder.name.clone(),
            _ => self.folder.clone(),
        }
    }

    fn open_folder(&mut self, name: String) -> Result<()> {
        if name != self.folder {
            self.alternate = Some(std::mem::replace(&mut self.folder, name.clone()));
        }
        self.filter = None;
        self.view = None;
        self.mode = Mode::Normal;
        self.sel = 0;
        self.focus = Focus::List;
        self.send(Cmd::Open(name));
        self.reload()
    }

    fn open_message(&mut self) -> Result<()> {
        let Some(message) = self.messages.get(self.sel) else {
            return Ok(());
        };
        let (uid, seen) = (message.uid, message.seen);
        self.mode = Mode::Normal;
        self.view = Some(View {
            uid,
            body: None,
            attachments: Vec::new(),
            scroll: 0,
        });
        self.load_body()?;
        if self.view.as_ref().is_some_and(|v| v.body.is_none()) {
            self.send(Cmd::FetchBody(self.folder.clone(), uid));
        }
        if !seen {
            self.send(Cmd::SetFlag(
                self.folder.clone(),
                vec![uid],
                Flag::Seen,
                true,
            ));
        }
        Ok(())
    }

    /// Returns true when the app should quit.
    fn on_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> Result<bool> {
        if self.help {
            self.help = false;
            return Ok(false);
        }
        if let Some((_, cmd)) = self.confirm.take() {
            if key.code == KeyCode::Char('y') {
                self.send(cmd);
            } else {
                self.set_status("Cancelled".to_string(), false);
            }
            return Ok(false);
        }
        match self.mode {
            Mode::Command(_) | Mode::Search(_) => self.on_cmdline_key(key, terminal),
            Mode::Normal | Mode::Visual(_) => self.on_normal_key(key, terminal),
        }
    }

    fn on_cmdline_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> Result<bool> {
        let (Mode::Command(text) | Mode::Search(text)) = &mut self.mode else {
            return Ok(false);
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                // As in vim, backspacing past the start leaves the mode.
                if text.pop().is_none() {
                    self.mode = Mode::Normal;
                }
            }
            KeyCode::Tab => {
                // Complete a folder-name argument.
                if let Some((command, arg)) = text.split_once(' ') {
                    let arg = arg.to_lowercase();
                    let found = self.folders.iter();
                    let found = found
                        .map(|f| &f.name)
                        .find(|n| n.to_lowercase().starts_with(&arg));
                    if let Some(name) = found {
                        *text = format!("{command} {name}");
                    }
                }
            }
            KeyCode::Char(c) => text.push(c),
            KeyCode::Enter => match std::mem::replace(&mut self.mode, Mode::Normal) {
                Mode::Command(text) => return self.run_command(&text, terminal),
                Mode::Search(text) => {
                    self.search = text;
                    self.highlight = true;
                    self.find(!self.search_back);
                }
                _ => {}
            },
            _ => {}
        }
        Ok(false)
    }

    fn on_normal_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> Result<bool> {
        let visual = matches!(self.mode, Mode::Visual(_));
        let list = self.view.is_none() && self.focus == Focus::List;
        let last = self.messages.len().saturating_sub(1);

        if self.prefix == Some(CTRL_W) {
            self.prefix = None;
            if self.view.is_none() {
                self.mode = Mode::Normal;
                self.focus = match (key.code, &self.focus) {
                    (KeyCode::Char('h'), _) => Focus::Folders,
                    (KeyCode::Char('l'), _) => Focus::List,
                    (KeyCode::Char('w'), Focus::List) => Focus::Folders,
                    (KeyCode::Char('w'), Focus::Folders) => Focus::List,
                    (_, Focus::Folders) => Focus::Folders,
                    (_, Focus::List) => Focus::List,
                };
            }
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let page = self.page as isize;
            match key.code {
                KeyCode::Char('d') => self.step(page / 2),
                KeyCode::Char('u') => self.step(-page / 2),
                KeyCode::Char('f') => self.step(page),
                KeyCode::Char('b') => self.step(-page),
                KeyCode::Char('w') => self.prefix = Some(CTRL_W),
                KeyCode::Char('o' | '^' | '6') => match self.alternate.clone() {
                    Some(name) => self.open_folder(name)?,
                    None => self.set_status("E23: No alternate folder".to_string(), true),
                },
                KeyCode::Char('r') => self.set_status("Redo is not supported".to_string(), true),
                _ => {}
            }
            return Ok(false);
        }
        if let KeyCode::Char(c) = key.code
            && let Some(digit) = c.to_digit(10)
            && (digit != 0 || self.count.is_some())
        {
            let count = self.count.unwrap_or(0) * 10 + digit as usize;
            self.count = Some(count.min(99_999));
            return Ok(false);
        }
        let count = self.count.take();
        let n = count.unwrap_or(1);

        match (self.prefix.take(), key.code) {
            (Some('g'), KeyCode::Char('g')) => self.jump(n - 1),
            (Some('g'), KeyCode::Char('t')) => self.cycle_folder(1)?,
            (Some('g'), KeyCode::Char('T')) => self.cycle_folder(-1)?,
            (Some('d'), KeyCode::Char('d')) => self.delete(n),
            (Some('y'), KeyCode::Char('y')) => self.yank(n),
            (Some('z'), KeyCode::Char('z')) => self.offset = self.sel.saturating_sub(self.page / 2),
            (Some('z'), KeyCode::Char('t')) => self.offset = self.sel,
            (Some('z'), KeyCode::Char('b')) => {
                self.offset = (self.sel + 1).saturating_sub(self.page)
            }
            (Some('Z'), KeyCode::Char('Z')) => return self.close_layer(),
            (Some('Z'), KeyCode::Char('Q')) => return Ok(true),
            (Some(LEADER), KeyCode::Char('f')) => self.mode = Mode::Command("e ".to_string()),
            (Some(LEADER), KeyCode::Char('m')) => self.mode = Mode::Command("m ".to_string()),
            (Some(LEADER), KeyCode::Char('s')) => self.prefix = Some(LEADER_S),
            (Some(LEADER_S), KeyCode::Char('m')) => self.send(Cmd::SyncAll),
            (Some(LEADER_S), KeyCode::Char('f')) => {
                self.mode = Mode::Command("search ".to_string())
            }
            (Some(LEADER), KeyCode::Char('c')) => {
                self.compose(terminal, draft_template("", "", ""), None)?
            }
            (Some(LEADER), KeyCode::Char('r')) => self.reply(terminal)?,
            (Some(LEADER), KeyCode::Char('u')) => self.undo_last(),
            (Some(LEADER), KeyCode::Char('h')) => self.help = true,
            // Any other key cancels a half-typed sequence.
            (Some(_), _) => {}

            (None, KeyCode::Char('d' | 'x')) if visual => self.delete(n),
            (None, KeyCode::Char('y')) if visual => self.yank(n),
            (None, KeyCode::Char('x')) if self.view.is_some() || list => self.delete(n),
            (None, KeyCode::Char(c @ ('g' | 'd' | 'y'))) => {
                (self.prefix, self.count) = (Some(c), count)
            }
            (None, KeyCode::Char(c @ ('z' | 'Z'))) => self.prefix = Some(c),
            (None, KeyCode::Char(' ')) => self.prefix = Some(LEADER),

            (None, KeyCode::Char('j') | KeyCode::Down) => self.step(n as isize),
            (None, KeyCode::Char('k') | KeyCode::Up) => self.step(-(n as isize)),
            (None, KeyCode::Char('G')) => self.jump(count.map_or(usize::MAX, |row| row - 1)),
            (None, KeyCode::PageDown) => self.step(self.page as isize),
            (None, KeyCode::PageUp) => self.step(-(self.page as isize)),
            (None, KeyCode::Char('H')) if list => self.sel = self.offset.min(last),
            (None, KeyCode::Char('M')) if list => {
                let rows = self.page.min(self.messages.len() - self.offset.min(last));
                self.sel = (self.offset + rows.saturating_sub(1) / 2).min(last);
            }
            (None, KeyCode::Char('L')) if list => {
                self.sel = (self.offset + self.page - 1).min(last)
            }
            (None, KeyCode::Char('h') | KeyCode::Left) => {
                if self.view.take().is_none() {
                    self.mode = Mode::Normal;
                    self.focus = Focus::Folders;
                }
            }
            (None, KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter) => self.enter()?,
            (None, KeyCode::Tab) if self.view.is_none() => {
                self.mode = Mode::Normal;
                self.focus = match self.focus {
                    Focus::Folders => Focus::List,
                    Focus::List => Focus::Folders,
                };
            }
            (None, KeyCode::Esc) => self.escape()?,
            (None, KeyCode::Char('q')) => {
                if self.view.take().is_none() {
                    self.set_status("Type :q to quit".to_string(), false);
                }
            }

            (None, KeyCode::Char(':')) => {
                self.mode = match self.mode {
                    Mode::Visual(anchor) => {
                        self.visual = (anchor.min(self.sel), anchor.max(self.sel));
                        Mode::Command("'<,'>".to_string())
                    }
                    _ => Mode::Command(String::new()),
                }
            }
            (None, KeyCode::Char(c @ ('/' | '?'))) => {
                self.search_back = c == '?';
                self.mode = Mode::Search(String::new());
            }
            (None, KeyCode::Char('n')) => self.find(!self.search_back),
            (None, KeyCode::Char('N')) => self.find(self.search_back),
            (None, KeyCode::F(1)) => self.help = true,
            (None, KeyCode::Char('V' | 'v')) => {
                if visual {
                    self.mode = Mode::Normal;
                } else if list {
                    self.mode = Mode::Visual(self.sel);
                }
            }

            (None, KeyCode::Char('u')) => self.undo_last(),
            (None, KeyCode::Char('p')) => self.paste(),
            (None, KeyCode::Char('~')) => self.flag(n, Flag::Seen, None),
            (None, KeyCode::Char('s')) => self.flag(n, Flag::Flagged, None),
            (None, KeyCode::Char('a')) => self.archive(n),
            (None, KeyCode::Char('m')) => self.pick_destination(n),
            (None, KeyCode::Char('r')) => self.reply(terminal)?,
            (None, KeyCode::Char('o' | 'c')) => {
                self.compose(terminal, draft_template("", "", ""), None)?
            }
            (None, KeyCode::Char('R')) => self.send(Cmd::SyncAll),
            _ => {}
        }
        Ok(false)
    }

    /// `gt` / `gT`: opens the folder after (or before) the open one.
    fn cycle_folder(&mut self, delta: isize) -> Result<()> {
        let len = self.folders.len();
        let Some(at) = self.folders.iter().position(|f| f.name == self.folder) else {
            return Ok(());
        };
        let next = (at + len).saturating_add_signed(delta) % len;
        self.open_folder(self.folders[next].name.clone())
    }

    /// `n` / `N`: moves to the next (or previous) row, or line of the open
    /// message, that contains the last search, wrapping around.
    fn find(&mut self, forward: bool) {
        if self.search.is_empty() {
            self.set_status("E35: No previous regular expression".to_string(), true);
            return;
        }
        let needle = self.search.to_lowercase();
        let (rows, at) = match &self.view {
            Some(view) => (self.view_lines(), view.scroll),
            None => (self.messages.iter().map(searchable).collect(), self.sel),
        };
        let len = rows.len();
        let mut order = (1..=len).map(|step| match forward {
            true => (at + step) % len,
            false => (at + len - step) % len,
        });
        match order.find(|&i| rows[i].to_lowercase().contains(&needle)) {
            Some(found) => match &mut self.view {
                Some(view) => view.scroll = found,
                None => self.sel = found,
            },
            None => self.set_status(format!("E486: Pattern not found: {}", self.search), true),
        }
    }

    /// Moves the cursor (or scrolls the open message) by `delta` rows.
    fn step(&mut self, delta: isize) {
        let moved =
            |at: usize, len: usize| at.saturating_add_signed(delta).min(len.saturating_sub(1));
        let lines = self.view_lines().len();
        if let Some(view) = &mut self.view {
            view.scroll = moved(view.scroll, lines);
        } else if self.focus == Focus::Folders {
            self.folder_sel = moved(self.folder_sel, self.folders.len());
        } else {
            self.sel = moved(self.sel, self.messages.len());
        }
    }

    /// Moves the cursor to row `index`, clamped to the last row.
    fn jump(&mut self, index: usize) {
        if let Some(view) = &mut self.view {
            view.scroll = 0;
        } else if self.focus == Focus::Folders {
            self.folder_sel = 0;
        } else {
            self.sel = 0;
        }
        self.step(index.min(isize::MAX as usize) as isize);
    }

    /// `l` / Enter: open what is under the cursor.
    fn enter(&mut self) -> Result<()> {
        if self.view.is_some() {
            return Ok(());
        }
        if self.focus == Focus::List {
            return self.open_message();
        }
        let Some(name) = self.folders.get(self.folder_sel).map(|f| f.name.clone()) else {
            return Ok(());
        };
        if self.moving.is_empty() {
            return self.open_folder(name);
        }
        self.set_status(format!("Moving to {name}"), false);
        let uids = std::mem::take(&mut self.moving);
        let moved = self.messages.iter().filter(|m| uids.contains(&m.uid));
        self.remember_move(&moved.cloned().collect::<Vec<_>>(), &name);
        self.send(Cmd::Move(self.folder.clone(), uids, name));
        self.view = None;
        self.focus = Focus::List;
        Ok(())
    }

    /// `:q` closes one layer, as it closes one window in vim: the open
    /// message, then the search results, then hermes itself. Returns true
    /// when that was the last layer.
    fn close_layer(&mut self) -> Result<bool> {
        if self.view.take().is_some() {
            return Ok(false);
        }
        if self.filter.take().is_some() {
            self.set_status(String::new(), false);
            self.reload()?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Esc backs out of one thing at a time.
    fn escape(&mut self) -> Result<()> {
        if matches!(self.mode, Mode::Visual(_)) {
            self.mode = Mode::Normal;
        } else if !self.moving.is_empty() {
            self.moving.clear();
            self.focus = Focus::List;
            self.set_status(String::new(), false);
        } else if self.view.take().is_none() && self.filter.take().is_some() {
            self.set_status(String::new(), false);
            self.reload()?;
        }
        Ok(())
    }

    /// Records how to undo moving `messages` out of the open folder.
    fn remember_move(&mut self, messages: &[Message], dest: &str) {
        if dest == self.folder {
            return;
        }
        // A message is found again in `dest` by its Message-ID.
        let with_id = messages.iter().filter(|m| !m.message_id.is_empty());
        let ids: Vec<String> = with_id.map(|m| m.message_id.clone()).collect();
        if !ids.is_empty() {
            let back = Cmd::MoveBack(dest.to_string(), ids, self.folder.clone());
            self.undo.push(vec![back]);
        }
    }

    /// Sends one command for all the target messages, which moves them to
    /// `dest` (if known, so that it can be undone), and leaves visual mode.
    fn move_targets(
        &mut self,
        count: usize,
        dest: Option<String>,
        command: impl FnOnce(String, Vec<u32>) -> Cmd,
    ) {
        let targets = self.targets(count);
        self.mode = Mode::Normal;
        if targets.is_empty() {
            return;
        }
        if let Some(dest) = dest {
            self.remember_move(&targets, &dest);
        }
        let uids = targets.iter().map(|m| m.uid).collect();
        self.send(command(self.folder.clone(), uids));
        self.view = None;
    }

    fn delete(&mut self, count: usize) {
        if self.focus == Focus::Folders && self.view.is_none() && self.range.is_none() {
            let name = self.folder_under_cursor();
            self.confirm = Some((
                format!("Delete folder {name} and all mail in it?"),
                Cmd::DeleteFolder(name),
            ));
            return;
        }
        self.move_targets(count, self.role_folder(&["trash"]), Cmd::Delete);
    }

    fn archive(&mut self, count: usize) {
        self.move_targets(count, self.role_folder(&["archive", "all"]), Cmd::Archive);
    }

    /// Sets a flag on the targets; `None` toggles, following the first one.
    fn flag(&mut self, count: usize, flag: Flag, on: Option<bool>) {
        let targets = self.targets(count);
        let Some(first) = targets.first() else {
            return;
        };
        let current = |m: &Message| match flag {
            Flag::Flagged => m.flagged,
            _ => m.seen,
        };
        let on = on.unwrap_or(!current(first));
        let folder = self.folder.clone();
        let uids = |messages: &mut dyn Iterator<Item = &Message>| messages.map(|m| m.uid).collect();
        // Undo restores each message's own previous value.
        let (was_on, was_off): (Vec<&Message>, Vec<&Message>) =
            targets.iter().partition(|m| current(m));
        let mut back = Vec::new();
        for (messages, value) in [(was_on, true), (was_off, false)] {
            if !messages.is_empty() {
                let uids = uids(&mut messages.into_iter());
                back.push(Cmd::SetFlag(folder.clone(), uids, flag, value));
            }
        }
        self.undo.push(back);
        self.send(Cmd::SetFlag(folder, uids(&mut targets.iter()), flag, on));
        self.mode = Mode::Normal;
    }

    /// `u`: reverses the last delete, archive, move or flag change.
    fn undo_last(&mut self) {
        let Some(commands) = self.undo.pop() else {
            self.set_status("Already at oldest change".to_string(), false);
            return;
        };
        self.set_status("1 change undone".to_string(), false);
        for command in commands {
            self.send(command);
        }
    }

    /// `yy`: remembers the targets for `p`.
    fn yank(&mut self, count: usize) {
        let uids: Vec<u32> = self.targets(count).iter().map(|m| m.uid).collect();
        self.mode = Mode::Normal;
        if !uids.is_empty() {
            self.set_status(format!("{} messages yanked", uids.len()), false);
            self.yanked = Some((self.folder.clone(), uids));
        }
    }

    /// `p`: copies the yanked messages into the open folder.
    fn paste(&mut self) {
        let Some((from, uids)) = self.yanked.clone() else {
            self.set_status("E353: Nothing in register".to_string(), true);
            return;
        };
        if from == self.folder {
            self.set_status("Open another folder to paste into".to_string(), true);
            return;
        }
        self.set_status(format!("{} messages copied here", uids.len()), false);
        self.send(Cmd::Copy(from, uids, self.folder.clone()));
    }

    /// `m`: remember the targets and let the folders pane pick where to.
    fn pick_destination(&mut self, count: usize) {
        self.moving = self.targets(count).iter().map(|m| m.uid).collect();
        if self.moving.is_empty() {
            return;
        }
        self.mode = Mode::Normal;
        self.focus = Focus::Folders;
        self.set_status(
            "Move to: pick a folder, Enter moves, Esc cancels".into(),
            false,
        );
    }

    fn search(&mut self, text: String) {
        let text = text.trim().to_string();
        if !text.is_empty() {
            self.set_status(format!("Searching for {text}..."), false);
            self.send(Cmd::Search(self.folder.clone(), text));
        }
    }

    /// Parses one address of a `:` range into a row: a number, `.`, `$`,
    /// `'<` or `'>`. Returns the row and the rest of the line.
    fn parse_address<'a>(&self, text: &'a str) -> Option<(usize, &'a str)> {
        let last = self.messages.len().saturating_sub(1);
        if let Some(rest) = text.strip_prefix("'<") {
            return Some((self.visual.0, rest));
        }
        if let Some(rest) = text.strip_prefix("'>") {
            return Some((self.visual.1, rest));
        }
        if let Some(rest) = text.strip_prefix('.') {
            return Some((self.sel, rest));
        }
        if let Some(rest) = text.strip_prefix('$') {
            return Some((last, rest));
        }
        let rest = text.trim_start_matches(|c: char| c.is_ascii_digit());
        let number: usize = text[..text.len() - rest.len()].parse().ok()?;
        Some((number.saturating_sub(1).min(last), rest))
    }

    /// Splits a leading range (`%`, `5`, `2,5`, `'<,'>`) off a `:` line.
    fn parse_range<'a>(&self, line: &'a str) -> (Option<(usize, usize)>, &'a str) {
        if let Some(rest) = line.strip_prefix('%') {
            let last = self.messages.len().saturating_sub(1);
            return (Some((0, last)), rest.trim_start());
        }
        let Some((start, rest)) = self.parse_address(line) else {
            return (None, line);
        };
        let second = rest.strip_prefix(',').and_then(|r| self.parse_address(r));
        match second {
            Some((end, rest)) => (Some((start.min(end), start.max(end))), rest.trim_start()),
            None => (Some((start, start)), rest.trim_start()),
        }
    }

    /// Runs a `:` command. Returns true when the app should quit.
    fn run_command(&mut self, line: &str, terminal: &mut DefaultTerminal) -> Result<bool> {
        let (range, line) = self.parse_range(line.trim());
        let (name, arg) = line.split_once(' ').unwrap_or((line, ""));
        let arg = arg.trim().to_string();
        if name.is_empty() {
            // A bare range, as in `:5`, moves the cursor.
            if let (Some((_, row)), None) = (range, &self.view) {
                self.focus = Focus::List;
                self.sel = row;
            }
            return Ok(false);
        }
        let needs_arg = [
            "m", "move", "mv", "e", "edit", "b", "buffer", "folder", "cd", "mkdir", "rename",
            "search",
        ];
        if needs_arg.contains(&name) && arg.is_empty() {
            self.set_status(format!("E471: Argument required: {name}"), true);
            return Ok(false);
        }
        self.range = range;
        let quit = self.dispatch(name, arg, terminal);
        self.range = None;
        quit
    }

    fn dispatch(
        &mut self,
        name: &str,
        arg: String,
        terminal: &mut DefaultTerminal,
    ) -> Result<bool> {
        match name {
            "q" | "quit" | "wq" | "x" => return self.close_layer(),
            "qa" | "qall" => return Ok(true),
            "w" | "write" | "sync" => self.send(Cmd::SyncAll),
            "compose" => self.compose(terminal, draft_template("", "", ""), None)?,
            "reply" => self.reply(terminal)?,
            "d" | "delete" => self.delete(1),
            "y" | "yank" => self.yank(1),
            "u" | "undo" => self.undo_last(),
            "archive" => self.archive(1),
            "m" | "move" | "mv" => {
                let dest = arg.clone();
                self.move_targets(1, Some(arg), |folder, uids| Cmd::Move(folder, uids, dest));
            }
            "read" => self.flag(1, Flag::Seen, Some(true)),
            "unread" => self.flag(1, Flag::Seen, Some(false)),
            "star" => self.flag(1, Flag::Flagged, Some(true)),
            "unstar" => self.flag(1, Flag::Flagged, Some(false)),
            "e" | "edit" | "b" | "buffer" | "folder" | "cd" => {
                if self.folders.iter().any(|f| f.name == arg) {
                    self.open_folder(arg)?;
                } else {
                    self.set_status(format!("E94: No matching folder for {arg}"), true);
                }
            }
            "bn" | "bnext" => self.cycle_folder(1)?,
            "bp" | "bprevious" | "bN" => self.cycle_folder(-1)?,
            "mkdir" => self.send(Cmd::CreateFolder(arg)),
            "rename" => self.send(Cmd::RenameFolder(self.folder_under_cursor(), arg)),
            "rmdir" => {
                let name = self.folder_under_cursor();
                self.confirm = Some((
                    format!("Delete folder {name} and all mail in it?"),
                    Cmd::DeleteFolder(name),
                ));
            }
            "search" => self.search(arg),
            "noh" | "nohlsearch" => self.highlight = false,
            "help" | "h" => self.help = true,
            _ => self.set_status(format!("E492: Not an editor command: {name}"), true),
        }
        Ok(false)
    }

    fn reply(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let Some(message) = self.targets(1).into_iter().next() else {
            return Ok(());
        };
        let Some(raw) = self.store.raw(&self.folder, message.uid)? else {
            self.set_status("Message is not downloaded yet".to_string(), true);
            return Ok(());
        };
        let subject = if message.subject.to_ascii_lowercase().starts_with("re:") {
            message.subject.clone()
        } else {
            format!("Re: {}", message.subject)
        };
        let (body, _) = body_and_attachments(&raw);
        let quoted: Vec<String> = body.lines().map(|line| format!("> {line}")).collect();
        let template = draft_template(
            &reply_address(&raw),
            &subject,
            &format!(
                "\n\nOn {}, {} wrote:\n{}\n",
                format_date(message.date),
                message.from,
                quoted.join("\n")
            ),
        );
        self.compose(terminal, template, Some(&message))
    }

    /// Hands `template` to $EDITOR and, if it was edited, asks whether to
    /// send the result.
    fn compose(
        &mut self,
        terminal: &mut DefaultTerminal,
        template: String,
        reply_to: Option<&Message>,
    ) -> Result<()> {
        let path = std::env::temp_dir().join(format!("hermes-{}.eml", std::process::id()));
        std::fs::write(&path, &template)?;
        let editor = std::env::var("EDITOR").unwrap_or("vi".to_string());
        ratatui::restore();
        // Through sh so that an EDITOR with arguments works.
        let exit = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(&path)
            .status();
        *terminal = ratatui::init();
        terminal.clear()?;
        let text = std::fs::read_to_string(&path);
        std::fs::remove_file(&path).ok();
        let text = text?;

        self.mode = Mode::Normal;
        let mut email = parse_draft(&text, &self.user);
        if !exit?.success() || text == template || email.to.is_empty() {
            self.set_status(
                "Nothing sent: draft unchanged or no recipient".into(),
                false,
            );
            return Ok(());
        }
        if let Some(message) = reply_to
            && !message.message_id.is_empty()
        {
            email.other_headers.insert(
                "In-Reply-To".to_string(),
                format!("<{}>", message.message_id),
            );
        }
        self.confirm = Some((
            format!("Send \"{}\" to {}?", email.subject, email.to.join(", ")),
            Cmd::Send(email, reply_to.map(|m| (self.folder.clone(), m.uid))),
        ));
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [main, statusline, cmdline] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let [left, right] =
            Layout::horizontal([Constraint::Length(30), Constraint::Min(0)]).areas(main);
        self.page = right.height.saturating_sub(2).max(1) as usize;
        // Scroll only as far as needed to keep the cursor in the window.
        self.offset = self
            .offset
            .clamp((self.sel + 1).saturating_sub(self.page), self.sel);

        self.draw_folders(frame, left);
        if self.help {
            let help = Paragraph::new(HELP).block(self.block("Help".to_string(), true));
            frame.render_widget(help, right);
        } else if self.view.is_some() {
            self.draw_view(frame, right);
        } else {
            self.draw_list(frame, right);
        }
        if let Some(prefix) = self.prefix {
            self.draw_which_key(frame, main, prefix);
        }
        self.draw_statusline(frame, statusline);
        self.draw_cmdline(frame, cmdline);
    }

    /// Popup along the bottom listing the keys that can come next.
    fn draw_which_key(&self, frame: &mut Frame, area: Rect, prefix: char) {
        const COLUMN: u16 = 36;
        let keys = which_key(prefix);
        let columns = (area.width / COLUMN).max(1) as usize;
        let rows = keys.len().div_ceil(columns) as u16;
        // A rule on top, the keys, a blank row and the footer.
        let height = (rows + 3).min(area.height);
        let popup = Rect {
            y: area.bottom() - height,
            height,
            ..area
        };
        let dim = Style::new().fg(Color::DarkGray);
        let title = match prefix {
            LEADER => " <leader> ".to_string(),
            LEADER_S => " <leader>s ".to_string(),
            CTRL_W => " ^W ".to_string(),
            _ => format!(" {prefix} "),
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Block::new()
                .borders(Borders::TOP)
                .border_style(dim)
                .title(title),
            popup,
        );
        // Filled column by column.
        for (i, (key, description)) in keys.iter().enumerate() {
            let (column, row) = (i as u16 / rows, i as u16 % rows);
            let cell = Rect {
                x: popup.x + 2 + column * COLUMN,
                y: popup.y + 1 + row,
                width: COLUMN.min(popup.right().saturating_sub(popup.x + 2 + column * COLUMN)),
                height: 1,
            };
            let line = Line::from(vec![
                Span::styled(*key, Style::new().fg(Color::Yellow)),
                Span::styled(" \u{2192} ", dim),
                Span::raw(*description),
            ]);
            frame.render_widget(line, cell.intersection(popup));
        }
        let footer = Rect {
            y: popup.bottom() - 1,
            height: 1,
            ..popup
        };
        frame.render_widget(Line::styled("esc close", dim).centered(), footer);
    }

    /// Mode badge, folder and cursor position, as in a vim statusline.
    fn draw_statusline(&self, frame: &mut Frame, area: Rect) {
        let (name, color) = match self.mode {
            Mode::Normal => ("NORMAL", Color::Blue),
            Mode::Visual(_) => ("VISUAL", Color::Magenta),
            Mode::Command(_) => ("COMMAND", Color::Green),
            Mode::Search(_) => ("SEARCH", Color::Green),
        };
        let badge = Style::new()
            .bg(color)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD);
        let bar = Style::new().bg(Color::DarkGray).fg(Color::White);
        let left = Line::from(vec![
            Span::styled(format!(" {name} "), badge),
            Span::raw(format!(" {}  {}", self.folder, self.user)),
        ]);
        let row = if self.messages.is_empty() {
            0
        } else {
            self.sel + 1
        };
        let right = format!(":help  {}/{} ", row, self.messages.len());
        frame.render_widget(left.style(bar), area);
        frame.render_widget(Line::from(right).right_aligned(), area);
    }

    /// The bottom row: the `:` or `/` being typed, a question, or the
    /// last message, with any half-typed count and key on the right.
    fn draw_cmdline(&self, frame: &mut Frame, area: Rect) {
        let typing = |sigil: char, text: &str| {
            let x = area.x + 1 + text.chars().count() as u16;
            (Line::from(format!("{sigil}{text}")), Some(x))
        };
        let (line, cursor) = match &self.mode {
            Mode::Command(text) => typing(':', text),
            Mode::Search(text) => typing('/', text),
            _ => match &self.confirm {
                Some((question, _)) => (Line::from(format!("{question} (y/n)")), None),
                None if self.error => (
                    Line::styled(self.status.as_str(), Style::new().fg(Color::Red)),
                    None,
                ),
                None => (Line::from(self.status.as_str()), None),
            },
        };
        frame.render_widget(line, area);
        if let Some(x) = cursor {
            frame.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
        }
        let pending = format!(
            "{}{} ",
            self.count.map(|c| c.to_string()).unwrap_or_default(),
            match self.prefix {
                Some(LEADER) => "<leader>".to_string(),
                Some(LEADER_S) => "<leader>s".to_string(),
                Some(CTRL_W) => "^W".to_string(),
                Some(key) => key.to_string(),
                None => String::new(),
            }
        );
        frame.render_widget(Line::from(pending).right_aligned(), area);
    }

    fn block(&self, title: String, focused: bool) -> Block<'static> {
        let color = if focused {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        Block::bordered()
            .title(title)
            .border_style(Style::new().fg(color))
    }

    fn draw_folders(&self, frame: &mut Frame, area: Rect) {
        let items = self.folders.iter().map(|folder| {
            let text = if folder.unread > 0 {
                format!("{} ({})", folder.name, folder.unread)
            } else {
                folder.name.clone()
            };
            let style = if folder.name == self.folder {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            Line::styled(text, style)
        });
        let focused = self.focus == Focus::Folders && self.view.is_none();
        let mut list = List::new(items).block(self.block("Folders".to_string(), focused));
        if focused {
            list = list.highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        }
        let mut state = ListState::default().with_selected(Some(self.folder_sel));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_list(&self, frame: &mut Frame, area: Rect) {
        let selected = match self.mode {
            Mode::Visual(anchor) => Some(anchor.min(self.sel)..=anchor.max(self.sel)),
            _ => None,
        };
        let needle = self.search.to_lowercase();
        let sent = self
            .folders
            .iter()
            .any(|f| f.name == self.folder && f.role == "sent");
        // Only the rows on screen are built; a folder can hold 100k messages.
        let rows = self.messages.iter().enumerate();
        let rows = rows.skip(self.offset).take(self.page).map(|(i, m)| {
            let flags = format!(
                "{}{}{}",
                if m.seen { ' ' } else { 'N' },
                if m.flagged { '*' } else { ' ' },
                if m.answered { 'r' } else { ' ' }
            );
            let who = if sent { &m.to } else { &m.from };
            let mut style = Style::new();
            if !m.seen {
                style = style.add_modifier(Modifier::BOLD);
            }
            if self.highlight
                && !needle.is_empty()
                && searchable(m).to_lowercase().contains(&needle)
            {
                style = style.fg(Color::Yellow);
            }
            if selected.as_ref().is_some_and(|rows| rows.contains(&i)) {
                style = style.bg(Color::DarkGray);
            }
            let mut cells = [
                Text::from(flags),
                Text::from(format_date(m.date)),
                Text::from(display_name(who)),
                Text::from(m.subject.clone()),
            ];
            // A draft replying to this message gets a second line under it.
            let draft = self.drafts.get(&m.message_id);
            if let Some((date, preview)) = draft {
                let dim = Style::new().fg(Color::Cyan).add_modifier(Modifier::ITALIC);
                cells[1].push_line(Line::styled(format_date(*date), dim));
                cells[2].push_line(Line::styled("  \u{21b3} draft reply", dim));
                cells[3].push_line(Line::styled(preview.clone(), dim));
            }
            Row::new(cells)
                .height(if draft.is_some() { 2 } else { 1 })
                .style(style)
        });
        let title = match &self.filter {
            Some(_) => format!("{} (search: {} matches)", self.folder, self.messages.len()),
            None => format!("{} ({})", self.folder, self.messages.len()),
        };
        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Length(16),
                Constraint::Length(24),
                Constraint::Min(10),
            ],
        )
        .block(self.block(title, self.focus == Focus::List))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut state = TableState::default().with_selected(Some(self.sel - self.offset));
        frame.render_stateful_widget(table, area, &mut state);
    }

    /// The open message as text: headers, a blank line, the body.
    fn view_lines(&self) -> Vec<String> {
        let Some(view) = &self.view else {
            return Vec::new();
        };
        let Some(message) = self.messages.iter().find(|m| m.uid == view.uid) else {
            return Vec::new();
        };
        let mut lines = vec![
            format!("From:    {}", message.from),
            format!("To:      {}", message.to),
        ];
        if !message.cc.is_empty() {
            lines.push(format!("Cc:      {}", message.cc));
        }
        lines.push(format!("Date:    {}", format_date(message.date)));
        lines.push(format!("Subject: {}", message.subject));
        if !view.attachments.is_empty() {
            lines.push(format!("Attachments: {}", view.attachments.join(", ")));
        }
        lines.push(String::new());
        match &view.body {
            Some(body) => lines.extend(body.lines().map(|l| l.replace('\t', "    "))),
            None => lines.push("Downloading...".to_string()),
        }
        lines
    }

    fn draw_view(&self, frame: &mut Frame, area: Rect) {
        let Some(view) = &self.view else {
            return;
        };
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let lines = self.view_lines().into_iter().map(|line| {
            let style = if line.starts_with("Subject: ") {
                bold
            } else {
                Style::new()
            };
            Line::styled(line, style)
        });
        let paragraph = Paragraph::new(lines.collect::<Vec<_>>())
            .block(self.block(self.folder.clone(), true))
            .wrap(Wrap { trim: false })
            .scroll((view.scroll.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(paragraph, area);
    }
}

/// The text of a message row that `/` searches.
fn searchable(message: &Message) -> String {
    format!("{} {} {}", message.from, message.to, message.subject)
}

/// The name part of "Name <addr>", or the whole string when there is none.
fn display_name(address: &str) -> String {
    match address.split_once('<') {
        Some((name, _)) if !name.trim().is_empty() => name.trim().to_string(),
        _ => address.to_string(),
    }
}
