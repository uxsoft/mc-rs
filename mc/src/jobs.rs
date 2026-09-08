use crate::panel::Mount;
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Operation {
    Copy,
    Move,
    Trash,
    Delete,
    Mkdir,
}
#[derive(Clone, Copy, Debug)]
pub enum Decision {
    Overwrite,
    Skip,
    OverwriteAll,
    SkipAll,
    Cancel,
}
pub struct Conflict {
    pub path: PathBuf,
    pub reply: mpsc::Sender<Decision>,
}
#[derive(Default)]
pub struct Progress {
    pub bytes: u64,
    pub files: u64,
    pub current: String,
    pub done: bool,
    pub error: Option<String>,
    pub conflict: Option<Conflict>,
}
pub struct Job {
    pub resources: Vec<PathBuf>,
    pub title: String,
    pub progress: Arc<Mutex<Progress>>,
    pub cancel: Arc<AtomicBool>,
}
struct Worker {
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    policy: Option<Decision>,
}
impl Worker {
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            bail!("Cancelled; completed items were kept");
        }
        Ok(())
    }
    fn overwrite(&mut self, path: &Path) -> Result<bool> {
        self.check()?;
        if fs::symlink_metadata(path).is_err() {
            return Ok(false);
        }
        let decision = if let Some(d) = self.policy {
            d
        } else {
            let (tx, rx) = mpsc::channel();
            self.progress.lock().unwrap().conflict = Some(Conflict {
                path: path.to_owned(),
                reply: tx,
            });
            loop {
                self.check()?;
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(d) => break d,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => bail!("Conflict dialog closed"),
                }
            }
        };
        match decision {
            Decision::OverwriteAll => {
                self.policy = Some(decision);
                Ok(true)
            }
            Decision::Overwrite => Ok(true),
            Decision::Skip | Decision::SkipAll => {
                if matches!(decision, Decision::SkipAll) {
                    self.policy = Some(decision);
                }
                bail!("SKIP")
            }
            Decision::Cancel => bail!("Cancelled"),
        }
    }
    fn copy(&mut self, from: &Path, to: &Path) -> Result<bool> {
        self.check()?;
        self.progress.lock().unwrap().current = from.display().to_string();
        let meta = fs::symlink_metadata(from)?;
        if meta.is_dir() {
            if let Ok(dest) = fs::symlink_metadata(to) {
                if !dest.is_dir() || dest.is_symlink() {
                    bail!("Destination is not a plain directory: {}", to.display());
                }
            } else {
                fs::create_dir(to)?;
            }
            let mut complete = true;
            for item in fs::read_dir(from)? {
                let item = item?;
                complete &= self.copy(&item.path(), &to.join(item.file_name()))?;
            }
            fs::set_permissions(to, meta.permissions())?;
            return Ok(complete);
        }
        if !meta.is_file() && !meta.is_symlink() {
            bail!("Unsupported special file: {}", from.display());
        }
        let overwrite = match self.overwrite(to) {
            Ok(v) => v,
            Err(e) if e.to_string() == "SKIP" => return Ok(false),
            Err(e) => return Err(e),
        };
        if overwrite && fs::symlink_metadata(to)?.is_dir() {
            bail!("Cannot overwrite a directory with a file");
        }
        let parent = to.parent().context("No destination parent")?;
        if meta.is_symlink() {
            // Stage links in an owned directory; never follow or truncate a destination link.
            let staging = tempfile::tempdir_in(parent)?;
            let staged = staging.path().join("link");
            let target = fs::read_link(from)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &staged)?;
            #[cfg(windows)]
            {
                if from.is_dir() {
                    std::os::windows::fs::symlink_dir(target, &staged)?;
                } else {
                    std::os::windows::fs::symlink_file(target, &staged)?;
                }
            }
            self.check()?;
            if overwrite {
                fs::rename(&staged, to)?;
            } else {
                rename_noreplace(&staged, to)?;
            }
        } else {
            let mut input = fs::File::open(from)?;
            let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
            let mut buf = [0; 128 * 1024];
            loop {
                self.check()?;
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                tmp.write_all(&buf[..n])?;
                self.progress.lock().unwrap().bytes += n as u64;
            }
            tmp.as_file().set_permissions(meta.permissions())?;
            tmp.as_file().sync_all()?;
            self.check()?;
            if overwrite {
                tmp.persist(to)?;
            } else {
                tmp.persist_noclobber(to)?;
            }
        }
        self.progress.lock().unwrap().files += 1;
        Ok(true)
    }
}
pub fn validate_destination(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from)?;
    let source = if meta.is_symlink() {
        fs::canonicalize(from.parent().context("No source parent")?)?
            .join(from.file_name().context("No source name")?)
    } else {
        fs::canonicalize(from)?
    };
    let destination = if fs::symlink_metadata(to).is_ok_and(|m| m.is_symlink()) {
        fs::canonicalize(to.parent().context("No destination parent")?)?
            .join(to.file_name().context("No destination name")?)
    } else if to.exists() {
        fs::canonicalize(to)?
    } else {
        fs::canonicalize(to.parent().context("Missing parent")?)?
            .join(to.file_name().context("Missing name")?)
    };
    if source == destination || (meta.is_dir() && destination.starts_with(&source)) {
        bail!("Destination is the source or inside it");
    }
    #[cfg(unix)]
    if let (Ok(a), Ok(b)) = (fs::symlink_metadata(from), fs::symlink_metadata(to)) {
        use std::os::unix::fs::MetadataExt;
        if a.dev() == b.dev() && a.ino() == b.ino() {
            bail!("Source and destination are the same file");
        }
    }
    Ok(())
}
pub fn start(
    op: Operation,
    sources: Vec<PathBuf>,
    destination: PathBuf,
    mount: Option<Arc<Mount>>,
) -> Job {
    let progress = Arc::new(Mutex::new(Progress::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let job = Job {
        resources: resources(op, &sources, &destination),
        title: format!("{op:?}"),
        progress: progress.clone(),
        cancel: cancel.clone(),
    };
    std::thread::spawn(move || {
        let _mount = mount;
        let mut worker = Worker {
            progress: progress.clone(),
            cancel,
            policy: None,
        };
        let result: Result<()> = (|| {
            if op == Operation::Mkdir {
                fs::create_dir(&destination)?;
                return Ok(());
            }
            for source in &sources {
                worker.check()?;
                progress.lock().unwrap().current = source.display().to_string();
                match op {
                    Operation::Trash => {
                        trash::delete(source)?;
                        progress.lock().unwrap().files += 1;
                    }
                    Operation::Delete => {
                        remove(source, &worker)?;
                    }
                    Operation::Copy | Operation::Move => {
                        let target = if destination.is_dir() {
                            destination.join(source.file_name().context("Cannot operate on root")?)
                        } else {
                            if sources.len() > 1 {
                                bail!("Multiple sources require an existing destination directory");
                            }
                            destination.clone()
                        };
                        validate_destination(source, &target)?;
                        if op == Operation::Move
                            && fs::symlink_metadata(&target).is_err()
                            && rename_noreplace(source, &target).is_ok()
                        {
                            progress.lock().unwrap().files += 1;
                            continue;
                        }
                        // Cross-device moves and merges copy first; incomplete copies never delete source data.
                        if worker.copy(source, &target)? && op == Operation::Move {
                            remove(source, &worker)?;
                        }
                    }
                    Operation::Mkdir => unreachable!(),
                }
            }
            Ok(())
        })();
        let mut p = progress.lock().unwrap();
        p.done = true;
        p.conflict = None;
        p.error = result.err().map(|e| format!("{e:#}"));
    });
    job
}
fn remove(path: &Path, worker: &Worker) -> Result<()> {
    worker.check()?;
    if fs::symlink_metadata(path)?.is_dir() {
        for item in fs::read_dir(path)? {
            remove(&item?.path(), worker)?;
        }
        fs::remove_dir(path)?;
    } else {
        fs::remove_file(path)?;
    }
    worker.progress.lock().unwrap().files += 1;
    Ok(())
}

fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            from,
            rustix::fs::CWD,
            to,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    }
    #[cfg(windows)]
    {
        fs::rename(from, to)
    }
}

/// Conservative path locks include sources and actual targets, allowing unrelated jobs together.
pub fn resources(op: Operation, sources: &[PathBuf], destination: &Path) -> Vec<PathBuf> {
    let mut paths = sources.to_vec();
    if matches!(op, Operation::Copy | Operation::Move) && destination.is_dir() {
        paths.extend(
            sources
                .iter()
                .filter_map(|p| p.file_name())
                .map(|name| destination.join(name)),
        );
    } else if matches!(op, Operation::Copy | Operation::Move | Operation::Mkdir) {
        paths.push(destination.to_owned());
    }
    paths
        .into_iter()
        .map(|p| {
            let mut ancestor = p.as_path();
            let mut suffix = Vec::new();
            while !ancestor.exists() {
                if let Some(name) = ancestor.file_name() {
                    suffix.push(name.to_owned());
                }
                if let Some(parent) = ancestor.parent() {
                    ancestor = parent;
                } else {
                    break;
                }
            }
            let mut canonical = fs::canonicalize(ancestor).unwrap_or_else(|_| ancestor.to_owned());
            for name in suffix.into_iter().rev() {
                canonical.push(name);
            }
            canonical
        })
        .collect()
}
pub fn overlaps(a: &[PathBuf], b: &[PathBuf]) -> bool {
    a.iter()
        .any(|a| b.iter().any(|b| a.starts_with(b) || b.starts_with(a)))
}
