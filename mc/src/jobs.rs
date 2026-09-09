use crate::vfs::{Context as VfsContext, Kind, VfsPath, copy_stream};
use anyhow::{Context, Result, bail};
use std::{
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
    pub path: VfsPath,
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
    pub resources: Vec<VfsPath>,
    pub title: String,
    pub progress: Arc<Mutex<Progress>>,
    pub cancel: Arc<AtomicBool>,
}
struct Worker {
    progress: Arc<Mutex<Progress>>,
    ctx: VfsContext,
    policy: Option<Decision>,
}
fn lookup(path: &VfsPath, ctx: &VfsContext) -> Result<Option<crate::vfs::Metadata>> {
    match path.metadata(false, ctx) {
        Ok(meta) => Ok(Some(meta)),
        Err(e)
            if !path.fs.is_remote()
                || e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
impl Worker {
    fn check(&self) -> Result<()> {
        if self.ctx.cancel.load(Ordering::Relaxed) {
            bail!("Cancelled; completed items were kept");
        }
        Ok(())
    }
    fn overwrite(&mut self, path: &VfsPath) -> Result<bool> {
        self.check()?;
        if lookup(path, &self.ctx)?.is_none() {
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
    fn copy(&mut self, from: &VfsPath, to: &VfsPath) -> Result<bool> {
        self.check()?;
        self.progress.lock().unwrap().current = from.display().to_string();
        let meta = from.metadata(false, &self.ctx)?;
        if meta.kind == Kind::Directory {
            if let Some(dest) = lookup(to, &self.ctx)? {
                if dest.kind != Kind::Directory {
                    bail!("Destination is not a plain directory: {to}");
                }
            } else {
                to.fs.mkdir(&to.path, &self.ctx)?;
            }
            let mut complete = true;
            for (child, _) in from.read_dir(&self.ctx)? {
                complete &= self.copy(&child, &to.join(child.file_name().unwrap()))?;
            }
            to.fs.set_metadata(&to.path, &meta, &self.ctx)?;
            return Ok(complete);
        }
        if !matches!(meta.kind, Kind::File | Kind::Symlink) {
            bail!("Unsupported special file: {from}");
        }
        let overwrite = match self.overwrite(to) {
            Ok(v) => v,
            Err(e) if e.to_string() == "SKIP" => return Ok(false),
            Err(e) => return Err(e),
        };
        if overwrite && to.metadata(false, &self.ctx)?.kind == Kind::Directory {
            bail!("Cannot overwrite a directory with a file");
        }
        if meta.kind == Kind::Symlink {
            let target = from.fs.read_link(&from.path, &self.ctx)?;
            self.check()?;
            to.fs
                .symlink(&target, &to.path, from.is_dir(), overwrite, &self.ctx)?;
        } else {
            let mut input = from.fs.open_read(&from.path, &self.ctx)?;
            let mut output = to.fs.create(&to.path, overwrite, &self.ctx)?;
            copy_stream(&mut input, &mut output, &self.ctx, |n| {
                self.progress.lock().unwrap().bytes += n
            })?;
            self.check()?;
            output.commit(&meta)?;
        }
        self.progress.lock().unwrap().files += 1;
        Ok(true)
    }
}
pub fn validate_destination(from: &VfsPath, to: &VfsPath) -> Result<()> {
    if from.fs.id() != to.fs.id() {
        return Ok(());
    }
    let ctx = VfsContext::default();
    let meta = from.metadata(false, &ctx)?;
    fn canonical(path: &VfsPath, ctx: &VfsContext) -> Result<std::path::PathBuf> {
        if path
            .metadata(false, ctx)
            .is_ok_and(|m| m.kind == Kind::Symlink)
        {
            Ok(path
                .fs
                .canonical(path.path.parent().context("No parent")?)?
                .join(path.file_name().context("No name")?))
        } else {
            path.fs.canonical(&path.path)
        }
    }
    let source = canonical(from, &ctx)?;
    let destination = canonical(to, &ctx)?;
    if source == destination || (meta.kind == Kind::Directory && destination.starts_with(&source)) {
        bail!("Destination is the source or inside it");
    }
    if from.fs.same_file(&from.path, &to.path) {
        bail!("Source and destination are the same file");
    }
    Ok(())
}
pub fn start(
    op: Operation,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    mut ctx: VfsContext,
) -> Job {
    let progress = Arc::new(Mutex::new(Progress::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    ctx.cancel = cancel.clone();
    let job = Job {
        resources: resources(op, &sources, &destination),
        title: format!("{op:?}"),
        progress: progress.clone(),
        cancel: cancel.clone(),
    };
    std::thread::spawn(move || {
        let mut worker = Worker {
            progress: progress.clone(),
            ctx,
            policy: None,
        };
        let result: Result<()> = (|| {
            if matches!(op, Operation::Copy | Operation::Move | Operation::Mkdir)
                && !destination.fs.capabilities().write
            {
                bail!("Read-only destination");
            }
            if op == Operation::Mkdir {
                destination.fs.mkdir(&destination.path, &worker.ctx)?;
                return Ok(());
            }
            for source in &sources {
                if op != Operation::Copy && !source.fs.capabilities().write {
                    bail!("Read-only source");
                }
                worker.check()?;
                progress.lock().unwrap().current = source.display().to_string();
                match op {
                    Operation::Trash => {
                        source.fs.trash(&source.path, &worker.ctx)?;
                        progress.lock().unwrap().files += 1;
                    }
                    Operation::Delete => {
                        remove(source, &worker)?;
                    }
                    Operation::Copy | Operation::Move => {
                        let target = if lookup(&destination, &worker.ctx)?
                            .is_some_and(|m| m.kind == Kind::Directory)
                        {
                            destination.join(source.file_name().context("Cannot operate on root")?)
                        } else {
                            if sources.len() > 1 {
                                bail!("Multiple sources require an existing destination directory");
                            }
                            destination.clone()
                        };
                        validate_destination(source, &target)?;
                        if op == Operation::Move
                            && lookup(&target, &worker.ctx)?.is_none()
                            && source.fs.id() == target.fs.id()
                            && source
                                .fs
                                .rename(&source.path, &target.path, &worker.ctx)
                                .is_ok()
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
fn remove(path: &VfsPath, worker: &Worker) -> Result<()> {
    worker.check()?;
    let directory = path.metadata(false, &worker.ctx)?.kind == Kind::Directory;
    if directory {
        for (child, _) in path.read_dir(&worker.ctx)? {
            remove(&child, worker)?;
        }
    }
    path.fs.remove(&path.path, directory, &worker.ctx)?;
    worker.progress.lock().unwrap().files += 1;
    Ok(())
}
/// Session-aware path locks also retain and lock archive backing resources.
pub fn resources(op: Operation, sources: &[VfsPath], destination: &VfsPath) -> Vec<VfsPath> {
    let mut paths = sources.to_vec();
    if matches!(op, Operation::Copy | Operation::Move)
        && !destination.fs.is_remote()
        && destination.is_dir()
    {
        paths.extend(
            sources
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| destination.join(n)),
        );
    } else if matches!(op, Operation::Copy | Operation::Move | Operation::Mkdir) {
        paths.push(destination.clone());
    }
    let backing: Vec<_> = paths
        .iter()
        .flat_map(|p| p.fs.backing_resources())
        .collect();
    paths.extend(backing);
    paths
        .into_iter()
        .map(|p| VfsPath::new(p.fs.clone(), p.fs.lock_path(&p.path)))
        .collect()
}
/// Compare lock sets returned by `resources`; inputs must already be canonicalized.
pub fn overlaps(a: &[VfsPath], b: &[VfsPath]) -> bool {
    a.iter()
        .any(|a| b.iter().any(|b| a.starts_with(b) || b.starts_with(a)))
}
