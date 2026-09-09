//! Contract tests use an independent provider with no operating-system paths.
use anyhow::{Result, bail};
use mc::{
    archives,
    jobs::{self, Operation},
    vfs::*,
};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Default)]
struct Memory {
    files: Arc<Mutex<BTreeMap<PathBuf, Vec<u8>>>>,
    reads: Arc<AtomicUsize>,
}
struct Output {
    fs: Memory,
    path: PathBuf,
    bytes: Vec<u8>,
}
impl Write for Output {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl WriteHandle for Output {
    fn commit(self: Box<Self>, _: &Metadata) -> Result<()> {
        self.fs.files.lock().unwrap().insert(self.path, self.bytes);
        Ok(())
    }
}
impl FileSystem for Memory {
    fn id(&self) -> String {
        format!("memory:{:p}", Arc::as_ptr(&self.files))
    }
    fn label(&self, p: &Path) -> String {
        format!("memory:///{p}", p = p.display())
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            write: true,
            seek: true,
            ..Default::default()
        }
    }
    fn metadata(&self, p: &Path, _: bool, _: &Context) -> Result<Metadata> {
        if p.as_os_str().is_empty() {
            return Ok(Metadata::directory());
        }
        let f = self.files.lock().unwrap();
        let Some(b) = f.get(p) else {
            bail!("Missing member");
        };
        Ok(Metadata {
            kind: Kind::File,
            size: b.len() as u64,
            modified: std::time::SystemTime::UNIX_EPOCH,
            permissions: None,
        })
    }
    fn read_dir(&self, _: &Path, _: &Context) -> Result<Vec<DirEntry>> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .iter()
            .map(|(p, b)| DirEntry {
                name: p.as_os_str().to_owned(),
                metadata: Metadata {
                    kind: Kind::File,
                    size: b.len() as u64,
                    modified: std::time::SystemTime::UNIX_EPOCH,
                    permissions: None,
                },
            })
            .collect())
    }
    fn open_read(&self, p: &Path, _: &Context) -> Result<Box<dyn Read + Send>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(io::Cursor::new(
            self.files.lock().unwrap()[p].clone(),
        )))
    }
    fn open_seek(&self, p: &Path, _: &Context) -> Result<Box<dyn ReadSeek>> {
        Ok(Box::new(io::Cursor::new(
            self.files.lock().unwrap()[p].clone(),
        )))
    }
    fn create(&self, p: &Path, _: bool, _: &Context) -> Result<Box<dyn WriteHandle>> {
        Ok(Box::new(Output {
            fs: self.clone(),
            path: p.to_owned(),
            bytes: vec![],
        }))
    }
}
fn wait(job: &jobs::Job) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let p = job.progress.lock().unwrap();
        if p.done {
            assert!(p.error.is_none(), "{:?}", p.error);
            return;
        }
        drop(p);
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn panels_and_jobs_work_between_arbitrary_providers() {
    let source = Arc::new(Memory::default());
    source
        .files
        .lock()
        .unwrap()
        .insert("hello".into(), b"vfs data".to_vec());
    let dest = Arc::new(Memory::default());
    let root = VfsPath::new(source.clone(), PathBuf::new());
    let target = VfsPath::new(dest.clone(), PathBuf::new());
    assert_ne!(root, target);
    assert!(!jobs::overlaps(
        std::slice::from_ref(&root),
        std::slice::from_ref(&target)
    ));
    let mut panel = mc::panel::Panel::new(root.clone());
    let until = Instant::now() + Duration::from_secs(3);
    while panel.loading {
        panel.poll();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(panel.entries[0].name, "hello");
    assert_eq!(
        source.reads.load(Ordering::Relaxed),
        0,
        "listing must not open files"
    );
    wait(&jobs::start(
        Operation::Copy,
        vec![root.join("hello")],
        target,
        Context::default(),
    ));
    assert_eq!(dest.files.lock().unwrap()[Path::new("hello")], b"vfs data");
}
#[test]
fn archive_can_mount_over_a_seekable_nonlocal_provider() {
    let source = Arc::new(Memory::default());
    let mut zip = zip::ZipWriter::new(io::Cursor::new(vec![]));
    zip.start_file("hello", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"virtual archive").unwrap();
    let data = zip.finish().unwrap().into_inner();
    for name in [
        "files.zip",
        "app.JAR",
        "document.DOCX",
        "slides.pptx",
        "workbook.xlsx",
    ] {
        source
            .files
            .lock()
            .unwrap()
            .insert(name.into(), data.clone());
        let path = VfsPath::new(source.clone(), PathBuf::from(name));
        assert!(archives::supported(&path));
        let root = archives::open(path.clone(), &Context::default(), |_| {}).unwrap();
        assert_eq!(root.parent().unwrap(), path.parent().unwrap());
        assert!(root.local_path().is_none());
        let file = root.join("hello");
        let mut reader = file.fs.open_read(&file.path, &Context::default()).unwrap();
        drop(root);
        drop(file);
        let mut bytes = vec![];
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"virtual archive");
    }
}
#[test]
fn dropping_a_bounded_stream_stops_its_producer() {
    let (done, rx) = std::sync::mpsc::channel();
    let reader = stream(Context::default(), move |mut writer, _| {
        while writer.write_all(&[1; 64 * 1024]).is_ok() {}
        let _ = done.send(());
        Ok(())
    });
    drop(reader);
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
}
#[test]
fn cancellation_interrupts_password_wait() {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = Context {
        auth: Some(tx),
        ..Default::default()
    };
    let cancel = ctx.cancel.clone();
    let thread = std::thread::spawn(move || ctx.password("test", false));
    let request = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    cancel.store(true, Ordering::Relaxed);
    assert!(thread.join().unwrap().is_err());
    drop(request);
}
