use mc::{
    jobs::{self, Decision, Operation},
    vfs::*,
};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct InterruptedSource {
    fail: bool,
    removed: Arc<AtomicBool>,
}
impl FileSystem for InterruptedSource {
    fn id(&self) -> String {
        "test-remote-source".into()
    }
    fn label(&self, _: &Path) -> String {
        "test-remote://source".into()
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn lock_path(&self, _: &Path) -> PathBuf {
        "/".into()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            write: true,
            ..Default::default()
        }
    }
    fn metadata(&self, _: &Path, _: bool, _: &Context) -> anyhow::Result<Metadata> {
        Ok(Metadata {
            kind: Kind::File,
            size: 100,
            modified: std::time::SystemTime::UNIX_EPOCH,
            permissions: None,
        })
    }
    fn read_dir(&self, _: &Path, _: &Context) -> anyhow::Result<Vec<DirEntry>> {
        unreachable!()
    }
    fn open_read(&self, _: &Path, _: &Context) -> anyhow::Result<Box<dyn Read + Send>> {
        struct Broken {
            sent: bool,
            fail: bool,
        }
        impl Read for Broken {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if !self.sent {
                    self.sent = true;
                    out[..3].copy_from_slice(b"new");
                    return Ok(3);
                }
                if self.fail {
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "Injected disconnect",
                    ))
                } else {
                    Ok(0)
                }
            }
        }
        Ok(Box::new(Broken {
            sent: false,
            fail: self.fail,
        }))
    }
    fn remove(&self, _: &Path, _: bool, _: &Context) -> anyhow::Result<()> {
        self.removed.store(true, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn interrupted_remote_moves_preserve_existing_destination_and_source() {
    for fail in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"original").unwrap();
        let removed = Arc::new(AtomicBool::new(false));
        let source = VfsPath::new(
            Arc::new(InterruptedSource {
                fail,
                removed: removed.clone(),
            }),
            "/source".into(),
        );
        let job = jobs::start(
            Operation::Move,
            vec![source],
            target.clone().into(),
            Context::default(),
        );
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let mut p = job.progress.lock().unwrap();
            if let Some(conflict) = p.conflict.take() {
                conflict.reply.send(Decision::Overwrite).unwrap();
            }
            if p.done {
                let error = p.error.as_ref().unwrap();
                assert!(
                    error.contains(if fail {
                        "Injected disconnect"
                    } else {
                        "Source size changed"
                    }),
                    "{error}"
                );
                break;
            }
            drop(p);
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert!(!removed.load(Ordering::Relaxed));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "Incomplete staging must be removed"
        );
    }
}
