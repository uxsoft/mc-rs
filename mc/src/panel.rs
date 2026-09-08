use anyhow::Result;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::SystemTime,
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
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
pub struct Mount {
    pub temp: tempfile::TempDir,
    pub source: PathBuf,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct DirectorySize {
    pub bytes: u64,
    pub errors: usize,
}
struct SizeScan {
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<DirectorySize>,
}
impl Drop for SizeScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
/// Logical bytes, including hidden files and link lengths, without following symlinks.
pub fn directory_size(path: &std::path::Path, cancel: &AtomicBool) -> DirectorySize {
    let mut total = DirectorySize::default();
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .follow_root_links(false)
    {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match entry {
            Ok(entry) if !entry.file_type().is_dir() => {
                match std::fs::symlink_metadata(entry.path()) {
                    Ok(meta) => total.bytes = total.bytes.saturating_add(meta.len()),
                    Err(_) => total.errors += 1,
                }
            }
            Ok(_) => {}
            Err(_) => total.errors += 1,
        }
    }
    total
}
pub struct Panel {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    pub offset: usize,
    pub selected: HashSet<PathBuf>,
    pub hidden: bool,
    pub sort: Sort,
    pub loading: bool,
    pub error: Option<String>,
    pub mount: Option<Arc<Mount>>,
    pending: Option<mpsc::Receiver<Result<Vec<Entry>>>>,
    pub reveal: Option<PathBuf>,
    pub directory_sizes: HashMap<PathBuf, DirectorySize>,
    size_scan: Option<SizeScan>,
}
impl Panel {
    pub fn new(path: PathBuf) -> Self {
        let mut p = Self {
            path,
            entries: vec![],
            cursor: 0,
            offset: 0,
            selected: HashSet::new(),
            hidden: true,
            sort: Sort::Name,
            loading: false,
            error: None,
            mount: None,
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
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send((|| {
                let mut entries = vec![];
                for item in std::fs::read_dir(path)? {
                    let item = item?;
                    let path = item.path();
                    let name = item.file_name().to_string_lossy().into_owned();
                    if !hidden && name.starts_with('.') {
                        continue;
                    }
                    let m = std::fs::symlink_metadata(&path)?;
                    entries.push(Entry {
                        directory: m.is_dir() || (m.is_symlink() && path.is_dir()),
                        link: m.is_symlink(),
                        size: m.len(),
                        modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
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
        let result = self.pending.as_ref().and_then(|rx| rx.try_recv().ok());
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
                let mount = self.mount.clone();
                std::thread::spawn(move || {
                    let _mount = mount;
                    let size = directory_size(&worker_path, &worker_cancel);
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
    pub fn sources(&self) -> Vec<PathBuf> {
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
    pub fn navigate(&mut self, path: PathBuf) {
        self.path = path;
        self.cursor = 0;
        self.offset = 0;
        self.entries.clear();
        self.selected.clear();
        self.refresh();
    }
    pub fn parent(&mut self) {
        if let Some(m) = &self.mount
            && self.path == m.temp.path()
        {
            let source = m.source.clone();
            self.mount = None;
            self.navigate(source.parent().unwrap_or(&source).to_owned());
            return;
        }
        if let Some(parent) = self.path.parent() {
            self.navigate(parent.to_owned());
        }
    }
    pub fn label(&self) -> String {
        if let Some(m) = &self.mount {
            format!(
                "{}!{}",
                m.source.display(),
                self.path
                    .strip_prefix(m.temp.path())
                    .unwrap_or(&self.path)
                    .display()
            )
        } else {
            self.path.display().to_string()
        }
    }
}
