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
        if let Some(p) = &m.permissions {
            self.file.as_file().set_permissions(p.clone())?;
        }
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
        for entry in fs::read_dir(p)? {
            ctx.check()?;
            let e = entry?;
            match fs::symlink_metadata(e.path()) {
                Ok(m) => entries.push(DirEntry {
                    name: e.file_name(),
                    metadata: Self::meta(m),
                }),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(entries)
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
        if let Some(perms) = &m.permissions {
            fs::set_permissions(p, perms.clone())?;
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
    fs::rename(a, b)?;
    Ok(())
}
