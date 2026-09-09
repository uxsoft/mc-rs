use crate::{
    archives, external,
    jobs::{self, Decision, Job, Operation},
    menu::{self, MENUS},
    panel::Panel,
    ui,
    vfs::{AuthRequest, Context, Kind, Secret, VfsPath, remote},
};
use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode as K, KeyEvent, KeyEventKind, KeyModifiers as M, MouseButton,
    MouseEventKind,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub enum Dialog {
    Input {
        title: String,
        value: String,
        cursor: usize,
        purpose: Purpose,
    },
    Delete {
        permanent: bool,
    },
    Message {
        title: String,
        text: String,
    },
    Jobs,
    Quit,
}
#[derive(Clone, Copy)]
pub enum Purpose {
    Copy,
    Move,
    Mkdir,
    Search,
    Goto,
    Select,
    Unselect,
}
pub struct Search {
    pub results: Arc<Mutex<Vec<VfsPath>>>,
    pub done: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub errors: Arc<Mutex<usize>>,
    pub cursor: usize,
}
pub struct ArchiveTask {
    pub receiver: mpsc::Receiver<Result<VfsPath>>,
    pub cancel: Arc<AtomicBool>,
    pub bytes: Arc<Mutex<u64>>,
    pub panel: usize,
}
pub struct PasswordDialog {
    pub request: AuthRequest,
    pub value: Secret,
}
pub struct ViewTask {
    pub receiver: mpsc::Receiver<Result<Box<dyn std::io::Read + Send>>>,
    pub cancel: Arc<AtomicBool>,
}
pub struct PathTask {
    pub receiver: mpsc::Receiver<Result<VfsPath>>,
    pub cancel: Arc<AtomicBool>,
    pub panel: usize,
    pub purpose: Purpose,
}
pub struct App {
    pub connecting: Option<PathTask>,
    pending_connections: std::collections::VecDeque<(String, usize)>,
    pub password: Option<PasswordDialog>,
    auth_tx: mpsc::Sender<AuthRequest>,
    auth_rx: mpsc::Receiver<AuthRequest>,
    pub viewing: Option<ViewTask>,
    pub panels: [Panel; 2],
    pub active: usize,
    pub dialog: Option<Dialog>,
    pub menu: Option<menu::State>,
    pub menu_tabs: [ratatui::layout::Rect; 4],
    pub menu_area: ratatui::layout::Rect,
    pub jobs: Vec<Job>,
    pub search: Option<Search>,
    pub archive: Option<ArchiveTask>,
    pub status: String,
    pub areas: [ratatui::layout::Rect; 2],
    pub dialog_area: ratatui::layout::Rect,
    pub height: u16,
    pub width: u16,
    pub quit: bool,
    pub quick: String,
    last_click: Option<(Instant, usize, usize)>,
    escape: Option<Instant>,
    completed: usize,
}
impl App {
    pub fn new(left: PathBuf, right: PathBuf) -> Self {
        let (auth_tx, auth_rx) = mpsc::channel();
        let ctx = Context {
            auth: Some(auth_tx.clone()),
            ..Default::default()
        };
        Self {
            connecting: None,
            pending_connections: Default::default(),
            password: None,
            auth_tx,
            auth_rx,
            viewing: None,
            panels: [
                Panel::with_context(left, ctx.clone()),
                Panel::with_context(right, ctx),
            ],
            active: 0,
            dialog: None,
            menu: None,
            menu_tabs: Default::default(),
            menu_area: Default::default(),
            jobs: vec![],
            search: None,
            archive: None,
            status: "Ready · Alt+? find files · F9 menu".into(),
            areas: Default::default(),
            dialog_area: Default::default(),
            height: 0,
            width: 0,
            quit: false,
            quick: String::new(),
            last_click: None,
            escape: None,
            completed: 0,
        }
    }
    pub fn panel(&self) -> &Panel {
        &self.panels[self.active]
    }
    pub fn panel_mut(&mut self) -> &mut Panel {
        &mut self.panels[self.active]
    }
    pub fn message(&mut self, title: &str, text: impl Into<String>) {
        self.dialog = Some(Dialog::Message {
            title: title.into(),
            text: text.into(),
        });
    }
    pub fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.poll();
            if let Some(result) = self
                .viewing
                .as_ref()
                .and_then(|v| v.receiver.try_recv().ok())
            {
                self.viewing = None;
                let result = result.and_then(|reader| external::cat_stream(reader, terminal));
                if let Err(e) = result {
                    self.message("View file", e.to_string());
                }
            }
            terminal.draw(|frame| ui::draw(frame, self))?;
            if event::poll(Duration::from_millis(40))? {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => self.key(k, terminal)?,
                    Event::Mouse(m) => self.mouse(m, terminal)?,
                    _ => {}
                }
            }
        }
        Ok(())
    }
    pub fn poll(&mut self) {
        if self.connecting.is_none()
            && let Some((value, panel)) = self.pending_connections.pop_front()
        {
            self.connect_url(value, panel);
        }
        if let Some(result) = self
            .connecting
            .as_ref()
            .and_then(|t| t.receiver.try_recv().ok())
        {
            let task = self.connecting.take().unwrap();
            if !task.cancel.load(Ordering::Relaxed) {
                match result {
                    Ok(path) => match task.purpose {
                        Purpose::Copy => self.start(Operation::Copy, path),
                        Purpose::Move => self.start(Operation::Move, path),
                        _ => self.panels[task.panel].navigate(path),
                    },
                    Err(e) => self.message("Connection / directory", e.to_string()),
                }
            }
        }
        if self
            .password
            .as_ref()
            .is_some_and(|p| p.request.cancel.load(Ordering::Relaxed))
        {
            self.password = None;
        }
        if self.password.is_none()
            && let Ok(request) = self.auth_rx.try_recv()
        {
            self.password = Some(PasswordDialog {
                request,
                value: Secret::new(String::with_capacity(256)),
            });
        }
        for p in &mut self.panels {
            p.poll();
        }
        for job in &mut self.jobs {
            if job.progress.lock().unwrap().done {
                job.resources.clear();
            }
        }
        let done = self
            .jobs
            .iter()
            .filter(|j| j.progress.lock().unwrap().done)
            .count();
        if done != self.completed {
            self.completed = done;
            for p in &mut self.panels {
                p.refresh();
            }
            self.status = "Job finished · F9 → Background jobs for results".into();
        }
        if let Some(result) = self
            .archive
            .as_ref()
            .and_then(|a| a.receiver.try_recv().ok())
        {
            let task = self.archive.take().unwrap();
            match result {
                Ok(mount) => {
                    let p = &mut self.panels[task.panel];
                    p.navigate(mount);
                    self.status = "Archive · read only · F5 extracts to the other panel".into();
                }
                Err(e) => self.message("Archive", e.to_string()),
            }
        }
    }
    fn busy(&self) -> bool {
        self.jobs.iter().any(|j| !j.progress.lock().unwrap().done)
    }
    fn input(&mut self, purpose: Purpose, title: &str, value: String) {
        self.dialog = Some(Dialog::Input {
            title: title.into(),
            cursor: value.len(),
            value,
            purpose,
        });
    }
    fn start(&mut self, op: Operation, destination: VfsPath) {
        if !self.panel().path.fs.capabilities().write && op != Operation::Copy {
            self.message(
                "Read-only archive",
                "Archive entries can be viewed and copied out. Modification is unavailable.",
            );
            return;
        }
        if !destination.fs.capabilities().write && matches!(op, Operation::Copy | Operation::Move) {
            self.message(
                "Read-only destination",
                "Leave the archive in the destination panel first.",
            );
            return;
        }
        if op == Operation::Trash && !self.panel().path.fs.capabilities().trash {
            self.message("Trash unavailable", "This filesystem has no trash. Use F8 and explicitly choose Permanently delete to remove remote files.");
            return;
        }
        let sources = self.panel().sources();
        if sources.is_empty() && op != Operation::Mkdir {
            return;
        }
        let resources = jobs::resources(op, &sources, &destination);
        if self.jobs.iter().any(|job| {
            !job.progress.lock().unwrap().done && jobs::overlaps(&resources, &job.resources)
        }) {
            self.message("Paths in use", "A running job uses these paths. Wait or cancel it in Background jobs; unrelated jobs can run together.");
            return;
        }
        self.jobs.push(jobs::start(
            op,
            sources,
            destination,
            Context {
                auth: Some(self.auth_tx.clone()),
                ..Default::default()
            },
        ));
        self.status = "Working in background · F9 → Background jobs".into();
    }
    fn open(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        if self.panel().loading {
            return Ok(());
        }
        if self.panel().cursor == 0 {
            self.panel_mut().parent();
            return Ok(());
        }
        if let Some(entry) = self.panel().current().cloned() {
            if entry.directory {
                self.panel_mut().navigate(entry.path);
            } else if archives::supported(&entry.path) {
                let (tx, rx) = mpsc::channel();
                let cancel = Arc::new(AtomicBool::new(false));
                let bytes = Arc::new(Mutex::new(0));
                let c = cancel.clone();
                let b = bytes.clone();
                let auth = self.auth_tx.clone();
                std::thread::spawn(move || {
                    let result = archives::open(
                        entry.path,
                        &Context {
                            cancel: c,
                            auth: Some(auth),
                            ..Default::default()
                        },
                        |n| *b.lock().unwrap() = n,
                    );
                    let _ = tx.send(result);
                });
                self.archive = Some(ArchiveTask {
                    receiver: rx,
                    cancel,
                    bytes,
                    panel: self.active,
                });
            } else {
                self.view(false, terminal)?;
            }
        }
        Ok(())
    }
    fn view(&mut self, edit: bool, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        if edit && !self.panel().path.fs.capabilities().write {
            self.message(
                "Read-only archive",
                "Copy the file to a local directory before editing.",
            );
            return Ok(());
        }
        if let Some(e) = self.panel().current()
            && !e.directory
        {
            let path = e.path.clone();
            if let Some(local) = path.local_path() {
                if let Err(e) = external::launch(&local, edit, terminal) {
                    self.message("External program", e.to_string());
                }
            } else if edit {
                self.message(
                    "Editor",
                    "Copy this file to a local directory before editing.",
                );
            } else {
                let (tx, receiver) = mpsc::channel();
                let ctx = Context {
                    auth: Some(self.auth_tx.clone()),
                    ..Default::default()
                };
                let cancel = ctx.cancel.clone();
                std::thread::spawn(move || {
                    use std::io::Read;
                    let result = (|| -> Result<Box<dyn Read + Send>> {
                        let mut reader = path.fs.open_read(&path.path, &ctx)?;
                        let mut first = vec![0; 64 * 1024];
                        let n = reader.read(&mut first)?;
                        first.truncate(n);
                        Ok(Box::new(std::io::Cursor::new(first).chain(reader)))
                    })();
                    let _ = tx.send(result);
                });
                self.viewing = Some(ViewTask { receiver, cancel });
            }
            self.panel_mut().refresh();
        }
        Ok(())
    }
    fn function(&mut self, n: u8, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        match n {
            1 => self.message("Keyboard help", "Tab  Switch panel\n↑ ↓ Home End PgUp PgDn  Navigate\nEnter  Open directory/archive or cat file\nSpace / Insert / Ctrl+T  Select and advance; size directories\n+ / - / *  Select / unselect / invert\nAlt+.  Hidden files    Ctrl+R  Refresh\nAlt+?  Recursive filename search\nCtrl+S  Quick search (then type)\nCtrl+U  Swap panels    Alt+O  Other panel to current directory\nF3 cat   F4 editor   F5 copy   F6 move\nF7 mkdir   F8 delete   F9 menu   F10 quit\nEsc then digit is an alternative to F1–F10.\nRight-click selects; double-click opens.\nShell integration and F2 user menus are omitted."),
            3 => self.view(false, terminal)?, 4 => self.view(true, terminal)?,
            5 | 6 => { if !self.panel().sources().is_empty() { self.input(if n == 5 { Purpose::Copy } else { Purpose::Move }, if n == 5 { "Copy to" } else { "Move to" }, self.panels[1-self.active].path.display().to_string()); } }
            7 => self.input(Purpose::Mkdir, "Create directory", String::new()),
            8 => { if !self.panel().sources().is_empty() { self.dialog = Some(Dialog::Delete { permanent: false }); } }
            9 => { self.quick.clear(); self.menu = Some(menu::State::default()); },
            10 => { if self.busy() { self.dialog = Some(Dialog::Quit); } else { self.quit = true; } }, _ => {}
        }
        Ok(())
    }
    fn conflict_key(&mut self, k: KeyEvent) -> bool {
        for job in &self.jobs {
            let mut p = job.progress.lock().unwrap();
            if p.conflict.is_some() {
                let decision = match k.code {
                    K::Char('o') => Some(Decision::Overwrite),
                    K::Char('a') => Some(Decision::OverwriteAll),
                    K::Char('s') | K::Enter => Some(Decision::Skip),
                    K::Char('n') => Some(Decision::SkipAll),
                    K::Esc => Some(Decision::Cancel),
                    _ => None,
                };
                if let Some(d) = decision {
                    let c = p.conflict.take().unwrap();
                    let _ = c.reply.send(d);
                }
                return true;
            }
        }
        false
    }
    pub fn key(&mut self, mut k: KeyEvent, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        if k.code == K::Char('l') && k.modifiers == M::CONTROL {
            terminal.clear()?;
            return Ok(());
        }
        if let Some(mut password) = self.password.take() {
            match k.code {
                K::Enter => {
                    let _ = password.request.reply.send(Some(password.value));
                    return Ok(());
                }
                K::Esc => {
                    let _ = password.request.reply.send(None);
                    return Ok(());
                }
                K::Backspace => {
                    password.value.pop();
                }
                K::Char(c)
                    if !k.modifiers.intersects(M::CONTROL | M::ALT)
                        && password.value.len() + c.len_utf8() <= 256 =>
                {
                    password.value.push(c)
                }
                _ => {}
            }
            self.password = Some(password);
            return Ok(());
        }
        if let Some(view) = &self.viewing {
            if k.code == K::Esc {
                view.cancel.store(true, Ordering::Relaxed);
            }
            return Ok(());
        }
        if let Some(task) = &self.connecting {
            if k.code == K::Esc {
                task.cancel.store(true, Ordering::Relaxed);
            }
            return Ok(());
        }
        if self.conflict_key(k) {
            return Ok(());
        }
        if let Some(archive) = &self.archive {
            if k.code == K::Esc {
                archive.cancel.store(true, Ordering::Relaxed);
            }
            return Ok(());
        }
        if self.menu.is_some() {
            return self.menu_key(k, terminal);
        }
        if let Some(dialog) = self.dialog.take() {
            return self.dialog_key(dialog, k, terminal);
        }
        if self.search.is_some() {
            self.search_key(k);
            return Ok(());
        }
        if let Some(time) = self.escape.take()
            && time.elapsed() < Duration::from_secs(2)
        {
            if let K::Char(c @ '0'..='9') = k.code {
                k.code = K::F(if c == '0' { 10 } else { c as u8 - b'0' });
            } else {
                k.modifiers |= M::ALT;
            }
        }
        if k.code == K::Esc {
            self.escape = Some(Instant::now());
            self.quick.clear();
            return Ok(());
        }
        if !self.quick.is_empty() {
            if k.code == K::Char('s') && k.modifiers.intersects(M::CONTROL | M::ALT) {
                let query = format!("{}*", self.quick.trim_start_matches('\0').to_lowercase());
                let len = self.panel().entries.len();
                let cursor = self.panel().cursor;
                if len > 0 {
                    for step in 0..len {
                        let i = (cursor + step) % len;
                        if wildcard(&query, &self.panel().entries[i].name.to_lowercase()) {
                            self.panel_mut().cursor = i + 1;
                            break;
                        }
                    }
                }
                return Ok(());
            }
            match k.code {
                K::Char(c) if k.modifiers.is_empty() || k.modifiers == M::SHIFT => {
                    self.quick.push(c);
                    self.quick_match();
                    return Ok(());
                }
                K::Backspace => {
                    self.quick.pop();
                    self.quick_match();
                    return Ok(());
                }
                _ => self.quick.clear(),
            }
        }
        match (k.code, k.modifiers) {
            (K::F(n), _) => self.function(n, terminal)?,
            (K::Tab | K::BackTab | K::Left | K::Right, _) => self.active = 1 - self.active,
            (K::Up, _) | (K::Char('p'), M::CONTROL) => self.panel_mut().step(-1),
            (K::Down, _) | (K::Char('n'), M::CONTROL) => self.panel_mut().step(1),
            (K::Home, _) | (K::Char('<'), M::ALT) => self.panel_mut().cursor = 0,
            (K::End, _) | (K::Char('>'), M::ALT) => {
                self.panel_mut().cursor = self.panel().entries.len()
            }
            (K::PageUp, M::CONTROL) => self.panel_mut().parent(),
            (K::PageDown, M::CONTROL) => self.open(terminal)?,
            (K::PageUp, _) | (K::Char('v'), M::ALT) => {
                let step = self.areas[self.active].height.saturating_sub(3).max(1) as isize;
                self.panel_mut().step(-step);
            }
            (K::PageDown, _) | (K::Char('v'), M::CONTROL) => {
                let step = self.areas[self.active].height.saturating_sub(3).max(1) as isize;
                self.panel_mut().step(step);
            }
            (K::Enter, _) => self.open(terminal)?,
            (K::Char(' '), M::NONE | M::SHIFT) | (K::Insert, _) | (K::Char('t'), M::CONTROL) => {
                self.panel_mut().toggle();
                self.panel_mut().step(1);
            }
            (K::Char('+'), _) => self.input(
                Purpose::Select,
                "Select files (wildcard pattern)",
                "*".into(),
            ),
            (K::Char('-' | '\\'), M::NONE) => self.input(
                Purpose::Unselect,
                "Unselect files (wildcard pattern)",
                "*".into(),
            ),
            (K::Char('*'), _) => {
                let p = self.panel_mut();
                for e in &p.entries {
                    if !e.directory && !p.selected.remove(&e.path) {
                        p.selected.insert(e.path.clone());
                    }
                }
            }
            (K::Char('.'), M::ALT) => {
                let p = self.panel_mut();
                p.hidden = !p.hidden;
                p.refresh();
            }
            (K::Char('?'), m) if m.contains(M::ALT) => {
                self.input(Purpose::Search, "Find filename (substring)", String::new())
            }
            (K::Char('r'), M::CONTROL) => self.panel_mut().refresh(),
            (K::Char('u'), M::CONTROL) => self.panels.swap(0, 1),
            (K::Char('c'), M::ALT) => self.input(
                Purpose::Goto,
                "Go to directory",
                self.panel().path.display().to_string(),
            ),
            (K::Char('i'), M::ALT) => {
                let path = self.panel().path.clone();
                self.panels[1 - self.active].navigate(path);
            }
            (K::Char('g' | 'r' | 'j'), M::ALT) => {
                let visible = self.areas[self.active].height.saturating_sub(3).max(1) as usize;
                let offset = self.panel().offset;
                self.panel_mut().cursor = (offset
                    + match k.code {
                        K::Char('g') => 0,
                        K::Char('r') => visible / 2,
                        _ => visible - 1,
                    })
                .min(self.panel().entries.len());
            }
            (K::Char('o'), M::ALT) => {
                let path = self
                    .panel()
                    .current()
                    .filter(|e| e.directory)
                    .map(|e| e.path.clone())
                    .unwrap_or_else(|| self.panel().path.clone());
                self.panels[1 - self.active].navigate(path);
                self.panel_mut().step(1);
            }
            (K::Char('s'), M::CONTROL) | (K::Char('s'), M::ALT) => {
                self.quick = "\u{0}".into();
                self.status = "Quick search: type a filename prefix; Esc clears".into();
            }
            _ => {}
        }
        Ok(())
    }
    fn quick_match(&mut self) {
        let query = self.quick.trim_start_matches('\0').to_lowercase();
        if let Some(i) = self
            .panel()
            .entries
            .iter()
            .position(|e| wildcard(&format!("{query}*"), &e.name.to_lowercase()))
        {
            self.panel_mut().cursor = i + 1;
        }
        self.status = format!("Quick search: {query}");
    }
    fn menu_key(&mut self, key: KeyEvent, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let state = self.menu.as_mut().unwrap();
        let count = MENUS[state.category].items.len();
        match key.code {
            K::Esc | K::F(9) => self.menu = None,
            K::Left | K::BackTab => {
                *state = menu::State {
                    category: (state.category + MENUS.len() - 1) % MENUS.len(),
                    ..Default::default()
                }
            }
            K::Right | K::Tab => {
                *state = menu::State {
                    category: (state.category + 1) % MENUS.len(),
                    ..Default::default()
                }
            }
            K::Up => state.cursor = (state.cursor + count - 1) % count,
            K::Down => state.cursor = (state.cursor + 1) % count,
            K::Home => state.cursor = 0,
            K::End => state.cursor = count - 1,
            K::Enter => {
                let action = MENUS[state.category].items[state.cursor].action;
                self.menu = None;
                match action {
                    menu::Action::Function(n) => self.function(n, terminal)?,
                    menu::Action::Sort(sort) => {
                        let p = self.panel_mut();
                        p.sort = sort;
                        p.sort_entries();
                        p.cursor = 0;
                    }
                    menu::Action::Hidden => {
                        let p = self.panel_mut();
                        p.hidden = !p.hidden;
                        p.refresh();
                    }
                    menu::Action::Refresh => self.panel_mut().refresh(),
                    menu::Action::Goto => self.input(
                        Purpose::Goto,
                        "Go to directory",
                        self.panel().path.display().to_string(),
                    ),
                    menu::Action::Connect(scheme) => self.input(
                        Purpose::Goto,
                        if scheme == "ftp://" {
                            "FTP URL (unencrypted): user@host:port/path"
                        } else {
                            "Remote URL: user@host:port/path"
                        },
                        scheme.into(),
                    ),
                    menu::Action::Local => match std::env::current_dir() {
                        Ok(path) => self.panel_mut().navigate(path.into()),
                        Err(e) => self.message("Local directory", e.to_string()),
                    },
                    menu::Action::Find => {
                        self.input(Purpose::Search, "Find filename (substring)", String::new())
                    }
                    menu::Action::Jobs => self.dialog = Some(Dialog::Jobs),
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn dialog_key(
        &mut self,
        mut dialog: Dialog,
        k: KeyEvent,
        _terminal: &mut ratatui::DefaultTerminal,
    ) -> Result<()> {
        if k.code == K::Esc {
            return Ok(());
        }
        match &mut dialog {
            Dialog::Input {
                value,
                cursor,
                purpose,
                ..
            } => {
                if k.code == K::Enter {
                    self.submit(*purpose, value.clone());
                    return Ok(());
                }
                edit_input(value, cursor, k);
            }
            Dialog::Delete { permanent } => match k.code {
                K::Tab | K::Left | K::Right => *permanent = !*permanent,
                K::Enter => {
                    self.start(
                        if *permanent {
                            Operation::Delete
                        } else {
                            Operation::Trash
                        },
                        self.panel().path.clone(),
                    );
                    return Ok(());
                }
                _ => {}
            },
            Dialog::Message { .. } => {
                if k.code == K::Enter {
                    return Ok(());
                }
            }
            Dialog::Jobs => {
                if k.code == K::Char('c') {
                    for j in &self.jobs {
                        j.cancel.store(true, Ordering::Relaxed);
                    }
                }
                if k.code == K::Enter {
                    return Ok(());
                }
            }
            Dialog::Quit => {
                if k.code == K::Enter {
                    for j in &self.jobs {
                        j.cancel.store(true, Ordering::Relaxed);
                    }
                    self.status = "Cancelling jobs; quit again after they finish".into();
                    return Ok(());
                }
            }
        }
        self.dialog = Some(dialog);
        Ok(())
    }
    fn submit(&mut self, purpose: Purpose, value: String) {
        if matches!(purpose, Purpose::Goto | Purpose::Copy | Purpose::Move)
            && remote::is_url(&value)
        {
            // Reuse an existing authenticated mount when the edited label is inside it.
            let existing = self.panels.iter().find_map(|p| {
                if !p.path.fs.is_remote() {
                    return None;
                }
                let root = p.path.fs.label(std::path::Path::new("/"));
                if value.starts_with(&root) {
                    remote::Endpoint::parse(&value)
                        .ok()
                        .map(|e| VfsPath::new(p.path.fs.clone(), e.path))
                } else {
                    None
                }
            });
            self.resolve_path(value, existing, purpose);
            return;
        }
        let path = if matches!(purpose, Purpose::Copy | Purpose::Move)
            && value == self.panels[1 - self.active].path.display()
        {
            self.panels[1 - self.active].path.clone()
        } else if value == self.panel().path.display() {
            self.panel().path.clone()
        } else {
            let p = PathBuf::from(&value);
            if value.starts_with("file://") {
                match url::Url::parse(&value)
                    .ok()
                    .and_then(|u| u.to_file_path().ok())
                {
                    Some(path) => path.into(),
                    None => {
                        self.message("Directory", "Invalid local file:// URL");
                        return;
                    }
                }
            } else if p.is_absolute() && !self.panel().path.fs.is_remote() {
                p.into()
            } else {
                self.panel().path.join(p)
            }
        };
        match purpose {
            Purpose::Copy => self.start(Operation::Copy, path),
            Purpose::Move => self.start(Operation::Move, path),
            Purpose::Mkdir => {
                if !value.is_empty() {
                    self.start(Operation::Mkdir, path);
                }
            }
            Purpose::Goto => {
                self.resolve_path(value, Some(path), purpose);
            }
            Purpose::Search => {
                if !value.is_empty() {
                    self.begin_search(value);
                }
            }
            Purpose::Select | Purpose::Unselect => {
                let p = self.panel_mut();
                for e in &p.entries {
                    if !e.directory && wildcard(&value, &e.name) {
                        if matches!(purpose, Purpose::Select) {
                            p.selected.insert(e.path.clone());
                        } else {
                            p.selected.remove(&e.path);
                        }
                    }
                }
            }
        }
    }
    pub fn connect_url(&mut self, value: String, panel: usize) {
        if self.connecting.is_some() {
            self.pending_connections.push_back((value, panel));
            return;
        }
        self.resolve_path(value, None, Purpose::Goto);
        self.connecting.as_mut().unwrap().panel = panel;
    }
    fn resolve_path(&mut self, value: String, existing: Option<VfsPath>, purpose: Purpose) {
        let (tx, receiver) = mpsc::channel();
        let ctx = Context {
            auth: Some(self.auth_tx.clone()),
            ..Default::default()
        };
        let cancel = ctx.cancel.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let path = if let Some(path) = existing {
                    path
                } else {
                    remote::connect_path(&value, &ctx)?
                };
                if matches!(purpose, Purpose::Goto) {
                    anyhow::ensure!(
                        path.metadata(true, &ctx)?.kind == Kind::Directory,
                        "Not an accessible directory"
                    );
                }
                ctx.check()?;
                Ok(path)
            })();
            let _ = tx.send(result);
        });
        self.connecting = Some(PathTask {
            receiver,
            cancel,
            panel: self.active,
            purpose,
        });
    }
    fn begin_search(&mut self, query: String) {
        let results = Arc::new(Mutex::new(vec![]));
        let done = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let errors = Arc::new(Mutex::new(0));
        let r = results.clone();
        let d = done.clone();
        let c = cancel.clone();
        let e = errors.clone();
        let path = self.panel().path.clone();
        let ctx = Context {
            cancel: c.clone(),
            auth: Some(self.auth_tx.clone()),
            ..Default::default()
        };
        std::thread::spawn(move || {
            let query = query.to_lowercase();
            let mut pending = vec![path];
            'scan: while let Some(path) = pending.pop() {
                if ctx.check().is_err() {
                    break;
                }
                match path.read_dir(&ctx) {
                    Ok(entries) => {
                        for (path, meta) in entries {
                            if ctx.check().is_err() {
                                break 'scan;
                            }
                            if path
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .to_lowercase()
                                .contains(&query)
                            {
                                let mut results = r.lock().unwrap();
                                if results.len() >= 100_000 {
                                    break 'scan;
                                }
                                results.push(path.clone());
                            }
                            if meta.kind == Kind::Directory {
                                pending.push(path);
                            }
                        }
                    }
                    Err(_) => *e.lock().unwrap() += 1,
                }
            }
            d.store(true, Ordering::Relaxed);
        });
        self.search = Some(Search {
            results,
            done,
            cancel,
            errors,
            cursor: 0,
        });
    }
    fn search_key(&mut self, k: KeyEvent) {
        let search = self.search.as_mut().unwrap();
        match k.code {
            K::Esc => {
                search.cancel.store(true, Ordering::Relaxed);
                self.search = None;
            }
            K::Up => search.cursor = search.cursor.saturating_sub(1),
            K::Down => {
                search.cursor =
                    (search.cursor + 1).min(search.results.lock().unwrap().len().saturating_sub(1))
            }
            K::Char('c') if k.modifiers == M::CONTROL => {
                search.cancel.store(true, Ordering::Relaxed)
            }
            K::Enter => {
                let path = search.results.lock().unwrap().get(search.cursor).cloned();
                search.cancel.store(true, Ordering::Relaxed);
                self.search = None;
                if let Some(path) = path
                    && let Some(parent) = path.parent()
                {
                    self.panel_mut().navigate(parent.to_owned());
                    self.panel_mut().reveal = Some(path.clone());
                    self.status = format!("Found: {}", path.display());
                }
            }
            _ => {}
        }
    }
    fn mouse(
        &mut self,
        m: event::MouseEvent,
        terminal: &mut ratatui::DefaultTerminal,
    ) -> Result<()> {
        if self.password.is_some() || self.viewing.is_some() || self.connecting.is_some() {
            return Ok(());
        }
        let click = m.kind == MouseEventKind::Down(MouseButton::Left);
        let area = self.dialog_area;
        let inside = m.column >= area.x
            && m.column < area.right()
            && m.row >= area.y
            && m.row < area.bottom();
        if self
            .jobs
            .iter()
            .any(|j| j.progress.lock().unwrap().conflict.is_some())
        {
            if click && inside {
                let key = if m.row == area.y + 3 {
                    if m.column < area.x + 19 { 'o' } else { 'a' }
                } else if m.row == area.y + 4 {
                    if m.column < area.x + 19 {
                        's'
                    } else if m.column < area.x + 34 {
                        'n'
                    } else {
                        return self.key(KeyEvent::new(K::Esc, M::NONE), terminal);
                    }
                } else {
                    return Ok(());
                };
                return self.key(KeyEvent::new(K::Char(key), M::NONE), terminal);
            }
            return Ok(());
        }
        if self.menu.is_some() {
            if let Some(category) = self
                .menu_tabs
                .iter()
                .position(|r| r.contains((m.column, m.row).into()))
            {
                if click || m.kind == MouseEventKind::Moved {
                    let state = self.menu.as_mut().unwrap();
                    if category != state.category {
                        *state = menu::State {
                            category,
                            ..Default::default()
                        };
                    } else if click {
                        self.menu = None;
                    }
                }
            } else if self.menu_area.contains((m.column, m.row).into()) {
                let row = m.row.saturating_sub(self.menu_area.y + 1) as usize;
                let state = self.menu.as_mut().unwrap();
                if m.row > self.menu_area.y && m.row < self.menu_area.bottom() - 1 {
                    state.cursor = (state.offset + row).min(MENUS[state.category].items.len() - 1);
                    if click {
                        return self.menu_key(KeyEvent::new(K::Enter, M::NONE), terminal);
                    }
                }
                if m.kind == MouseEventKind::ScrollDown {
                    return self.menu_key(KeyEvent::new(K::Down, M::NONE), terminal);
                }
                if m.kind == MouseEventKind::ScrollUp {
                    return self.menu_key(KeyEvent::new(K::Up, M::NONE), terminal);
                }
            } else if click {
                self.menu = None;
            }
            return Ok(());
        }
        if self.dialog.is_some() {
            if click && inside {
                let relative_x = m.column - area.x;
                if m.row == area.bottom().saturating_sub(2) {
                    let code = match &self.dialog {
                        Some(Dialog::Message { .. }) => K::Esc,
                        Some(Dialog::Jobs) if relative_x < 17 => K::Char('c'),
                        Some(Dialog::Jobs) => K::Esc,
                        _ if relative_x < 16 => K::Enter,
                        _ => K::Esc,
                    };
                    return self.key(KeyEvent::new(code, M::NONE), terminal);
                }
                match self.dialog.as_mut().unwrap() {
                    Dialog::Delete { permanent } if m.row == area.y + 3 => {
                        *permanent = relative_x >= 22
                    }
                    _ => {}
                }
            }
            return Ok(());
        }
        if let Some(task) = &self.archive {
            if click && inside {
                task.cancel.store(true, Ordering::Relaxed);
            }
            return Ok(());
        }
        if let Some(search) = &mut self.search {
            match m.kind {
                MouseEventKind::ScrollUp => search.cursor = search.cursor.saturating_sub(3),
                MouseEventKind::ScrollDown => {
                    search.cursor = (search.cursor + 3)
                        .min(search.results.lock().unwrap().len().saturating_sub(1))
                }
                MouseEventKind::Down(MouseButton::Left) if inside && m.row >= area.y + 2 => {
                    let row = search.cursor.saturating_sub(8) + (m.row - area.y - 2) as usize;
                    if row < search.results.lock().unwrap().len() {
                        search.cursor = row;
                        let double = self.last_click.is_some_and(|(t, p, r)| {
                            t.elapsed() < Duration::from_millis(400) && p == 2 && r == row
                        });
                        self.last_click = Some((Instant::now(), 2, row));
                        if double {
                            self.search_key(KeyEvent::new(K::Enter, M::NONE));
                        }
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if click && m.row == 0 {
            if let Some(category) = self
                .menu_tabs
                .iter()
                .position(|r| r.contains((m.column, m.row).into()))
            {
                self.quick.clear();
                self.menu = Some(menu::State {
                    category,
                    ..Default::default()
                });
            }
            return Ok(());
        }
        if m.row == self.height.saturating_sub(1)
            && m.kind == MouseEventKind::Down(MouseButton::Left)
        {
            let n = ((m.column as u32 * 10 / self.width.max(1) as u32) + 1) as u8;
            return self.function(n.min(10), terminal);
        }
        for i in 0..2 {
            let area = self.areas[i];
            if m.column >= area.x
                && m.column < area.right()
                && m.row >= area.y
                && m.row < area.bottom()
            {
                self.active = i;
                match m.kind {
                    MouseEventKind::ScrollUp => self.panel_mut().step(-3),
                    MouseEventKind::ScrollDown => self.panel_mut().step(3),
                    MouseEventKind::Down(button)
                        if m.row >= area.y + 2 && m.row < area.bottom().saturating_sub(1) =>
                    {
                        let row = self.panels[i].offset + (m.row - area.y - 2) as usize;
                        if row <= self.panels[i].entries.len() {
                            self.panels[i].cursor = row;
                            if button == MouseButton::Right {
                                self.panel_mut().toggle();
                            } else if button == MouseButton::Left {
                                let double = self.last_click.is_some_and(|(t, p, r)| {
                                    t.elapsed() < Duration::from_millis(400) && p == i && r == row
                                });
                                self.last_click = Some((Instant::now(), i, row));
                                if double {
                                    self.open(terminal)?;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                break;
            }
        }
        Ok(())
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if let Some(task) = &self.connecting {
            task.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(v) = &self.viewing {
            v.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(s) = &self.search {
            s.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(a) = &self.archive {
            a.cancel.store(true, Ordering::Relaxed);
        }
        for j in &self.jobs {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
}
pub fn wildcard(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let mut dp = vec![false; n.len() + 1];
    dp[0] = true;
    for c in p {
        let mut next = vec![false; n.len() + 1];
        if c == '*' {
            next[0] = dp[0];
        }
        for j in 1..=n.len() {
            next[j] = if c == '*' {
                dp[j] || next[j - 1]
            } else {
                dp[j - 1] && (c == '?' || c == n[j - 1])
            };
        }
        dp = next;
    }
    dp[n.len()]
}

/// UTF-8 safe editing with the conventional MC/Emacs input bindings.
pub fn edit_input(value: &mut String, cursor: &mut usize, key: KeyEvent) {
    let before = value[..*cursor]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let after = value[*cursor..]
        .chars()
        .next()
        .map(|c| *cursor + c.len_utf8())
        .unwrap_or(*cursor);
    match (key.code, key.modifiers) {
        (K::Home, _) | (K::Char('a'), M::CONTROL) => *cursor = 0,
        (K::End, _) | (K::Char('e'), M::CONTROL) => *cursor = value.len(),
        (K::Left, _) | (K::Char('b'), M::CONTROL) => *cursor = before,
        (K::Right, _) | (K::Char('f'), M::CONTROL) => *cursor = after,
        (K::Backspace, _) | (K::Char('h'), M::CONTROL) => {
            value.replace_range(before..*cursor, "");
            *cursor = before;
        }
        (K::Delete, _) | (K::Char('d'), M::CONTROL) => {
            value.replace_range(*cursor..after, "");
        }
        (K::Char('k'), M::CONTROL) => value.truncate(*cursor),
        (K::Char('u'), M::CONTROL) => {
            value.clear();
            *cursor = 0;
        }
        (K::Char(c), m) if !m.intersects(M::CONTROL | M::ALT) => {
            value.insert(*cursor, c);
            *cursor += c.len_utf8();
        }
        _ => {}
    }
}
