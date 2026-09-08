use crate::panel::Mount;
use anyhow::{Result, bail};
use compress_tools::{ArchiveContents, ArchiveIterator};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub fn supported(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        ["zip", "rar", "tar", "7z", "gz", "tgz"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}
pub fn safe_path(name: &str) -> Result<PathBuf> {
    // Reject Windows syntax on Unix too, so the same archive has the same safety policy everywhere.
    if name.contains('\\') || name.contains(':') || name.starts_with('/') {
        bail!("Unsafe archive path: {name}");
    }
    let path = Path::new(name);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("Unsafe archive path: {name}");
    }
    Ok(path.to_owned())
}
pub fn open(path: PathBuf, cancel: &AtomicBool, progress: impl Fn(u64)) -> Result<Arc<Mount>> {
    let temp = tempfile::tempdir()?;
    let lower = path.to_string_lossy().to_lowercase();
    if lower.ends_with(".gz") && !lower.ends_with(".tar.gz") {
        let mut decoder = flate2::read::MultiGzDecoder::new(File::open(&path)?);
        let name = path
            .file_stem()
            .ok_or_else(|| anyhow::anyhow!("Missing gzip filename"))?;
        let mut out = File::create(temp.path().join(name))?;
        let mut buf = [0; 65536];
        let mut total = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("Cancelled");
            }
            let n = decoder.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            total += n as u64;
            progress(total);
        }
    } else {
        let mut iter = ArchiveIterator::from_read(File::open(&path)?)?;
        let mut output: Option<File> = None;
        let mut total = 0;
        for item in &mut iter {
            if cancel.load(Ordering::Relaxed) {
                bail!("Cancelled");
            }
            match item {
                ArchiveContents::StartOfEntry(name, stat) => {
                    output = None;
                    let relative = safe_path(&name)?;
                    let target = temp.path().join(relative);
                    #[allow(clippy::unnecessary_cast)] // mode_t is narrower on macOS and Windows.
                    let kind = stat.st_mode as u32 & 0o170000;
                    if kind == 0o040000 {
                        fs::create_dir_all(&target)?;
                    } else if kind == 0o100000 {
                        if let Some(p) = target.parent() {
                            fs::create_dir_all(p)?;
                        }
                        output = Some(File::options().write(true).create_new(true).open(target)?);
                    } else {
                        bail!("Archive contains an unsupported link or special file: {name}");
                    }
                }
                ArchiveContents::DataChunk(bytes) => {
                    if let Some(file) = output.as_mut() {
                        file.write_all(&bytes)?;
                        total += bytes.len() as u64;
                        progress(total);
                    }
                }
                ArchiveContents::EndOfEntry => {
                    output = None;
                }
                ArchiveContents::Err(e) => return Err(e.into()),
            }
        }
        iter.close()?;
    }
    Ok(Arc::new(Mount { temp, source: path }))
}
