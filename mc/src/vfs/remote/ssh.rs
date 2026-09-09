use super::*;
use serde::Deserialize;
use serde_json::{Value, json};
use ssh2::{
    CheckResult, FileStat, KnownHostFileKind, OpenFlags, OpenType, RenameFlags, Session, Sftp,
};
use std::io::{BufRead, BufReader, SeekFrom};

pub(super) struct Ssh {
    endpoint: Endpoint,
    session: Session,
    sftp: Option<Arc<Sftp>>,
}
fn home() -> Result<PathBuf> {
    Ok(std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("Home directory is unavailable")?
        .into())
}
impl Ssh {
    pub fn connect(endpoint: Endpoint, ctx: &Context) -> Result<Self> {
        let mut session = Session::new()?;
        session.set_timeout(TIMEOUT.as_millis() as u32);
        session.set_tcp_stream(socket(&endpoint, ctx)?);
        session.handshake()?;
        ctx.check()?;
        let mut known = session.known_hosts()?;
        let known_file = home()?.join(".ssh/known_hosts");
        if known_file.exists() {
            known.read_file(&known_file, KnownHostFileKind::OpenSSH)?;
        }
        let (key, _) = session
            .host_key()
            .context("SSH server supplied no host key")?;
        match known.check_port(&endpoint.host, endpoint.port, key) {
            CheckResult::Match => {}
            CheckResult::Mismatch => bail!(
                "SSH host key changed for {}. Verify the server identity before updating {}",
                endpoint.authority(),
                known_file.display()
            ),
            _ => bail!(
                "SSH host is not trusted: {}. Verify its fingerprint using your SSH client and add it to {} first",
                endpoint.authority(),
                known_file.display()
            ),
        }
        let _ = session.userauth_agent(&endpoint.user);
        let keys = [home()?.join(".ssh/id_ed25519"), home()?.join(".ssh/id_rsa")];
        if !session.authenticated() {
            for key in keys.iter().filter(|k| k.is_file()) {
                ctx.check()?;
                if session
                    .userauth_pubkey_file(&endpoint.user, None, key, None)
                    .is_ok()
                {
                    break;
                }
            }
        }
        for retry in 0..3 {
            if session.authenticated() {
                break;
            }
            let password = ctx.password(
                &format!(
                    "{} — password or private-key passphrase",
                    endpoint.label(Path::new("/"))
                ),
                retry > 0,
            )?;
            for key in keys.iter().filter(|k| k.is_file()) {
                ctx.check()?;
                if session
                    .userauth_pubkey_file(&endpoint.user, None, key, Some(&password))
                    .is_ok()
                {
                    break;
                }
            }
            if !session.authenticated() {
                let _ = session.userauth_password(&endpoint.user, &password);
            }
        }
        ensure!(session.authenticated(), "SSH authentication failed");
        ctx.check()?;
        let sftp = if endpoint.protocol == Protocol::Sftp {
            Some(Arc::new(session.sftp()?))
        } else {
            None
        };
        Ok(Self {
            endpoint,
            session,
            sftp,
        })
    }
    fn rpc(&self, value: Value, ctx: &Context) -> Result<Rpc> {
        ctx.check()?;
        let mut channel = self.session.channel_session()?;
        // Only constant source is interpolated into the command. User data is JSON on stdin.
        let source = include_str!("helper.py").replace('\'', "'\\''");
        channel.exec(&format!("python3 -u -c '{source}'"))?;
        let mut rpc = Rpc {
            channel: BufReader::new(channel),
            ctx: ctx.clone(),
        };
        rpc.send(value)?;
        Ok(rpc)
    }
    fn call(&self, value: Value, ctx: &Context) -> Result<Value> {
        self.rpc(value, ctx)?.receive()
    }
}
fn ssh_error(error: ssh2::Error) -> anyhow::Error {
    if error.code() == ssh2::ErrorCode::SFTP(2) {
        missing()
    } else {
        error.into()
    }
}
fn metadata(stat: FileStat) -> Metadata {
    Metadata {
        kind: if stat.is_dir() {
            Kind::Directory
        } else if stat.is_file() {
            Kind::File
        } else if stat.file_type().is_symlink() {
            Kind::Symlink
        } else {
            Kind::Special
        },
        size: stat.size.unwrap_or(0),
        modified: SystemTime::UNIX_EPOCH + Duration::from_secs(stat.mtime.unwrap_or(0)),
        permissions: None,
    }
}
#[derive(Deserialize)]
struct Info {
    kind: String,
    size: u64,
    modified: u64,
}
impl Info {
    fn metadata(self) -> Metadata {
        Metadata {
            kind: match self.kind.as_str() {
                "directory" => Kind::Directory,
                "file" => Kind::File,
                "symlink" => Kind::Symlink,
                _ => Kind::Special,
            },
            size: self.size,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(self.modified),
            permissions: None,
        }
    }
}
impl FileSystem for Ssh {
    fn id(&self) -> String {
        self.endpoint.id()
    }
    fn label(&self, path: &Path) -> String {
        self.endpoint.label(path)
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn lock_path(&self, _path: &Path) -> PathBuf {
        PathBuf::from("/")
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            write: true,
            seek: true,
            ..Default::default()
        }
    }
    fn metadata(&self, path: &Path, follow: bool, ctx: &Context) -> Result<Metadata> {
        ctx.check()?;
        let path = wire_path(path)?;
        if let Some(sftp) = &self.sftp {
            Ok(metadata(
                if follow {
                    sftp.stat(Path::new(&path))
                } else {
                    sftp.lstat(Path::new(&path))
                }
                .map_err(ssh_error)?,
            ))
        } else {
            Ok(serde_json::from_value::<Info>(
                self.call(json!({"op":"metadata", "path":path, "follow":follow}), ctx)?,
            )?
            .metadata())
        }
    }
    fn read_dir(&self, path: &Path, ctx: &Context) -> Result<Vec<DirEntry>> {
        ctx.check()?;
        let path = wire_path(path)?;
        if let Some(sftp) = &self.sftp {
            let mut entries = vec![];
            let mut directory = sftp.opendir(Path::new(&path))?;
            loop {
                ctx.check()?;
                let (name, stat) = match directory.readdir() {
                    Ok(entry) => entry,
                    Err(e) if e.code() == ssh2::ErrorCode::Session(-16) => break,
                    Err(e) => return Err(e.into()),
                };
                let name = name.to_str().context("Remote filenames must be UTF-8")?;
                if matches!(name, "." | "..") {
                    continue;
                }
                remote_name(name)?;
                ensure!(
                    entries.len() < 100_000,
                    "Remote directory exceeds 100,000 entries"
                );
                entries.push(DirEntry {
                    name: name.into(),
                    metadata: metadata(stat),
                });
            }
            Ok(entries)
        } else {
            #[derive(Deserialize)]
            struct Item {
                name: String,
                #[serde(flatten)]
                info: Info,
            }
            let entries: Vec<Item> =
                serde_json::from_value(self.call(json!({"op":"list", "path":path}), ctx)?)?;
            entries
                .into_iter()
                .map(|item| {
                    remote_name(&item.name)?;
                    Ok(DirEntry {
                        name: item.name.into(),
                        metadata: item.info.metadata(),
                    })
                })
                .collect()
        }
    }
    fn open_read(&self, path: &Path, ctx: &Context) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(self.open_seek(path, ctx)?))
    }
    fn open_seek(&self, path: &Path, ctx: &Context) -> Result<Box<dyn ReadSeek>> {
        ctx.check()?;
        let path = wire_path(path)?;
        if let Some(sftp) = &self.sftp {
            let mut file = sftp.open(Path::new(&path))?;
            ensure!(file.stat()?.is_file(), "Only regular files may be read");
            Ok(Box::new(SftpReader {
                file,
                ctx: ctx.clone(),
            }))
        } else {
            let mut rpc = self.rpc(json!({"op":"read", "path":path}), ctx)?;
            rpc.receive()?;
            Ok(Box::new(rpc))
        }
    }
    fn create(&self, path: &Path, overwrite: bool, ctx: &Context) -> Result<Box<dyn WriteHandle>> {
        ctx.check()?;
        let path = wire_path(path)?;
        if let Some(sftp) = &self.sftp {
            let stage = staging_path(Path::new(&path))?;
            let file = sftp.open_mode(
                &stage,
                OpenFlags::CREATE | OpenFlags::EXCLUSIVE | OpenFlags::WRITE,
                0o600,
                OpenType::File,
            )?;
            Ok(Box::new(SftpWriter {
                file: Some(file),
                sftp: sftp.clone(),
                stage,
                destination: path.into(),
                overwrite,
                ctx: ctx.clone(),
                committed: false,
            }))
        } else {
            let mut rpc = self.rpc(
                json!({"op":"write", "path":path, "overwrite":overwrite}),
                ctx,
            )?;
            rpc.receive()?;
            Ok(Box::new(rpc))
        }
    }
    fn mkdir(&self, path: &Path, ctx: &Context) -> Result<()> {
        ctx.check()?;
        let path = wire_path(path)?;
        if let Some(sftp) = &self.sftp {
            sftp.mkdir(Path::new(&path), 0o755)?;
        } else {
            self.call(json!({"op":"mkdir", "path":path}), ctx)?;
        }
        Ok(())
    }
    fn remove(&self, path: &Path, directory: bool, ctx: &Context) -> Result<()> {
        ctx.check()?;
        let path = wire_path(path)?;
        ensure!(path != "/", "Cannot remove a remote root");
        if let Some(sftp) = &self.sftp {
            if directory {
                sftp.rmdir(Path::new(&path))?;
            } else {
                sftp.unlink(Path::new(&path))?;
            }
        } else {
            self.call(
                json!({"op":"remove", "path":path, "directory":directory}),
                ctx,
            )?;
        }
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path, ctx: &Context) -> Result<()> {
        ctx.check()?;
        if let Some(sftp) = &self.sftp {
            sftp.rename(
                Path::new(&wire_path(from)?),
                Path::new(&wire_path(to)?),
                Some(RenameFlags::empty()),
            )?;
            Ok(())
        } else {
            bail!("SSH helper moves use safe copy and remove")
        }
    }
    fn read_link(&self, path: &Path, ctx: &Context) -> Result<PathBuf> {
        ctx.check()?;
        let path = wire_path(path)?;
        if let Some(sftp) = &self.sftp {
            Ok(sftp.readlink(Path::new(&path))?)
        } else {
            Ok(serde_json::from_value::<String>(
                self.call(json!({"op":"readlink", "path":path}), ctx)?,
            )?
            .into())
        }
    }
    fn canonical(&self, path: &Path) -> Result<PathBuf> {
        let path = wire_path(path)?;
        if let Some(sftp) = &self.sftp {
            let mut current = Path::new(&path);
            let mut tail = vec![];
            loop {
                match sftp.realpath(current) {
                    Ok(mut resolved) => {
                        for name in tail.into_iter().rev() {
                            resolved.push(name);
                        }
                        return Ok(resolved);
                    }
                    Err(e) if e.code() == ssh2::ErrorCode::SFTP(2) => {
                        tail.push(
                            current
                                .file_name()
                                .context("Cannot resolve remote root")?
                                .to_owned(),
                        );
                        current = current.parent().context("Cannot resolve remote parent")?;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        } else {
            Ok(serde_json::from_value::<String>(
                self.call(json!({"op":"canonical", "path":path}), &Context::default())?,
            )?
            .into())
        }
    }
}
fn staging_path(path: &Path) -> Result<PathBuf> {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Ok(path.parent().context("No remote parent")?.join(format!(
        ".mc-upload-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )))
}
struct SftpReader {
    file: ssh2::File,
    ctx: Context,
}
impl Read for SftpReader {
    fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
        self.ctx.check().map_err(io::Error::other)?;
        self.file.read(data)
    }
}
impl Seek for SftpReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.ctx.check().map_err(io::Error::other)?;
        self.file.seek(position)
    }
}
struct SftpWriter {
    file: Option<ssh2::File>,
    sftp: Arc<Sftp>,
    stage: PathBuf,
    destination: PathBuf,
    overwrite: bool,
    ctx: Context,
    committed: bool,
}
impl Write for SftpWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.ctx.check().map_err(io::Error::other)?;
        self.file.as_mut().unwrap().write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().unwrap().flush()
    }
}
impl WriteHandle for SftpWriter {
    fn commit(mut self: Box<Self>, _metadata: &Metadata) -> Result<()> {
        self.ctx.check()?;
        self.file.take().unwrap().close()?;
        let flags = if self.overwrite {
            RenameFlags::OVERWRITE | RenameFlags::ATOMIC | RenameFlags::NATIVE
        } else {
            RenameFlags::empty()
        };
        self.sftp.rename(&self.stage, &self.destination, Some(flags)).context("Server rejected staged rename; existing destination was not deleted. Try ssh:// for atomic replacement")?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for SftpWriter {
    fn drop(&mut self) {
        self.file.take();
        if !self.committed {
            let _ = self.sftp.unlink(&self.stage);
        }
    }
}
struct Rpc {
    channel: BufReader<ssh2::Channel>,
    ctx: Context,
}
impl Rpc {
    fn send(&mut self, value: Value) -> Result<()> {
        self.ctx.check()?;
        serde_json::to_writer(self.channel.get_mut(), &value)?;
        self.channel.get_mut().write_all(b"\n")?;
        self.channel.get_mut().flush()?;
        Ok(())
    }
    fn receive(&mut self) -> Result<Value> {
        self.ctx.check()?;
        let mut line = String::new();
        let n = self
            .channel
            .by_ref()
            .take(32 * 1024 * 1024)
            .read_line(&mut line)?;
        ensure!(
            n > 0 && line.ends_with('\n'),
            "SSH helper did not respond. The server needs Python 3 and permission to execute it"
        );
        let value: Value = serde_json::from_str(&line)?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            if value.get("missing").and_then(Value::as_bool) == Some(true) {
                return Err(missing());
            }
            bail!("SSH filesystem: {error}");
        }
        Ok(value)
    }
}
impl Read for Rpc {
    fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        (|| -> Result<usize> {
            self.send(json!({"read":data.len().min(65536)}))?;
            let length = self
                .receive()?
                .as_u64()
                .context("Invalid SSH read response")? as usize;
            ensure!(length <= data.len().min(65536), "Invalid SSH read length");
            self.channel.read_exact(&mut data[..length])?;
            Ok(length)
        })()
        .map_err(io::Error::other)
    }
}
impl Seek for Rpc {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        (|| -> Result<u64> {
            let (offset, whence) = match position {
                SeekFrom::Start(n) => (i64::try_from(n)?, 0),
                SeekFrom::Current(n) => (n, 1),
                SeekFrom::End(n) => (n, 2),
            };
            self.send(json!({"seek":[offset, whence]}))?;
            self.receive()?
                .as_u64()
                .context("Invalid SSH seek response")
        })()
        .map_err(io::Error::other)
    }
}
impl Write for Rpc {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        (|| -> Result<usize> {
            let n = data.len().min(65536);
            self.send(json!({"write":n}))?;
            self.channel.get_mut().write_all(&data[..n])?;
            self.channel.get_mut().flush()?;
            self.receive()?;
            Ok(n)
        })()
        .map_err(io::Error::other)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.channel.get_mut().flush()
    }
}
impl WriteHandle for Rpc {
    fn commit(mut self: Box<Self>, metadata: &Metadata) -> Result<()> {
        self.send(json!({"commit":true, "modified":metadata.modified.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs())}))?;
        self.receive()?;
        Ok(())
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.channel.get_mut().send_eof();
        let _ = self.channel.get_mut().close();
    }
}
