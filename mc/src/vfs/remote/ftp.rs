use super::*;
use std::io::{BufRead, BufReader, SeekFrom};
use suppaftp::{DataStream, FtpStream, Mode, types::FileType};

#[derive(Clone)]
pub(super) struct Ftp {
    endpoint: Endpoint,
    password: Arc<Secret>,
    mfmt: bool,
}
fn control(endpoint: &Endpoint, ctx: &Context) -> Result<FtpStream> {
    let socket = socket(endpoint, ctx)?;
    let peer = socket.peer_addr()?.ip();
    let mut ftp =
        FtpStream::connect_with_stream(socket)?.passive_stream_builder(move |mut addr| {
            // Never connect to a third-party address supplied by PASV.
            addr.set_ip(peer);
            let stream = TcpStream::connect_timeout(&addr, TIMEOUT)
                .map_err(suppaftp::FtpError::ConnectionError)?;
            stream
                .set_read_timeout(Some(TIMEOUT))
                .map_err(suppaftp::FtpError::ConnectionError)?;
            stream
                .set_write_timeout(Some(TIMEOUT))
                .map_err(suppaftp::FtpError::ConnectionError)?;
            Ok(stream)
        });
    ftp.set_mode(if peer.is_ipv6() {
        Mode::ExtendedPassive
    } else {
        Mode::Passive
    });
    ftp.set_passive_nat_workaround(true);
    Ok(ftp)
}
impl Ftp {
    pub fn connect(endpoint: Endpoint, ctx: &Context) -> Result<Self> {
        for retry in 0..3 {
            let password = if endpoint.user == "anonymous" {
                Secret::new("anonymous@".into())
            } else {
                ctx.password(&endpoint.label(Path::new("/")), retry > 0)?
            };
            safe(&password)?;
            let mut ftp = control(&endpoint, ctx)?;
            if ftp.login(&endpoint.user, &password).is_ok() {
                let mfmt = ftp
                    .feat()
                    .is_ok_and(|features| features.keys().any(|k| k.eq_ignore_ascii_case("MFMT")));
                return Ok(Self {
                    mfmt,
                    endpoint,
                    password: Arc::new(password),
                });
            }
            if endpoint.user == "anonymous" {
                break;
            }
        }
        bail!("FTP authentication failed")
    }
    fn session(&self, ctx: &Context) -> Result<FtpStream> {
        let mut ftp = control(&self.endpoint, ctx)?;
        ftp.login(self.endpoint.user.as_str(), self.password.as_str())?;
        ftp.transfer_type(FileType::Binary)?;
        Ok(ftp)
    }
    fn listing(ftp: &mut FtpStream, path: &str, ctx: &Context) -> Result<Vec<DirEntry>> {
        let mut entries = vec![];
        Self::visit_listing(ftp, path, ctx, &mut |e| {
            entries.push(e);
            Ok(())
        })?;
        Ok(entries)
    }
    fn visit_listing(
        ftp: &mut FtpStream,
        path: &str,
        ctx: &Context,
        emit: &mut dyn FnMut(DirEntry) -> Result<()>,
    ) -> Result<()> {
        ctx.check()?;
        // CWD avoids LIST treating a filename beginning with '-' as an option.
        ftp.cwd(path)?;
        let statuses = [suppaftp::Status::AboutToSend, suppaftp::Status::AlreadyOpen];
        let (stream, machine) = match ftp.custom_data_command("MLSD", &statuses) {
            Ok((_, stream)) => (stream, true),
            Err(suppaftp::FtpError::UnexpectedResponse(r))
                if [500, 502, 504].contains(&(r.status.code())) =>
            {
                (ftp.custom_data_command("LIST", &statuses)?.1, false)
            }
            Err(e) => return Err(e.into()),
        };
        let mut reader = BufReader::new(stream);
        let mut count = 0;
        let mut total = 0;
        loop {
            ctx.check()?;
            let mut line = String::new();
            let bytes = reader.by_ref().take(65536).read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            total += bytes;
            ensure!(
                line.ends_with('\n') && total <= 32 * 1024 * 1024,
                "FTP listing exceeds size limits"
            );
            let line = line.trim_end_matches(['\r', '\n']);
            if machine
                && line.split_once(' ').is_some_and(|(facts, _)| {
                    facts.split(';').any(|f| {
                        f.eq_ignore_ascii_case("type=cdir") || f.eq_ignore_ascii_case("type=pdir")
                    })
                })
            {
                continue;
            }
            let file = if machine {
                suppaftp::list::ListParser::parse_mlsd(line)
            } else {
                suppaftp::list::File::try_from(line)
            }
            .map_err(|_| anyhow::anyhow!("Unsupported FTP directory listing entry"))?;
            if matches!(file.name(), "." | "..") {
                continue;
            }
            remote_name(file.name())?;
            ensure!(count < 100_000, "Remote directory exceeds 100,000 entries");
            count += 1;
            emit(DirEntry {
                name: file.name().into(),
                metadata: Metadata {
                    kind: if file.is_symlink() {
                        Kind::Symlink
                    } else if file.is_directory() {
                        Kind::Directory
                    } else if file.is_file() {
                        Kind::File
                    } else {
                        Kind::Special
                    },
                    size: file.size() as u64,
                    modified: file.modified(),
                    permissions: None,
                },
            })?;
        }
        ftp.close_data_connection(reader.into_inner())?;
        Ok(())
    }
    fn stat(ftp: &mut FtpStream, path: &str, ctx: &Context) -> Result<Metadata> {
        if path == "/" {
            ftp.cwd("/")?;
            return Ok(Metadata::directory());
        }
        let (parent, name) = path.rsplit_once('/').context("Invalid FTP path")?;
        Self::listing(ftp, if parent.is_empty() { "/" } else { parent }, ctx)?
            .into_iter()
            .find(|e| e.name == OsStr::new(name))
            .map(|e| e.metadata)
            .ok_or_else(missing)
    }
}
impl FileSystem for Ftp {
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
        let path = wire_path(path)?;
        let mut ftp = self.session(ctx)?;
        let meta = Self::stat(&mut ftp, &path, ctx)?;
        if follow && meta.kind == Kind::Symlink {
            if ftp.cwd(&path).is_ok() {
                return Ok(Metadata::directory());
            }
            bail!("FTP cannot resolve this symlink safely");
        }
        Ok(meta)
    }
    fn read_dir(&self, path: &Path, ctx: &Context) -> Result<Vec<DirEntry>> {
        Self::listing(&mut self.session(ctx)?, &wire_path(path)?, ctx)
    }
    fn visit_dir(
        &self,
        path: &Path,
        ctx: &Context,
        emit: &mut dyn FnMut(DirEntry) -> Result<()>,
    ) -> Result<()> {
        Self::visit_listing(&mut self.session(ctx)?, &wire_path(path)?, ctx, emit)
    }
    fn open_read(&self, path: &Path, ctx: &Context) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(self.open_seek(path, ctx)?))
    }
    fn open_seek(&self, path: &Path, ctx: &Context) -> Result<Box<dyn ReadSeek>> {
        let meta = self.metadata(path, true, ctx)?;
        ensure!(meta.kind == Kind::File, "Only regular files may be read");
        Ok(Box::new(Reader {
            fs: self.clone(),
            path: wire_path(path)?,
            ctx: ctx.clone(),
            transfer: None,
            position: 0,
            length: meta.size,
        }))
    }
    fn create(&self, path: &Path, overwrite: bool, ctx: &Context) -> Result<Box<dyn WriteHandle>> {
        let path = wire_path(path)?;
        let mut ftp = self.session(ctx)?;
        // MKD reserves a unique staging directory even on servers without STOU.
        let parent = Path::new(&path).parent().context("No FTP parent")?;
        let stage_dir = format!(
            "{}/.mc-upload-{}-{}",
            wire_path(parent)?,
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_nanos()
        );
        ftp.mkdir(&stage_dir)?;
        let stage = format!("{stage_dir}/data");
        let data = match ftp.put_with_stream(&stage) {
            Ok(DataStream::Tcp(data)) => data,
            Ok(_) => bail!("Unexpected TLS stream"),
            Err(e) => {
                let _ = ftp.rmdir(&stage_dir);
                return Err(e.into());
            }
        };
        Ok(Box::new(Writer {
            fs: self.clone(),
            ftp: Some(ftp),
            data: Some(data),
            stage_dir,
            stage,
            destination: path,
            overwrite,
            ctx: ctx.clone(),
            committed: false,
            written: 0,
        }))
    }
    fn set_metadata(&self, path: &Path, metadata: &Metadata, ctx: &Context) -> Result<()> {
        if self.mfmt && metadata.kind == Kind::File {
            set_mtime(&mut self.session(ctx)?, &wire_path(path)?, metadata)?;
        }
        Ok(())
    }
    fn mkdir(&self, path: &Path, ctx: &Context) -> Result<()> {
        self.session(ctx)?.mkdir(wire_path(path)?)?;
        Ok(())
    }
    fn remove(&self, path: &Path, directory: bool, ctx: &Context) -> Result<()> {
        let path = wire_path(path)?;
        ensure!(path != "/", "Cannot remove an FTP root");
        let mut ftp = self.session(ctx)?;
        if directory {
            ftp.rmdir(path)?;
        } else {
            ftp.rm(path)?;
        }
        Ok(())
    }
    fn canonical(&self, path: &Path) -> Result<PathBuf> {
        let path = wire_path(path)?;
        let ctx = Context::default();
        let mut ftp = self.session(&ctx)?;
        if ftp.cwd(&path).is_ok() {
            return Ok(ftp.pwd()?.into());
        }
        let p = Path::new(&path);
        ftp.cwd(wire_path(p.parent().context("No FTP parent")?)?)?;
        Ok(PathBuf::from(ftp.pwd()?).join(p.file_name().context("No FTP filename")?))
    }
}
type Data = TcpStream;
struct Reader {
    fs: Ftp,
    path: String,
    ctx: Context,
    transfer: Option<(FtpStream, Data)>,
    position: u64,
    length: u64,
}
impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        (|| -> Result<usize> {
            self.ctx.check()?;
            if out.is_empty() {
                return Ok(0);
            }
            if self.position >= self.length {
                if let Some((mut ftp, mut data)) = self.transfer.take() {
                    let mut extra = [0];
                    ensure!(data.read(&mut extra)? == 0, "FTP file grew during transfer");
                    ftp.finalize_retr_stream(data)?;
                }
                return Ok(0);
            }
            if self.transfer.is_none() {
                let mut ftp = self.fs.session(&self.ctx)?;
                if self.position > 0 {
                    ftp.resume_transfer(self.position.try_into()?)?;
                }
                let DataStream::Tcp(data) = ftp.retr_as_stream(&self.path)? else {
                    bail!("Unexpected TLS stream");
                };
                self.transfer = Some((ftp, data));
            }
            let remaining = usize::try_from(self.length - self.position).unwrap_or(usize::MAX);
            let limit = out.len().min(remaining);
            let n = self.transfer.as_mut().unwrap().1.read(&mut out[..limit])?;
            self.position += n as u64;
            if n == 0 {
                let (mut ftp, data) = self.transfer.take().unwrap();
                ftp.finalize_retr_stream(data)?;
                ensure!(
                    self.position == self.length,
                    "FTP file ended before its advertised size"
                );
            }
            Ok(n)
        })()
        .map_err(io::Error::other)
    }
}
impl Seek for Reader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.ctx.check().map_err(io::Error::other)?;
        let value = match position {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.length) + i128::from(n),
        };
        let value = u64::try_from(value).map_err(io::Error::other)?;
        if value != self.position {
            self.transfer = None;
            self.position = value;
        }
        Ok(value)
    }
}
struct Writer {
    fs: Ftp,
    ftp: Option<FtpStream>,
    data: Option<Data>,
    stage_dir: String,
    stage: String,
    destination: String,
    overwrite: bool,
    ctx: Context,
    committed: bool,
    written: u64,
}
impl Write for Writer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.ctx.check().map_err(io::Error::other)?;
        let n = self.data.as_mut().unwrap().write(data)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.data.as_mut().unwrap().flush()
    }
}
impl WriteHandle for Writer {
    fn staging_location(&self) -> Option<String> {
        Some(self.fs.endpoint.label(Path::new(&self.stage_dir)))
    }
    fn commit(mut self: Box<Self>, metadata: &Metadata) -> Result<()> {
        self.ctx.check()?;
        ensure!(
            self.written == metadata.size,
            "Incomplete FTP upload; destination was not published"
        );
        let data = self.data.take().unwrap();
        let ftp = self.ftp.as_mut().unwrap();
        ftp.finalize_put_stream(data)?;
        ensure!(
            Ftp::stat(ftp, &self.stage, &self.ctx)?.size == self.written,
            "FTP server stored an unexpected byte count; destination was not published"
        );
        if self.fs.mfmt {
            set_mtime(ftp, &self.stage, metadata)?;
        }
        match Ftp::stat(ftp, &self.destination, &self.ctx) {
            Ok(meta) => {
                ensure!(
                    self.overwrite,
                    "FTP destination appeared during transfer; refusing overwrite"
                );
                ensure!(
                    meta.kind == Kind::File,
                    "Cannot replace an FTP directory, link, or special file"
                );
            }
            Err(e)
                if e.downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) => {}
            Err(e) => return Err(e),
        }
        self.ctx.check()?;
        // FTP has no conditional rename. Never delete the old destination to make RNTO succeed.
        ftp.rename(&self.stage, &self.destination).context("FTP publication could not be confirmed; inspect the destination before retrying. The client did not delete it or retry the rename")?;
        self.committed = true;
        let _ = ftp.rmdir(&self.stage_dir);
        Ok(())
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.data.take();
        if !self.committed {
            self.ftp.take();
            // A new control connection avoids stale transfer replies after cancellation.
            if let Ok(mut ftp) = self.fs.session(&Context::default()) {
                let _ = ftp.rm(&self.stage);
                let _ = ftp.rmdir(&self.stage_dir);
            }
        }
    }
}

fn set_mtime(ftp: &mut FtpStream, path: &str, metadata: &Metadata) -> Result<()> {
    let stamp: chrono::DateTime<chrono::Utc> = metadata.modified.into();
    ftp.custom_command(
        format!("MFMT {} {path}", stamp.format("%Y%m%d%H%M%S")),
        &[
            suppaftp::Status::File,
            suppaftp::Status::RequestedFileActionOk,
        ],
    )?;
    Ok(())
}
