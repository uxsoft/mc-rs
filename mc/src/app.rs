use crate::{
    archives, external,
    jobs::{self, Decision, Job, Operation},
    panel::{Mount, Panel, Sort},
    ui,
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
    Menu {
        cursor: usize,
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
    pub results: Arc<Mutex<Vec<PathBuf>>>,
    pub done: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub errors: Arc<Mutex<usize>>,
    pub cursor: usize,
}
pub struct ArchiveTask {
    pub receiver: mpsc::Receiver<Result<Arc<Mount>>>,
    pub cancel: Arc<AtomicBool>,
    pub bytes: Arc<Mutex<u64>>,
    pub panel: usize,
}
pub struct App {
    pub panels: [Panel; 2],
    pub active: usize,
    pub dialog: Option<Dialog>,
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
        Self {
            panels: [Panel::new(left), Panel::new(right)],
            active: 0,
            dialog: None,
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
        for p in &mut self.panels {
            p.poll();
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
                    p.navigate(mount.temp.path().to_owned());
                    p.mount = Some(mount);
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
    fn start(&mut self, op: Operation, destination: PathBuf) {
        if self.panel().mount.is_some() && op != Operation::Copy {
            self.message(
                "Read-only archive",
                "Archive entries can be viewed and copied out. Modification is unavailable.",
            );
            return;
        }
        if self.panels[1 - self.active].mount.is_some()
            && matches!(op, Operation::Copy | Operation::Move)
        {
            self.message(
                "Read-only destination",
                "Leave the archive in the destination panel first.",
            );
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
            self.panel().mount.clone(),
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
            } else if archives::supported(&entry.path) && self.panel().mount.is_none() {
                let (tx, rx) = mpsc::channel();
                let cancel = Arc::new(AtomicBool::new(false));
                let bytes = Arc::new(Mutex::new(0));
                let c = cancel.clone();
                let b = bytes.clone();
                std::thread::spawn(move || {
                    let result = archives::open(entry.path, &c, |n| *b.lock().unwrap() = n);
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
        if edit && self.panel().mount.is_some() {
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
            if let Err(e) = external::launch(&path, edit, terminal) {
                self.message("External program", e.to_string());
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
            9 => self.dialog = Some(Dialog::Menu { cursor: 0 }),
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
        if self.conflict_key(k) {
            return Ok(());
        }
        if let Some(archive) = &self.archive {
            if k.code == K::Esc {
                archive.cancel.store(true, Ordering::Relaxed);
            }
            return Ok(());
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
            (K::Char('l'), M::CONTROL) => terminal.clear()?,
            (K::Char('i'), M::ALT) => {
                let path = self.panel().path.clone();
                let mount = self.panel().mount.clone();
                self.panels[1 - self.active].mount = mount;
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
                let mount = self.panel().mount.clone();
                self.panels[1 - self.active].mount = mount;
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
                        PathBuf::new(),
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
            Dialog::Menu { cursor } => match k.code {
                K::Up => *cursor = cursor.saturating_sub(1),
                K::Down => *cursor = (*cursor + 1).min(6),
                K::Enter => {
                    match *cursor {
                        0 => self.input(
                            Purpose::Goto,
                            "Go to directory",
                            self.panel().path.display().to_string(),
                        ),
                        1 => {
                            self.input(Purpose::Search, "Find filename (substring)", String::new())
                        }
                        2..=4 => {
                            let p = self.panel_mut();
                            p.sort = match *cursor {
                                2 => Sort::Name,
                                3 => Sort::Size,
                                _ => Sort::Modified,
                            };
                            p.sort_entries();
                            p.cursor = 0;
                        }
                        5 => self.dialog = Some(Dialog::Jobs),
                        _ => {
                            let p = self.panel_mut();
                            p.hidden = !p.hidden;
                            p.refresh();
                        }
                    }
                    return Ok(());
                }
                _ => {}
            },
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
        let path = if matches!(purpose, Purpose::Copy | Purpose::Move)
            && value == self.panels[1 - self.active].path.display().to_string()
        {
            self.panels[1 - self.active].path.clone()
        } else {
            let p = PathBuf::from(&value);
            if p.is_absolute() {
                p
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
                if path.is_dir() {
                    self.panel_mut().mount = None;
                    self.panel_mut().navigate(path);
                } else {
                    self.message(
                        "Directory",
                        "Directory does not exist or cannot be accessed",
                    );
                }
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
        let mount = self.panel().mount.clone();
        std::thread::spawn(move || {
            let _mount = mount;
            let query = query.to_lowercase();
            for item in walkdir::WalkDir::new(path).follow_links(false).min_depth(1) {
                if c.load(Ordering::Relaxed) {
                    break;
                }
                match item {
                    Ok(item) => {
                        if item
                            .file_name()
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&query)
                        {
                            let mut results = r.lock().unwrap();
                            if results.len() >= 100_000 {
                                break;
                            }
                            results.push(item.path().to_owned());
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
        if self.dialog.is_some() {
            if click && inside {
                let relative_x = m.column - area.x;
                if m.row == area.bottom().saturating_sub(2) {
                    let code = match &self.dialog {
                        Some(Dialog::Message { .. } | Dialog::Menu { .. }) => K::Esc,
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
                    Dialog::Menu { cursor } if m.row > area.y && m.row < area.y + 8 => {
                        *cursor = (m.row - area.y - 1) as usize;
                        return self.key(KeyEvent::new(K::Enter, M::NONE), terminal);
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
            return self.function(9, terminal);
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
