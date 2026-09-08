use crate::vfs::{Context, Kind, VfsPath};
use anyhow::Result;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::SystemTime,
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: VfsPath,
    pub name: String,
    pub directory: bool,
    pub link: bool,
    pub size: u64,
    pub modified: SystemTime,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Sort {
    #[default]
    Name,
    Size,
    Modified,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct DirectorySize {
    pub bytes: u64,
    pub errors: usize,
}
struct SizeScan {
    path: VfsPath,
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<DirectorySize>,
}
impl Drop for SizeScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
/// Logical bytes, including hidden files and link lengths, without following symlinks.
pub fn directory_size(path: &VfsPath, cancel: &AtomicBool) -> DirectorySize {
    scan_size(path, cancel, &Context::default())
}
fn scan_size(path: &VfsPath, cancel: &AtomicBool, ctx: &Context) -> DirectorySize {
    let mut total = DirectorySize::default();
    let mut pending = vec![path.clone()];
    while let Some(path) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match path.metadata(false, ctx) {
            Ok(m) if m.kind == Kind::Directory => match path.read_dir(ctx) {
                Ok(entries) => pending.extend(entries.into_iter().map(|(p, _)| p)),
                Err(_) => total.errors += 1,
            },
            Ok(m) => total.bytes = total.bytes.saturating_add(m.size),
            Err(_) => total.errors += 1,
        }
    }
    total
}
struct Listing {
    receiver: mpsc::Receiver<Result<Vec<Entry>>>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Listing {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
pub struct Panel {
    context: Context,
    pub path: VfsPath,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    pub offset: usize,
    pub selected: HashSet<VfsPath>,
    pub hidden: bool,
    pub sort: Sort,
    pub loading: bool,
    pub error: Option<String>,
    pending: Option<Listing>,
    pub reveal: Option<VfsPath>,
    pub directory_sizes: HashMap<VfsPath, DirectorySize>,
    size_scan: Option<SizeScan>,
}
impl Panel {
    pub fn new(path: impl Into<VfsPath>) -> Self {
        Self::with_context(path, Context::default())
    }
    pub fn with_context(path: impl Into<VfsPath>, context: Context) -> Self {
        let mut p = Self {
            context,
            path: path.into(),
            entries: vec![],
            cursor: 0,
            offset: 0,
            selected: HashSet::new(),
            hidden: true,
            sort: Sort::Name,
            loading: false,
            error: None,
            pending: None,
            reveal: None,
            directory_sizes: HashMap::new(),
            size_scan: None,
        };
        p.refresh();
        p
    }
    pub fn refresh(&mut self) {
        self.size_scan = None;
        self.directory_sizes.clear();
        let path = self.path.clone();
        let hidden = self.hidden;
        let (tx, rx) = mpsc::channel();
        self.loading = true;
        self.error = None;
        let ctx = Context {
            cancel: Arc::new(AtomicBool::new(false)),
            ..self.context.clone()
        };
        self.pending = Some(Listing {
            receiver: rx,
            cancel: ctx.cancel.clone(),
        });
        std::thread::spawn(move || {
            let _ = tx.send((|| {
                let mut entries = vec![];
                for (path, m) in path.read_dir(&ctx)? {
                    let name = path.file_name().unwrap().to_string_lossy().into_owned();
                    if !hidden && name.starts_with('.') {
                        continue;
                    }
                    entries.push(Entry {
                        directory: m.kind == Kind::Directory
                            || (m.kind == Kind::Symlink
                                && path
                                    .metadata(true, &ctx)
                                    .is_ok_and(|m| m.kind == Kind::Directory)),
                        link: m.kind == Kind::Symlink,
                        size: m.size,
                        modified: m.modified,
                        name,
                        path,
                    });
                }
                Ok(entries)
            })());
        });
    }
    pub fn poll(&mut self) {
        self.poll_sizes();
        let result = self
            .pending
            .as_ref()
            .and_then(|listing| listing.receiver.try_recv().ok());
        if let Some(result) = result {
            self.pending = None;
            self.loading = false;
            match result {
                Ok(entries) => {
                    let old = self
                        .reveal
                        .take()
                        .or_else(|| self.current().map(|e| e.path.clone()));
                    self.entries = entries;
                    self.sort_entries();
                    self.selected
                        .retain(|p| self.entries.iter().any(|e| &e.path == p));
                    self.cursor = old
                        .and_then(|p| self.entries.iter().position(|e| e.path == p).map(|i| i + 1))
                        .unwrap_or(self.cursor.min(self.entries.len()));
                }
                Err(e) => {
                    self.entries.clear();
                    self.cursor = 0;
                    self.selected.clear();
                    self.error = Some(e.to_string());
                }
            }
        }
    }
    /// Keep at most one directory traversal running per panel.
    pub fn poll_sizes(&mut self) {
        if self
            .size_scan
            .as_ref()
            .is_some_and(|scan| !self.selected.contains(&scan.path))
        {
            self.size_scan = None;
        }
        if let Some(result) = self
            .size_scan
            .as_ref()
            .and_then(|scan| scan.receiver.try_recv().ok())
        {
            let scan = self.size_scan.take().unwrap();
            self.directory_sizes.insert(scan.path.clone(), result);
        }
        if self.size_scan.is_none() && !self.loading {
            let path = self
                .entries
                .iter()
                .find(|entry| {
                    entry.directory
                        && !entry.link
                        && self.selected.contains(&entry.path)
                        && !self.directory_sizes.contains_key(&entry.path)
                })
                .map(|entry| entry.path.clone());
            if let Some(path) = path {
                let (tx, receiver) = mpsc::channel();
                let cancel = Arc::new(AtomicBool::new(false));
                let worker_cancel = cancel.clone();
                let worker_path = path.clone();
                let ctx = Context {
                    cancel: cancel.clone(),
                    ..self.context.clone()
                };
                std::thread::spawn(move || {
                    let size = scan_size(&worker_path, &worker_cancel, &ctx);
                    if !worker_cancel.load(Ordering::Relaxed) {
                        let _ = tx.send(size);
                    }
                });
                self.size_scan = Some(SizeScan {
                    path,
                    cancel,
                    receiver,
                });
            }
        }
    }
    /// Selected byte total, outstanding directory calculations, and unreadable entries.
    pub fn selection_size(&self) -> (u64, usize, usize) {
        let mut total = (0u64, 0, 0);
        for entry in self
            .entries
            .iter()
            .filter(|e| self.selected.contains(&e.path))
        {
            if entry.directory && !entry.link {
                if let Some(size) = self.directory_sizes.get(&entry.path) {
                    total.0 = total.0.saturating_add(size.bytes);
                    total.2 += size.errors;
                } else {
                    total.1 += 1;
                }
            } else {
                total.0 = total.0.saturating_add(entry.size);
            }
        }
        total
    }
    pub fn sort_entries(&mut self) {
        self.entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| match self.sort {
                    Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    Sort::Size => b.size.cmp(&a.size),
                    Sort::Modified => b.modified.cmp(&a.modified),
                })
                .then_with(|| a.path.cmp(&b.path))
        });
    }
    pub fn current(&self) -> Option<&Entry> {
        self.cursor.checked_sub(1).and_then(|i| self.entries.get(i))
    }
    pub fn sources(&self) -> Vec<VfsPath> {
        if self.selected.is_empty() {
            self.current()
                .map(|e| vec![e.path.clone()])
                .unwrap_or_default()
        } else {
            self.entries
                .iter()
                .filter(|e| self.selected.contains(&e.path))
                .map(|e| e.path.clone())
                .collect()
        }
    }
    pub fn step(&mut self, delta: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.entries.len());
    }
    pub fn toggle(&mut self) {
        if let Some(path) = self.current().map(|e| e.path.clone())
            && !self.selected.remove(&path)
        {
            self.directory_sizes.remove(&path);
            self.selected.insert(path);
        }
    }
    pub fn navigate(&mut self, path: VfsPath) {
        self.path = path;
        self.cursor = 0;
        self.offset = 0;
        self.entries.clear();
        self.selected.clear();
        self.refresh();
    }
    pub fn parent(&mut self) {
        if let Some(parent) = self.path.parent() {
            self.navigate(parent);
        }
    }
    pub fn label(&self) -> String {
        self.path.display()
    }
}
