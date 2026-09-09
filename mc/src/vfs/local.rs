use super::*;
use std::fs;
pub struct Local;
impl Local {
    fn meta(m: fs::Metadata) -> Metadata {
        Metadata {
            kind: if m.is_symlink() {
                Kind::Symlink
            } else if m.is_dir() {
                Kind::Directory
            } else if m.is_file() {
                Kind::File
            } else {
                Kind::Special
            },
            size: m.len(),
            modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            permissions: Some(m.permissions()),
        }
    }
}
struct Staged {
    file: tempfile::NamedTempFile,
    path: PathBuf,
    overwrite: bool,
}
impl Write for Staged {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.file.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl WriteHandle for Staged {
    fn commit(self: Box<Self>, m: &Metadata) -> Result<()> {
        if let Some(p) = mode_permissions(permission_mode(m)).or_else(|| m.permissions.clone()) {
            self.file.as_file().set_permissions(p)?;
        }
        self.file.as_file().set_modified(m.modified)?;
        self.file.as_file().sync_all()?;
        if self.overwrite {
            self.file.persist(&self.path)?;
        } else {
            self.file.persist_noclobber(&self.path)?;
        }
        Ok(())
    }
}
impl FileSystem for Local {
    fn id(&self) -> String {
        "file".into()
    }
    fn label(&self, p: &Path) -> String {
        p.display().to_string()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            write: true,
            trash: true,
            symlink: true,
            seek: true,
        }
    }
    fn metadata(&self, p: &Path, follow: bool, _: &Context) -> Result<Metadata> {
        Ok(Self::meta(if follow {
            fs::metadata(p)?
        } else {
            fs::symlink_metadata(p)?
        }))
    }
    fn read_dir(&self, p: &Path, ctx: &Context) -> Result<Vec<DirEntry>> {
        let mut entries = vec![];
        self.visit_dir(p, ctx, &mut |e| {
            entries.push(e);
            Ok(())
        })?;
        Ok(entries)
    }
    fn visit_dir(
        &self,
        p: &Path,
        ctx: &Context,
        emit: &mut dyn FnMut(DirEntry) -> Result<()>,
    ) -> Result<()> {
        for entry in fs::read_dir(p)? {
            ctx.check()?;
            let e = entry?;
            match fs::symlink_metadata(e.path()) {
                Ok(m) => emit(DirEntry {
                    name: e.file_name(),
                    metadata: Self::meta(m),
                })?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    fn open_read(&self, p: &Path, _: &Context) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(fs::File::open(p)?))
    }
    fn open_seek(&self, p: &Path, _: &Context) -> Result<Box<dyn ReadSeek>> {
        Ok(Box::new(fs::File::open(p)?))
    }
    fn create(&self, p: &Path, overwrite: bool, _: &Context) -> Result<Box<dyn WriteHandle>> {
        Ok(Box::new(Staged {
            file: tempfile::NamedTempFile::new_in(
                p.parent()
                    .ok_or_else(|| anyhow::anyhow!("Missing destination parent"))?,
            )?,
            path: p.to_owned(),
            overwrite,
        }))
    }
    fn mkdir(&self, p: &Path, _: &Context) -> Result<()> {
        Ok(fs::create_dir(p)?)
    }
    fn remove(&self, p: &Path, dir: bool, _: &Context) -> Result<()> {
        if dir {
            fs::remove_dir(p)?;
        } else {
            fs::remove_file(p)?;
        }
        Ok(())
    }
    fn trash(&self, p: &Path, _: &Context) -> Result<()> {
        Ok(trash::delete(p)?)
    }
    fn rename(&self, a: &Path, b: &Path, _: &Context) -> Result<()> {
        rename_noreplace(a, b)
    }
    fn read_link(&self, p: &Path, _: &Context) -> Result<PathBuf> {
        Ok(fs::read_link(p)?)
    }
    fn symlink(
        &self,
        target: &Path,
        to: &Path,
        directory: bool,
        overwrite: bool,
        _: &Context,
    ) -> Result<()> {
        let temp = tempfile::tempdir_in(to.parent().ok_or_else(|| anyhow::anyhow!("No parent"))?)?;
        let staged = temp.path().join("link");
        #[cfg(unix)]
        {
            let _ = directory;
            std::os::unix::fs::symlink(target, &staged)?;
        }
        #[cfg(windows)]
        {
            if directory {
                std::os::windows::fs::symlink_dir(target, &staged)?;
            } else {
                std::os::windows::fs::symlink_file(target, &staged)?;
            }
        }
        if overwrite {
            fs::rename(staged, to)?;
        } else {
            rename_noreplace(&staged, to)?;
        }
        Ok(())
    }
    fn set_metadata(&self, p: &Path, m: &Metadata, _: &Context) -> Result<()> {
        filetime::set_file_mtime(p, filetime::FileTime::from_system_time(m.modified))?;
        if let Some(perms) = mode_permissions(permission_mode(m)).or_else(|| m.permissions.clone())
        {
            fs::set_permissions(p, perms)?;
        }
        Ok(())
    }
    fn canonical(&self, p: &Path) -> Result<PathBuf> {
        let mut ancestor = p;
        let mut suffix = vec![];
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
        let mut path = fs::canonicalize(ancestor)?;
        for s in suffix.into_iter().rev() {
            path.push(s);
        }
        Ok(path)
    }
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let (Ok(a), Ok(b)) = (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
                return a.dev() == b.dev() && a.ino() == b.ino();
            }
        }
        self.canonical(a)
            .ok()
            .zip(self.canonical(b).ok())
            .is_some_and(|(a, b)| a == b)
    }
    fn local_path(&self, p: &Path) -> Option<PathBuf> {
        Some(p.to_owned())
    }
}
fn rename_noreplace(a: &Path, b: &Path) -> Result<()> {
    #[cfg(unix)]
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        a,
        rustix::fs::CWD,
        b,
        rustix::fs::RenameFlags::NOREPLACE,
    )?;
    #[cfg(windows)]
    {
        use anyhow::Context as _;
        use std::os::windows::ffi::OsStrExt;
        fn wide(path: &Path) -> Result<Vec<u16>> {
            // Canonicalize only the parent: retain symlink identity and get the
            // Windows extended-length prefix without requiring the target to exist.
            let absolute = std::path::absolute(path)?;
            let path = fs::canonicalize(absolute.parent().context("No parent")?)?
                .join(absolute.file_name().context("No filename")?);
            let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
            anyhow::ensure!(!value.contains(&0), "Path contains a NUL character");
            value.push(0);
            Ok(value)
        }
        let from = wide(a)?;
        let to = wide(b)?;
        // std::fs::rename replaces existing files on Windows. Flags=0 omits
        // MOVEFILE_REPLACE_EXISTING, enforcing no-replace in the OS call itself.
        // SAFETY: both pointers reference live, NUL-terminated UTF-16 strings.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
