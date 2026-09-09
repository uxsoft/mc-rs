//! Backend-dispatched filesystem operations, inspired by MC's vfs_class/vfs_path_t.
//! Provider-relative paths never masquerade as OS paths. Open handles own their sessions.
pub mod local;
pub mod remote;
use anyhow::{Result, bail};
use std::{
    cmp::Ordering as Cmp,
    ffi::OsStr,
    fmt,
    hash::{Hash, Hasher},
    io::{self, Read, Seek, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime},
};
use zeroize::Zeroizing;

pub type Secret = Zeroizing<String>;
pub struct AuthRequest {
    pub resource: String,
    pub retry: bool,
    pub cancel: Arc<AtomicBool>,
    pub reply: mpsc::SyncSender<Option<Secret>>,
}
#[derive(Clone, Default)]
pub struct Context {
    pub cancel: Arc<AtomicBool>,
    pub auth: Option<mpsc::Sender<AuthRequest>>,
    pub abandoned: Option<Arc<AtomicBool>>,
}
impl Context {
    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed)
            || self
                .abandoned
                .as_ref()
                .is_some_and(|a| a.load(Ordering::Relaxed))
        {
            bail!("Cancelled");
        }
        Ok(())
    }
    pub fn password(&self, resource: &str, retry: bool) -> Result<Secret> {
        self.check()?;
        let sender = self
            .auth
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Password required for {resource}"))?;
        let (reply, receiver) = mpsc::sync_channel(1);
        sender.send(AuthRequest {
            resource: resource.into(),
            retry,
            cancel: self.cancel.clone(),
            reply,
        })?;
        loop {
            self.check()?;
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(password)) => return Ok(password),
                Ok(None) => bail!("Password entry cancelled"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => bail!("Password request closed"),
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Symlink,
    Special,
}
#[derive(Clone, Debug)]
pub struct Metadata {
    pub kind: Kind,
    pub size: u64,
    pub modified: SystemTime,
    pub permissions: Option<std::fs::Permissions>,
}
impl Metadata {
    pub fn directory() -> Self {
        Self {
            kind: Kind::Directory,
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
            permissions: None,
        }
    }
}
#[derive(Clone, Copy, Default)]
pub struct Capabilities {
    pub write: bool,
    pub trash: bool,
    pub symlink: bool,
    pub seek: bool,
}
pub struct DirEntry {
    pub name: std::ffi::OsString,
    pub metadata: Metadata,
}
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}
/// A staged destination handle; dropping it aborts an incomplete copy.
pub trait WriteHandle: Write + Send {
    fn commit(self: Box<Self>, metadata: &Metadata) -> Result<()>;
}
pub trait FileSystem: Send + Sync {
    /// Stable session/backend identity, excluding credentials.
    fn id(&self) -> String;
    fn label(&self, path: &Path) -> String;
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn metadata(&self, path: &Path, follow: bool, ctx: &Context) -> Result<Metadata>;
    fn read_dir(&self, path: &Path, ctx: &Context) -> Result<Vec<DirEntry>>;
    fn open_read(&self, path: &Path, ctx: &Context) -> Result<Box<dyn Read + Send>>;
    fn open_seek(&self, _path: &Path, _ctx: &Context) -> Result<Box<dyn ReadSeek>> {
        bail!("Seekable reads unsupported by this filesystem")
    }
    fn create(
        &self,
        _path: &Path,
        _overwrite: bool,
        _ctx: &Context,
    ) -> Result<Box<dyn WriteHandle>> {
        bail!("Read-only filesystem")
    }
    fn mkdir(&self, _path: &Path, _ctx: &Context) -> Result<()> {
        bail!("Read-only filesystem")
    }
    fn remove(&self, _path: &Path, _directory: bool, _ctx: &Context) -> Result<()> {
        bail!("Read-only filesystem")
    }
    fn trash(&self, _path: &Path, _ctx: &Context) -> Result<()> {
        bail!("Trash unsupported by this filesystem")
    }
    fn rename(&self, _from: &Path, _to: &Path, _ctx: &Context) -> Result<()> {
        bail!("Rename unsupported by this filesystem")
    }
    fn read_link(&self, _path: &Path, _ctx: &Context) -> Result<PathBuf> {
        bail!("Links unsupported by this filesystem")
    }
    fn symlink(
        &self,
        _target: &Path,
        _to: &Path,
        _directory: bool,
        _overwrite: bool,
        _ctx: &Context,
    ) -> Result<()> {
        bail!("Links unsupported by this filesystem")
    }
    fn set_metadata(&self, _path: &Path, _metadata: &Metadata, _ctx: &Context) -> Result<()> {
        Ok(())
    }
    fn canonical(&self, path: &Path) -> Result<PathBuf> {
        Ok(path.to_owned())
    }
    /// UI-safe lock identity. Remote providers lock their endpoint conservatively.
    fn lock_path(&self, path: &Path) -> PathBuf {
        self.canonical(path).unwrap_or_else(|_| path.to_owned())
    }
    fn is_remote(&self) -> bool {
        false
    }
    fn same_file(&self, from: &Path, to: &Path) -> bool {
        from == to
    }
    fn local_path(&self, _path: &Path) -> Option<PathBuf> {
        None
    }
    fn mount_parent(&self) -> Option<VfsPath> {
        None
    }
    /// Underlying archive/container resources, for cross-backend job locking.
    fn backing_resources(&self) -> Vec<VfsPath> {
        vec![]
    }
}
#[derive(Clone)]
pub struct VfsPath {
    pub fs: Arc<dyn FileSystem>,
    pub path: PathBuf,
}
impl VfsPath {
    pub fn new(fs: Arc<dyn FileSystem>, path: PathBuf) -> Self {
        Self {
            fs,
            path: normalize(&path),
        }
    }
    pub fn join(&self, path: impl AsRef<Path>) -> Self {
        Self::new(self.fs.clone(), self.path.join(path))
    }
    pub fn parent(&self) -> Option<Self> {
        if self.path.as_os_str().is_empty() {
            self.fs.mount_parent()
        } else {
            self.path
                .parent()
                .map(|p| Self::new(self.fs.clone(), p.to_owned()))
        }
    }
    pub fn file_name(&self) -> Option<&OsStr> {
        self.path.file_name()
    }
    pub fn display(&self) -> String {
        self.fs.label(&self.path)
    }
    pub fn local_path(&self) -> Option<PathBuf> {
        self.fs.local_path(&self.path)
    }
    pub fn metadata(&self, follow: bool, ctx: &Context) -> Result<Metadata> {
        ctx.check()?;
        self.fs.metadata(&self.path, follow, ctx)
    }
    pub fn read_dir(&self, ctx: &Context) -> Result<Vec<(Self, Metadata)>> {
        ctx.check()?;
        self.fs
            .read_dir(&self.path, ctx)?
            .into_iter()
            .map(|e| {
                let p = Path::new(&e.name);
                anyhow::ensure!(
                    p.components().count() == 1
                        && matches!(p.components().next(), Some(Component::Normal(_))),
                    "Invalid directory entry name"
                );
                Ok((self.join(p), e.metadata))
            })
            .collect()
    }
    pub fn is_dir(&self) -> bool {
        self.metadata(true, &Context::default())
            .is_ok_and(|m| m.kind == Kind::Directory)
    }
    pub fn starts_with(&self, other: &Self) -> bool {
        self.fs.id() == other.fs.id() && self.path.starts_with(&other.path)
    }
}
impl From<PathBuf> for VfsPath {
    fn from(p: PathBuf) -> Self {
        Self::new(Arc::new(local::Local), p)
    }
}
impl From<&Path> for VfsPath {
    fn from(p: &Path) -> Self {
        p.to_owned().into()
    }
}
impl fmt::Display for VfsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display())
    }
}
impl fmt::Debug for VfsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VfsPath").field(&self.display()).finish()
    }
}
impl PartialEq for VfsPath {
    fn eq(&self, b: &Self) -> bool {
        self.fs.id() == b.fs.id() && self.path == b.path
    }
}
impl Eq for VfsPath {}
impl Hash for VfsPath {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.fs.id().hash(h);
        self.path.hash(h);
    }
}
impl Ord for VfsPath {
    fn cmp(&self, b: &Self) -> Cmp {
        self.fs.id().cmp(&b.fs.id()).then(self.path.cmp(&b.path))
    }
}
impl PartialOrd for VfsPath {
    fn partial_cmp(&self, b: &Self) -> Option<Cmp> {
        Some(self.cmp(b))
    }
}
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(c.as_os_str()),
        }
    }
    out
}

/// Bounded producer/consumer stream. Decoder handles stay on their owning worker.
pub fn stream(
    mut ctx: Context,
    produce: impl FnOnce(StreamWriter, &Context) -> Result<()> + Send + 'static,
) -> Box<dyn Read + Send> {
    let (tx, rx) = mpsc::sync_channel(2);
    let abandoned = Arc::new(AtomicBool::new(false));
    let flag = abandoned.clone();
    let reader_ctx = ctx.clone();
    ctx.abandoned = Some(flag.clone());
    std::thread::spawn(move || {
        let sink = StreamWriter {
            tx: tx.clone(),
            ctx: ctx.clone(),
            abandoned: flag,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| produce(sink, &ctx)))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Filesystem decoder failed unexpectedly")));
        if let Err(e) = result {
            let _ = tx.send(Err(io::Error::other(e.to_string())));
        }
    });
    Box::new(PipeReader {
        rx,
        buffer: io::Cursor::new(Vec::new()),
        abandoned,
        ctx: reader_ctx,
    })
}
#[derive(Clone)]
pub struct StreamWriter {
    tx: mpsc::SyncSender<io::Result<Vec<u8>>>,
    ctx: Context,
    abandoned: Arc<AtomicBool>,
}
impl Write for StreamWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.abandoned.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Reader closed"));
        }
        self.ctx.check().map_err(io::Error::other)?;
        let n = data.len().min(64 * 1024);
        self.tx
            .send(Ok(data[..n].to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Reader closed"))?;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct PipeReader {
    rx: mpsc::Receiver<io::Result<Vec<u8>>>,
    buffer: io::Cursor<Vec<u8>>,
    abandoned: Arc<AtomicBool>,
    ctx: Context,
}
impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let n = self.buffer.read(out)?;
            if n > 0 {
                return Ok(n);
            }
            self.ctx.check().map_err(io::Error::other)?;
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(data)) => self.buffer = io::Cursor::new(data),
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Ok(0),
            }
        }
    }
}
impl Drop for PipeReader {
    fn drop(&mut self) {
        self.abandoned.store(true, Ordering::Relaxed);
    }
}
pub fn copy_stream(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    ctx: &Context,
    mut progress: impl FnMut(u64),
) -> Result<()> {
    let mut buf = [0; 64 * 1024];
    loop {
        ctx.check()?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        progress(n as u64);
    }
    Ok(())
}
