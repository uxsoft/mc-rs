//! Read-only archive VFS sessions: immutable header index, on-demand content streams.
use crate::vfs::{self, *};
use anyhow::{Result, ensure};
use compress_tools::{ArchiveContents, ArchiveIteratorBuilder, ArchivePassword};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

pub fn supported(path: &VfsPath) -> bool {
    path.path.extension().is_some_and(|e| {
        ["zip", "rar", "tar", "7z", "gz", "tgz"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}
pub fn safe_path(name: &str) -> Result<PathBuf> {
    ensure!(
        !name.contains('\\') && !name.contains(':') && !name.starts_with('/'),
        "Unsafe archive path: {name}"
    );
    let path = Path::new(name);
    ensure!(
        path.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
        "Unsafe archive path: {name}"
    );
    Ok(path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect())
}
#[derive(Clone, Copy)]
enum Driver {
    Zip,
    Libarchive,
    SevenZip,
    Rar,
    Gzip,
}
#[derive(Clone)]
struct Entry {
    metadata: Metadata,
    member: Option<String>,
}
struct Archive {
    id: String,
    source: VfsPath,
    stamp: Metadata,
    driver: Driver,
    entries: BTreeMap<PathBuf, Entry>,
    children: BTreeMap<PathBuf, Vec<PathBuf>>,
    password: Arc<Mutex<Option<Secret>>>,
}
static SESSION: AtomicU64 = AtomicU64::new(1);
fn auth_error(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    // zip represents a missing password as UnsupportedArchive(PASSWORD_REQUIRED).
    if s.contains("password required") {
        return true;
    }
    if s.contains("unsupported") || s.contains("cancel") {
        return false;
    }
    ["password", "passphrase"]
        .iter()
        .any(|word| s.contains(word))
}
/// Allow cancellation during a parser's header reads as well as between entries.
struct CheckedReader {
    inner: Box<dyn ReadSeek>,
    ctx: Context,
}
impl Read for CheckedReader {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.ctx.check().map_err(io::Error::other)?;
        self.inner.read(b)
    }
}
impl std::io::Seek for CheckedReader {
    fn seek(&mut self, p: std::io::SeekFrom) -> io::Result<u64> {
        self.ctx.check().map_err(io::Error::other)?;
        self.inner.seek(p)
    }
}
fn seek_source(source: &VfsPath, ctx: &Context) -> Result<Box<dyn ReadSeek>> {
    Ok(Box::new(CheckedReader {
        inner: source.fs.open_seek(&source.path, ctx)?,
        ctx: ctx.clone(),
    }))
}
fn builder(
    source: &VfsPath,
    password: Option<&str>,
    ctx: &Context,
) -> Result<compress_tools::ArchiveIterator<Box<dyn ReadSeek>>> {
    let mut b = ArchiveIteratorBuilder::new(seek_source(source, ctx)?).mtree_format(false);
    if let Some(p) = password {
        b = b.with_password(ArchivePassword::new(p)?);
    }
    Ok(b.build()?)
}
fn rar_options(password: Option<&str>) -> rars::ArchiveReadOptions<'_> {
    rars::ArchiveReadOptions::with_optional_password(password.map(str::as_bytes))
        .with_rar50_buffered_decode_limit(32 * 1024 * 1024)
}
fn headers(
    source: &VfsPath,
    driver: Driver,
    password: Option<&str>,
    ctx: &Context,
) -> Result<Vec<(String, Metadata)>> {
    let mut entries = vec![];
    let mut add = |name: String, directory: bool, size: u64, modified: SystemTime| -> Result<()> {
        ctx.check()?;
        ensure!(
            entries.len() < 1_000_000,
            "Archive has too many entries (limit 1,000,000)"
        );
        entries.push((
            name,
            Metadata {
                kind: if directory {
                    Kind::Directory
                } else {
                    Kind::File
                },
                size,
                modified,
                permissions: None,
            },
        ));
        Ok(())
    };
    match driver {
        Driver::Zip => {
            let mut archive = zip::ZipArchive::new(seek_source(source, ctx)?)?;
            ensure!(archive.len() <= 1_000_000, "Archive has too many entries");
            for i in 0..archive.len() {
                let e = archive.by_index_raw(i)?;
                let kind = e.unix_mode().unwrap_or(0) & 0o170000;
                ensure!(
                    matches!(kind, 0 | 0o100000 | 0o040000),
                    "Archive contains an unsupported link or special file: {}",
                    e.name()
                );
                add(
                    e.name().to_owned(),
                    e.is_dir(),
                    e.size(),
                    SystemTime::UNIX_EPOCH,
                )?;
            }
        }
        Driver::Gzip => {
            // ISIZE wraps at 4 GiB; obtain the true size without retaining decoded data.
            let mut input =
                flate2::read::MultiGzDecoder::new(source.fs.open_read(&source.path, ctx)?);
            let mut size = 0;
            vfs::copy_stream(&mut input, &mut io::sink(), ctx, |n| size += n)?;
            add(
                source
                    .path
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                false,
                size,
                SystemTime::UNIX_EPOCH,
            )?;
        }
        Driver::SevenZip => {
            let reader = sevenz_rust2::ArchiveReader::new(
                seek_source(source, ctx)?,
                sevenz_rust2::Password::from(password.unwrap_or("")),
            )?;
            for e in &reader.archive().files {
                let kind = (e.windows_attributes >> 16) & 0o170000;
                ensure!(
                    !e.is_anti_item
                        && matches!(kind, 0 | 0o100000 | 0o040000)
                        && e.windows_attributes & 0x400 == 0,
                    "Archive contains an unsupported link or special file: {}",
                    e.name
                );
                add(
                    e.name.clone(),
                    e.is_directory,
                    e.size,
                    SystemTime::UNIX_EPOCH,
                )?;
            }
        }
        Driver::Rar => {
            let local=source.local_path().ok_or_else(||anyhow::anyhow!("This RAR decoder requires a local archive; remote seekable RAR transport is not implemented yet"))?;
            let archive =
                rars::ArchiveReader::read_path_with_options(local, rar_options(password))?;
            if let Some(rar) = archive.as_rar50() {
                ensure!(
                    rar.files().all(|f| f.redirection.is_none()),
                    "Archive contains unsupported link/redirection entries"
                );
            }
            for member in archive.members() {
                let e = member.meta;
                ensure!(
                    matches!(e.file_attr & 0o170000, 0 | 0o100000 | 0o040000),
                    "Archive contains an unsupported link or special file"
                );
                ensure!(
                    !e.is_split_before && !e.is_split_after,
                    "Multi-volume RAR is not supported yet"
                );
                add(
                    String::from_utf8(e.name)?,
                    e.is_directory,
                    e.unpacked_size,
                    SystemTime::UNIX_EPOCH,
                )?;
            }
        }
        Driver::Libarchive => {
            let mut iter = builder(source, password, ctx)?;
            while let Some(item) = iter.next_header() {
                ctx.check()?;
                match item {
                    ArchiveContents::StartOfEntry(name, stat) => {
                        #[allow(clippy::unnecessary_cast)]
                        let kind = stat.st_mode as u32 & 0o170000;
                        ensure!(
                            matches!(kind, 0o040000 | 0o100000),
                            "Archive contains an unsupported link or special file: {name}"
                        );
                        let modified = SystemTime::UNIX_EPOCH
                            .checked_add(Duration::from_secs(stat.st_mtime.max(0) as u64))
                            .unwrap_or(SystemTime::UNIX_EPOCH);
                        add(name, kind == 0o040000, stat.st_size.max(0) as u64, modified)?;
                    }
                    ArchiveContents::Err(e) => return Err(e.into()),
                    _ => {}
                }
            }
            iter.close()?;
        }
    }
    Ok(entries)
}
pub fn open(source: VfsPath, ctx: &Context, progress: impl Fn(u64)) -> Result<VfsPath> {
    let name = source.path.to_string_lossy().to_lowercase();
    let driver = if name.ends_with(".zip") {
        Driver::Zip
    } else if name.ends_with(".7z") {
        Driver::SevenZip
    } else if name.ends_with(".rar") {
        Driver::Rar
    } else if name.ends_with(".gz") && !name.ends_with(".tar.gz") {
        Driver::Gzip
    } else {
        Driver::Libarchive
    };
    let stamp = source.metadata(true, ctx)?;
    let mut password: Option<Secret> = None;
    let list = loop {
        match headers(
            &source,
            driver,
            password.as_deref().map(|s| s.as_str()),
            ctx,
        ) {
            Ok(list) => break list,
            Err(e) if auth_error(&e) => {
                password = Some(ctx.password(&source.display(), password.is_some())?);
            }
            Err(e) => return Err(e),
        }
    };
    let mut entries = BTreeMap::new();
    entries.insert(
        PathBuf::new(),
        Entry {
            metadata: Metadata::directory(),
            member: None,
        },
    );
    for (name, metadata) in list {
        ctx.check()?;
        let path = safe_path(&name)?;
        if path.as_os_str().is_empty() {
            ensure!(
                metadata.kind == Kind::Directory,
                "Invalid empty archive filename"
            );
            continue;
        }
        if let Some(old) = entries.get(&path) {
            ensure!(
                old.member.is_none()
                    && old.metadata.kind == Kind::Directory
                    && metadata.kind == Kind::Directory,
                "Duplicate/conflicting archive path: {name}"
            );
        }
        let mut parent = path.parent();
        while let Some(p) = parent {
            let e = entries.entry(p.to_owned()).or_insert_with(|| Entry {
                metadata: Metadata::directory(),
                member: None,
            });
            ensure!(
                e.metadata.kind == Kind::Directory,
                "File used as archive directory: {}",
                p.display()
            );
            parent = p.parent();
        }
        entries.insert(
            path,
            Entry {
                metadata,
                member: Some(name),
            },
        );
        progress(entries.len() as u64);
    }
    let mut children: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for path in entries.keys() {
        if let Some(parent) = path.parent() {
            children
                .entry(parent.to_owned())
                .or_default()
                .push(path.clone());
        }
    }
    Ok(VfsPath::new(
        Arc::new(Archive {
            id: format!("archive:{}", SESSION.fetch_add(1, Ordering::Relaxed)),
            source,
            stamp,
            driver,
            entries,
            children,
            password: Arc::new(Mutex::new(password)),
        }),
        PathBuf::new(),
    ))
}
/// Owned sink permits decoders with 'static writer factories without unbounded buffers.
#[derive(Clone)]
struct Sink {
    pipe: Option<vfs::StreamWriter>,
    ctx: Context,
}
impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.ctx.check().map_err(io::Error::other)?;
        if let Some(p) = &mut self.pipe {
            p.write(b)
        } else {
            Ok(b.len())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn decode(
    source: &VfsPath,
    driver: Driver,
    member: &str,
    password: Option<&str>,
    ctx: &Context,
    pipe: Option<vfs::StreamWriter>,
) -> Result<()> {
    let mut sink = Sink {
        pipe,
        ctx: ctx.clone(),
    };
    match driver {
        Driver::Zip => {
            let mut archive = zip::ZipArchive::new(seek_source(source, ctx)?)?;
            let mut file = if let Some(key) = password {
                archive.by_name_decrypt(member, key.as_bytes())?
            } else {
                archive.by_name(member)?
            };
            vfs::copy_stream(&mut file, &mut sink, ctx, |_| {})?;
        }
        Driver::Gzip => {
            vfs::copy_stream(
                &mut flate2::read::MultiGzDecoder::new(source.fs.open_read(&source.path, ctx)?),
                &mut sink,
                ctx,
                |_| {},
            )?;
        }
        Driver::SevenZip => {
            let mut reader = sevenz_rust2::ArchiveReader::new(
                seek_source(source, ctx)?,
                sevenz_rust2::Password::from(password.unwrap_or("")),
            )?;
            let mut found = false;
            reader.for_each_entries(|entry, reader| {
                ctx.check().map_err(|e| io::Error::other(e.to_string()))?;
                if entry.name == member {
                    io::copy(reader, &mut sink)?;
                    found = true;
                    Ok(false)
                } else {
                    io::copy(
                        reader,
                        &mut Sink {
                            pipe: None,
                            ctx: ctx.clone(),
                        },
                    )?;
                    Ok(true)
                }
            })?;
            ensure!(found, "Archive member disappeared");
        }
        Driver::Rar => {
            let archive = rars::ArchiveReader::read_path_with_options(
                source
                    .local_path()
                    .ok_or_else(|| anyhow::anyhow!("RAR requires local transport"))?,
                rar_options(password),
            )?;
            let mut found = false;
            let mut stopped = false;
            // Stop at the next header, after the selected member's checksum was checked.
            let result = archive.extract_to_with_options(rar_options(password), |entry| {
                if found {
                    stopped = true;
                    return Err(io::Error::other("End of selected member").into());
                }
                let selected = entry.name == member.as_bytes();
                found = selected;
                Ok(Box::new(if selected {
                    sink.clone()
                } else {
                    Sink {
                        pipe: None,
                        ctx: ctx.clone(),
                    }
                }))
            });
            if let Err(e) = result
                && !stopped
            {
                return Err(e.into());
            }
            ensure!(found, "Archive member disappeared");
        }
        Driver::Libarchive => {
            let wanted = member.to_owned();
            let mut b = ArchiveIteratorBuilder::new(seek_source(source, ctx)?)
                .mtree_format(false)
                .filter(move |name, _| name == wanted);
            if let Some(p) = password {
                b = b.with_password(ArchivePassword::new(p)?);
            }
            let mut iter = b.build()?;
            let mut found = false;
            for item in &mut iter {
                ctx.check()?;
                match item {
                    ArchiveContents::StartOfEntry(_, _) => found = true,
                    ArchiveContents::DataChunk(data) => sink.write_all(&data)?,
                    ArchiveContents::EndOfEntry if found => break,
                    ArchiveContents::Err(e) => return Err(e.into()),
                    _ => {}
                }
            }
            iter.close()?;
            ensure!(found, "Archive member disappeared");
        }
    }
    Ok(())
}
impl FileSystem for Archive {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn label(&self, p: &Path) -> String {
        format!("{}!/{}", self.source.display(), p.display())
    }
    fn metadata(&self, p: &Path, _: bool, ctx: &Context) -> Result<Metadata> {
        ctx.check()?;
        Ok(self
            .entries
            .get(p)
            .ok_or_else(|| anyhow::anyhow!("Archive entry not found"))?
            .metadata
            .clone())
    }
    fn read_dir(&self, p: &Path, ctx: &Context) -> Result<Vec<DirEntry>> {
        ensure!(
            self.metadata(p, false, ctx)?.kind == Kind::Directory,
            "Not a directory"
        );
        self.children
            .get(p)
            .into_iter()
            .flatten()
            .map(|path| {
                ctx.check()?;
                Ok(DirEntry {
                    name: path.file_name().unwrap().to_owned(),
                    metadata: self.entries[path].metadata.clone(),
                })
            })
            .collect()
    }

    fn open_read(&self, p: &Path, ctx: &Context) -> Result<Box<dyn Read + Send>> {
        let entry = self
            .entries
            .get(p)
            .ok_or_else(|| anyhow::anyhow!("Archive entry not found"))?;
        ensure!(
            entry.metadata.kind == Kind::File,
            "Only regular archive files can be read"
        );
        let stamp = self.source.metadata(true, ctx)?;
        ensure!(
            stamp.size == self.stamp.size && stamp.modified == self.stamp.modified,
            "Archive changed; leave and reopen it"
        );
        let source = self.source.clone();
        let driver = self.driver;
        let member = entry.member.clone().unwrap();
        let password = self.password.clone();
        Ok(vfs::stream(ctx.clone(), move |pipe, ctx| {
            // Validate before releasing bytes, so retry cannot concatenate wrong-password output.
            // This bounded pass also checks checksums before an external viewer receives content.
            let mut key = password.lock().unwrap().clone();
            loop {
                match decode(
                    &source,
                    driver,
                    &member,
                    key.as_deref().map(|s| s.as_str()),
                    ctx,
                    None,
                ) {
                    Ok(()) => break,
                    Err(e) if auth_error(&e) => {
                        key = Some(ctx.password(&source.display(), key.is_some())?);
                    }
                    Err(e) => return Err(e),
                }
            }
            *password.lock().unwrap() = key.clone();
            decode(
                &source,
                driver,
                &member,
                key.as_deref().map(|s| s.as_str()),
                ctx,
                Some(pipe),
            )
        }))
    }
    fn mount_parent(&self) -> Option<VfsPath> {
        self.source.parent()
    }
    fn backing_resources(&self) -> Vec<VfsPath> {
        let mut paths = vec![self.source.clone()];
        paths.extend(self.source.fs.backing_resources());
        paths
    }
}
