use crate::vfs::{Context, Kind, VfsPath};
use anyhow::Result;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
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
    let mut pending = vec![(path.clone(), path.metadata(false, ctx))];
    while let Some((path, meta)) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match meta {
            Ok(m) if m.kind == Kind::Directory => match path.read_dir(ctx) {
                Ok(entries) => pending.extend(entries.into_iter().map(|(p, m)| (p, Ok(m)))),
                Err(_) => total.errors += 1,
            },
            Ok(m) => total.bytes = total.bytes.saturating_add(m.size),
            Err(_) => total.errors += 1,
        }
    }
    total
}
enum ListingEvent {
    Batch(Vec<Entry>),
    Done(Result<()>),
}
struct Listing {
    receiver: mpsc::Receiver<ListingEvent>,
    first: bool,
    cancel: Arc<AtomicBool>,
}
impl Drop for Listing {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
pub struct Panel {
    refreshed: Instant,
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
            refreshed: Instant::now(),
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
        if self.reveal.is_none() {
            self.reveal = self.current().map(|e| e.path.clone());
        }
        self.size_scan = None;
        self.directory_sizes.clear();
        let path = self.path.clone();
        let hidden = self.hidden;
        let (tx, rx) = mpsc::sync_channel(2);
        self.loading = true;
        self.error = None;
        let ctx = Context {
            cancel: Arc::new(AtomicBool::new(false)),
            ..self.context.clone()
        };
        self.refreshed = Instant::now();
        self.pending = Some(Listing {
            first: true,
            receiver: rx,
            cancel: ctx.cancel.clone(),
        });
        std::thread::spawn(move || {
            let result = (|| {
                let mut entries = vec![];
                path.fs.visit_dir(&path.path, &ctx, &mut |entry| {
                    ctx.check()?;
                    let m = entry.metadata;
                    let name = entry.name.to_string_lossy().into_owned();
                    anyhow::ensure!(
                        std::path::Path::new(&entry.name).components().count() == 1
                            && matches!(
                                std::path::Path::new(&entry.name).components().next(),
                                Some(std::path::Component::Normal(_))
                            ),
                        "Invalid directory entry name"
                    );
                    if !hidden && name.starts_with('.') {
                        return Ok(());
                    }
                    let path = path.join(entry.name);
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
                    if entries.len() >= 256 {
                        tx.send(ListingEvent::Batch(std::mem::take(&mut entries)))?;
                    }
                    Ok(())
                })?;
                if !entries.is_empty() {
                    tx.send(ListingEvent::Batch(entries))?;
                }
                Ok(())
            })();
            let _ = tx.send(ListingEvent::Done(result));
        });
    }
    pub fn auto_refresh(&mut self) {
        let interval = if self.path.fs.is_remote() { 15 } else { 3 };
        if (self.path.local_path().is_some() || self.path.fs.is_remote())
            && !self.loading
            && self.selected.is_empty()
            && self.refreshed.elapsed() > Duration::from_secs(interval)
        {
            self.refresh();
        }
    }
    pub fn poll(&mut self) {
        self.poll_sizes();
        let old = self
            .reveal
            .clone()
            .or_else(|| self.current().map(|e| e.path.clone()));
        let mut changed = false;
        for _ in 0..8 {
            let event = self
                .pending
                .as_ref()
                .and_then(|p| p.receiver.try_recv().ok());
            let Some(event) = event else {
                break;
            };
            if self.pending.as_ref().unwrap().first {
                self.entries.clear();
                self.pending.as_mut().unwrap().first = false;
                changed = true;
            }
            match event {
                ListingEvent::Batch(entries) => {
                    self.entries.extend(entries);
                    changed = true;
                }
                ListingEvent::Done(result) => {
                    self.pending = None;
                    self.loading = false;
                    self.refreshed = Instant::now();
                    self.error = result.err().map(|e| e.to_string());
                    let paths = self.entries.iter().map(|e| &e.path).collect::<HashSet<_>>();
                    self.selected.retain(|p| paths.contains(p));
                    break;
                }
            }
        }
        if changed {
            self.sort_entries();
            if let Some(i) = old.and_then(|p| self.entries.iter().position(|e| e.path == p)) {
                self.cursor = i + 1;
                self.reveal = None;
            } else {
                self.cursor = self.cursor.min(self.entries.len());
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
        match self.sort {
            Sort::Name => self
                .entries
                .sort_by_cached_key(|e| (!e.directory, e.name.to_lowercase(), e.path.path.clone())),
            Sort::Size => self.entries.sort_by_cached_key(|e| {
                (!e.directory, std::cmp::Reverse(e.size), e.path.path.clone())
            }),
            Sort::Modified => self.entries.sort_by_cached_key(|e| {
                (
                    !e.directory,
                    std::cmp::Reverse(e.modified),
                    e.path.path.clone(),
                )
            }),
        }
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
        self.reveal = None;
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
