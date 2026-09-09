use super::*;
use serde::Deserialize;
use serde_json::{Value, json};
use ssh2::{
    CheckResult, FileStat, KnownHostFileKind, OpenFlags, OpenType, RenameFlags, Session, Sftp,
};
use std::io::{BufRead, BufReader, SeekFrom};

pub(super) struct Ssh {
    config: super::config::Config,
    endpoint: Endpoint,
    session: Session,
    sftp: Option<Arc<Sftp>>,
}
pub(super) fn home() -> Result<PathBuf> {
    Ok(std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("Home directory is unavailable")?
        .into())
}
impl Ssh {
    pub fn connect(config: super::config::Config, ctx: &Context) -> Result<Self> {
        let session = connect_session(&config, ctx, &mut vec![])?;
        let endpoint = config.endpoint.clone();
        let sftp = if endpoint.protocol == Protocol::Sftp {
            Some(Arc::new(session.sftp()?))
        } else {
            None
        };
        Ok(Self {
            config,
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
            staging: None,
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
        permissions: mode_permissions(stat.perm),
    }
}
#[derive(Deserialize)]
struct Info {
    #[serde(default)]
    mode: Option<u32>,
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
            permissions: mode_permissions(self.mode),
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
    fn reconnect(&self, ctx: &Context) -> Result<Option<Arc<dyn FileSystem>>> {
        Ok(Some(Arc::new(Self::connect(self.config.clone(), ctx)?)))
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
    fn set_metadata(&self, path: &Path, metadata: &Metadata, ctx: &Context) -> Result<()> {
        ctx.check()?;
        if let Some(sftp) = &self.sftp {
            sftp.setstat(Path::new(&wire_path(path)?), file_stat(metadata))?;
        } else {
            self.call(json!({"op":"set_metadata","path":wire_path(path)?,"mode":permission_mode(metadata),"modified":metadata.modified.duration_since(SystemTime::UNIX_EPOCH)?.as_secs()}),ctx)?;
        }
        Ok(())
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
        let mut entries = vec![];
        self.visit_dir(path, ctx, &mut |e| {
            entries.push(e);
            Ok(())
        })?;
        Ok(entries)
    }
    fn visit_dir(
        &self,
        path: &Path,
        ctx: &Context,
        emit: &mut dyn FnMut(DirEntry) -> Result<()>,
    ) -> Result<()> {
        ctx.check()?;
        let path = wire_path(path)?;
        if let Some(sftp) = &self.sftp {
            let mut count = 0;
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
                ensure!(count < 100_000, "Remote directory exceeds 100,000 entries");
                count += 1;
                emit(DirEntry {
                    name: name.into(),
                    metadata: metadata(stat),
                })?;
            }
            Ok(())
        } else {
            #[derive(Deserialize)]
            struct Item {
                name: String,
                #[serde(flatten)]
                info: Info,
            }
            let mut rpc = self.rpc(json!({"op":"list_stream","path":path}), ctx)?;
            let mut count = 0;
            loop {
                let value = rpc.receive()?;
                if value.is_null() {
                    break;
                }
                ensure!(count < 100_000, "Remote directory exceeds 100,000 entries");
                count += 1;
                let item: Item = serde_json::from_value(value)?;
                remote_name(&item.name)?;
                emit(DirEntry {
                    name: item.name.into(),
                    metadata: item.info.metadata(),
                })?;
            }
            Ok(())
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
                staging_label: self.endpoint.label(&stage),
                stage,
                destination: path.into(),
                overwrite,
                ctx: ctx.clone(),
                committed: false,
                written: 0,
            }))
        } else {
            let mut rpc = self.rpc(
                json!({"op":"write", "path":path, "overwrite":overwrite}),
                ctx,
            )?;
            let stage = rpc
                .receive()?
                .as_str()
                .context("SSH helper did not report its staging path")?
                .to_owned();
            rpc.staging = Some(self.endpoint.label(Path::new(&stage)));
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
    fn rename_in_place(&self, from: &Path, to: &Path, ctx: &Context) -> Result<()> {
        if self.sftp.is_some() {
            return self
                .rename(from, to, ctx)
                .context("SFTP rename failed; inspect both names before retrying");
        }
        self.call(
            json!({"op":"rename", "path":wire_path(from)?, "to":wire_path(to)?}),
            ctx,
        )
        .context("SSH rename failed; inspect both names before retrying")?;
        Ok(())
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
    written: u64,
    staging_label: String,
}
impl Write for SftpWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.ctx.check().map_err(io::Error::other)?;
        let n = self.file.as_mut().unwrap().write(data)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().unwrap().flush()
    }
}
impl WriteHandle for SftpWriter {
    fn staging_location(&self) -> Option<String> {
        Some(self.staging_label.clone())
    }
    fn commit(mut self: Box<Self>, metadata: &Metadata) -> Result<()> {
        self.ctx.check()?;
        ensure!(
            self.written == metadata.size,
            "Incomplete SFTP upload; destination was not published"
        );
        ensure!(
            self.file.as_mut().unwrap().stat()?.size == Some(self.written),
            "SFTP server stored an unexpected byte count; destination was not published"
        );
        self.file.as_mut().unwrap().setstat(file_stat(metadata))?;
        self.file.take().unwrap().close()?;
        self.ctx.check()?;
        let flags = if self.overwrite {
            RenameFlags::OVERWRITE | RenameFlags::ATOMIC | RenameFlags::NATIVE
        } else {
            RenameFlags::empty()
        };
        self.sftp.rename(&self.stage, &self.destination, Some(flags)).context("SFTP publication could not be confirmed; inspect the destination before retrying. The client did not delete it or retry the rename. Servers without overwrite support may require ssh://")?;
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
    staging: Option<String>,
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
            .read_line(&mut line)
            .context("SSH helper response failed. The server needs Python 3; noninteractive shell startup must not read stdin or write stdout")?;
        ensure!(
            n > 0 && line.ends_with('\n'),
            "SSH helper did not respond. The server needs Python 3 and permission to execute it"
        );
        let value: Value = serde_json::from_str(&line).context(
            "Invalid SSH helper response; noninteractive shell startup must not write to stdout",
        )?;
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
    fn staging_location(&self) -> Option<String> {
        self.staging.clone()
    }
    fn commit(mut self: Box<Self>, metadata: &Metadata) -> Result<()> {
        self.ctx.check()?;
        (|| {
            self.send(json!({"commit":true, "size":metadata.size, "mode":permission_mode(metadata), "modified":metadata.modified.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs())}))?;
            self.receive()
        })().context("SSH publication could not be confirmed; inspect the destination before retrying. The commit was not retried")?;
        Ok(())
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.channel.get_mut().send_eof();
        let _ = self.channel.get_mut().close();
    }
}

fn connect_session(
    config: &super::config::Config,
    ctx: &Context,
    route: &mut Vec<String>,
) -> Result<Session> {
    use base64::Engine;
    let endpoint = &config.endpoint;
    ensure!(
        route.len() < 8 && !route.contains(&endpoint.authority()),
        "SSH jump loop or more than 8 hops"
    );
    route.push(endpoint.authority());
    let transport = if let Some(jumps) = &config.jump {
        let (prefix, last) = jumps
            .rsplit_once(',')
            .map_or((None, jumps.as_str()), |(p, l)| (Some(p), l));
        let url = format!("ssh://{last}/");
        let uri = Url::parse(&url)?;
        let mut hop = super::config::Config::resolve(
            Endpoint::parse(&url)?,
            !uri.username().is_empty(),
            uri.port().is_some(),
            &home()?,
        )?;
        if let Some(prefix) = prefix {
            hop.jump = Some(prefix.into());
        }
        let parent = connect_session(&hop, ctx, route)?;
        let channel = parent.channel_direct_tcpip(&endpoint.host, endpoint.port, None)?;
        tunnel(parent, channel)?
    } else {
        socket(endpoint, ctx)?
    };
    let mut session = Session::new()?;
    session.set_timeout(TIMEOUT.as_millis() as u32);
    session.set_tcp_stream(transport);
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
            "SSH host key changed for {}. Verify its identity before updating {}",
            endpoint.authority(),
            known_file.display()
        ),
        CheckResult::NotFound => {
            ensure!(
                config.allow_unknown,
                "SSH host is not trusted; StrictHostKeyChecking yes requires a matching known_hosts entry"
            );
            ensure!(
                ctx.auth.is_some(),
                "SSH host is not trusted: {}",
                endpoint.authority()
            );
            let hash = session
                .host_key_hash(ssh2::HashType::Sha256)
                .context("SSH fingerprint unavailable")?;
            let fingerprint = base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash);
            let answer=ctx.prompt(&format!("{}\nUnknown host · SHA256:{fingerprint}\nVerify this fingerprint independently. Trust lasts for this connection only.",endpoint.label(Path::new("/"))), false,true)?;
            ensure!(answer.as_str() == "trust", "SSH host trust cancelled");
        }
        _ => bail!("SSH known_hosts check failed"),
    }
    if !config.identities_only {
        let _ = session.userauth_agent(&endpoint.user);
    }
    for key in config.keys.iter().filter(|k| k.is_file()) {
        if session.authenticated() {
            break;
        }
        ctx.check()?;
        if !session
            .auth_methods(&endpoint.user)?
            .split(',')
            .any(|m| m == "publickey")
        {
            break;
        }
        let _ = session.userauth_pubkey_file(&endpoint.user, None, key, None);
    }
    // Unlock encrypted private keys before keyboard-interactive: some servers
    // require publickey followed by a second factor and reject MFA before the key.
    for key in config.keys.iter().filter(|k| encrypted_key(k)) {
        for retry in 0..3 {
            if session.authenticated() {
                break;
            }
            let methods = session.auth_methods(&endpoint.user)?.to_owned();
            if !methods.split(',').any(|m| m == "publickey") {
                break;
            }
            let secret = ctx.password(
                &format!(
                    "{} — passphrase for {}",
                    endpoint.label(Path::new("/")),
                    key.display()
                ),
                retry > 0,
            )?;
            if session
                .userauth_pubkey_file(&endpoint.user, None, key, Some(&secret))
                .is_ok()
            {
                break;
            }
        }
    }
    struct Mfa<'a> {
        ctx: &'a Context,
        label: String,
        error: Option<anyhow::Error>,
    }
    impl ssh2::KeyboardInteractivePrompt for Mfa<'_> {
        fn prompt<'a>(
            &mut self,
            _: &str,
            instructions: &str,
            prompts: &[ssh2::Prompt<'a>],
        ) -> Vec<String> {
            prompts
                .iter()
                .map(|p| {
                    if self.error.is_some() {
                        return String::new();
                    }
                    match self.ctx.password(
                        &format!("{}\n{}\n{}", self.label, instructions, p.text),
                        false,
                    ) {
                        Ok(secret) => secret.to_string(),
                        Err(e) => {
                            self.error = Some(e);
                            String::new()
                        }
                    }
                })
                .collect()
        }
    }
    for retry in 0..3 {
        if session.authenticated() {
            break;
        }
        ctx.check()?;
        let methods = session.auth_methods(&endpoint.user)?.to_owned();
        if methods.split(',').any(|m| m == "keyboard-interactive") {
            let mut prompt = Mfa {
                ctx,
                label: endpoint.label(Path::new("/")),
                error: None,
            };
            let _ = session.userauth_keyboard_interactive(&endpoint.user, &mut prompt);
            if let Some(e) = prompt.error {
                return Err(e);
            }
            if session.authenticated() {
                break;
            }
            if !methods.split(',').any(|m| m == "password") {
                continue;
            }
        }
        let password = ctx.password(
            &format!(
                "{} — password or private-key passphrase",
                endpoint.label(Path::new("/"))
            ),
            retry > 0,
        )?;
        for key in config.keys.iter().filter(|k| k.is_file()) {
            ctx.check()?;
            if session
                .userauth_pubkey_file(&endpoint.user, None, key, Some(&password))
                .is_ok()
            {
                break;
            }
        }
        if !session.authenticated() && methods.split(',').any(|m| m == "password") {
            let _ = session.userauth_password(&endpoint.user, &password);
        }
    }
    ensure!(session.authenticated(), "SSH authentication failed");
    ctx.check()?;
    Ok(session)
}

// Bridge libssh2's socket-only API to an owned direct-tcpip channel. Both directions
// are nonblocking so a pending read cannot lock out a write on the parent session.
fn tunnel(session: Session, mut channel: ssh2::Channel) -> Result<TcpStream> {
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let client = TcpStream::connect(listener.local_addr()?)?;
    let (mut relay, _) = listener.accept()?;
    client.set_read_timeout(Some(TIMEOUT))?;
    client.set_write_timeout(Some(TIMEOUT))?;
    relay.set_nonblocking(true)?;
    session.set_blocking(false);
    std::thread::spawn(move || {
        let _session = session;
        let mut up = Vec::<u8>::new();
        let mut down = Vec::<u8>::new();
        let mut buf = [0u8; 65536];
        fn pump(out: &mut impl Write, buffer: &mut Vec<u8>) -> io::Result<()> {
            if !buffer.is_empty() {
                match out.write(buffer) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(n) => {
                        buffer.drain(..n);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
        loop {
            if up.is_empty() {
                match relay.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => up.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
            if pump(&mut channel, &mut up).is_err() {
                break;
            }
            if down.is_empty() {
                match channel.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => down.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
            if pump(&mut relay, &mut down).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = channel.send_eof();
        let _ = channel.close();
    });
    Ok(client)
}

fn file_stat(m: &Metadata) -> FileStat {
    let modified = m
        .modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|t| t.as_secs());
    FileStat {
        size: None,
        uid: None,
        gid: None,
        perm: permission_mode(m),
        atime: modified,
        mtime: modified,
    }
}

fn encrypted_key(path: &Path) -> bool {
    use base64::Engine;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut text = zeroize::Zeroizing::new(String::new());
    if file.take(1024 * 1024).read_to_string(&mut text).is_err() {
        return false;
    }
    if text.contains("ENCRYPTED") {
        return true;
    }
    if !text.contains("BEGIN OPENSSH PRIVATE KEY") {
        return false;
    }
    let encoded = zeroize::Zeroizing::new(
        text.lines()
            .filter(|l| !l.starts_with("-----"))
            .collect::<String>(),
    );
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes()) else {
        return false;
    };
    let bytes = zeroize::Zeroizing::new(bytes);
    if !bytes.starts_with(b"openssh-key-v1\0") {
        return false;
    }
    let Some(length) = bytes.get(15..19) else {
        return false;
    };
    let length = u32::from_be_bytes(length.try_into().unwrap()) as usize;
    bytes
        .get(19..19usize.saturating_add(length))
        .is_some_and(|cipher| cipher != b"none")
}
