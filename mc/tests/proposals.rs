use mc::{
    archives,
    jobs::{self, Decision, Operation},
    vfs::*,
};
use std::{
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};
fn finish(job: &jobs::Job) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut p = job.progress.lock().unwrap();
        if let Some(c) = p.conflict.take() {
            c.reply.send(Decision::Overwrite).unwrap();
        }
        if p.done {
            assert!(p.error.is_none(), "{:?}", p.error);
            return;
        }
        drop(p);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn local_copy_preserves_file_and_directory_metadata() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("file"), b"payload").unwrap();
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    for p in [&source, &source.join("file")] {
        filetime::set_file_mtime(p, filetime::FileTime::from_system_time(time)).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(source.join("file"), std::fs::Permissions::from_mode(0o640))
            .unwrap();
    }
    let target = d.path().join("target");
    finish(&jobs::start(
        Operation::Copy,
        vec![source.into()],
        target.clone().into(),
        Context::default(),
    ));
    for p in [&target, &target.join("file")] {
        assert_eq!(std::fs::metadata(p).unwrap().modified().unwrap(), time);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(target.join("file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }
}
#[test]
fn explicit_retry_skips_completed_sources_and_confirms_overwrite() {
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    std::fs::write(&a, b"first").unwrap();
    let failed = jobs::start(
        Operation::Copy,
        vec![a.into(), b.clone().into()],
        out.clone().into(),
        Context::default(),
    );
    let until = Instant::now() + Duration::from_secs(5);
    while !failed.progress.lock().unwrap().done {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(failed.progress.lock().unwrap().error.is_some());
    assert_eq!(failed.progress.lock().unwrap().completed_sources, 1);
    std::fs::write(out.join("a"), b"keep completed edit").unwrap();
    std::fs::write(&b, b"second").unwrap();
    std::fs::write(out.join("b"), b"existing").unwrap();
    let retry = jobs::retry(&failed, Context::default());
    let until = Instant::now() + Duration::from_secs(5);
    while retry.progress.lock().unwrap().conflict.is_none() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(std::fs::read(out.join("b")).unwrap(), b"existing");
    finish(&retry);
    assert_eq!(
        std::fs::read(out.join("a")).unwrap(),
        b"keep completed edit"
    );
    assert_eq!(std::fs::read(out.join("b")).unwrap(), b"second");
}
fn zip_member(name: &str, bytes: &[u8], encrypted: bool) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    let options = if encrypted {
        options.with_aes_encryption(zip::AesMode::Aes256, "correct")
    } else {
        options
    };
    writer.start_file(name, options).unwrap();
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap().into_inner()
}
#[test]
fn nested_encrypted_zip_and_rar_browse_without_extracted_files() {
    let d = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel::<AuthRequest>();
    std::thread::spawn(move || {
        while let Ok(r) = rx.recv() {
            let _ = r.reply.send(Some(Secret::new("correct".into())));
        }
    });
    let ctx = Context {
        auth: Some(tx),
        ..Default::default()
    };
    for (name, bytes) in [
        (
            "inner.zip",
            zip_member("file.txt", b"nested payload", false),
        ),
        (
            "inner.rar",
            std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tree.rar"))
                .unwrap(),
        ),
    ] {
        let path = d.path().join("outer.zip");
        std::fs::write(&path, zip_member(name, &bytes, true)).unwrap();
        let outer = archives::open(path.clone().into(), &ctx, |_| {}).unwrap();
        let inner = archives::open(outer.join(name), &ctx, |_| {}).unwrap();
        let locks = inner.fs.backing_resources();
        assert!(locks.iter().any(|p| p.local_path().as_ref() == Some(&path)));
        let mut pending = vec![inner];
        let mut files = 0;
        while let Some(p) = pending.pop() {
            for (p, m) in p.read_dir(&ctx).unwrap() {
                if m.kind == Kind::Directory {
                    pending.push(p);
                } else {
                    let mut bytes = vec![];
                    p.fs.open_read(&p.path, &ctx)
                        .unwrap()
                        .read_to_end(&mut bytes)
                        .unwrap();
                    assert_eq!(bytes.len() as u64, m.size);
                    files += 1;
                }
            }
        }
        assert!(files > 0);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
#[test]
fn seek_cache_reuses_bytes_and_enforces_size_and_cancellation() {
    let ctx = Context::default();
    let mut reader =
        cache::SeekCache::new(Box::new(Cursor::new(b"abcdef")), 6, ctx.clone()).unwrap();
    reader.seek(SeekFrom::End(-2)).unwrap();
    let mut s = String::new();
    reader.read_to_string(&mut s).unwrap();
    assert_eq!(s, "ef");
    reader.seek(SeekFrom::Start(0)).unwrap();
    s.clear();
    reader.read_to_string(&mut s).unwrap();
    assert_eq!(s, "abcdef");
    assert!(reader.seek(SeekFrom::End(-7)).is_err());
    assert!(
        cache::SeekCache::new(Box::new(std::io::empty()), cache::LIMIT + 1, ctx.clone()).is_err()
    );
    ctx.cancel.store(true, Ordering::Relaxed);
    assert!(reader.seek(SeekFrom::Start(0)).is_err());
}
#[test]
fn panel_delivers_batches_before_listing_finishes_and_discards_stale_work() {
    struct Slow {
        release: Arc<AtomicBool>,
    }
    impl FileSystem for Slow {
        fn id(&self) -> String {
            "slow".into()
        }
        fn label(&self, p: &Path) -> String {
            p.display().to_string()
        }
        fn metadata(&self, _: &Path, _: bool, _: &Context) -> anyhow::Result<Metadata> {
            Ok(Metadata::directory())
        }
        fn read_dir(&self, _: &Path, _: &Context) -> anyhow::Result<Vec<DirEntry>> {
            panic!("must use incremental listings")
        }
        fn open_read(&self, _: &Path, _: &Context) -> anyhow::Result<Box<dyn Read + Send>> {
            unreachable!()
        }
        fn visit_dir(
            &self,
            _: &Path,
            ctx: &Context,
            emit: &mut dyn FnMut(DirEntry) -> anyhow::Result<()>,
        ) -> anyhow::Result<()> {
            for i in 0..256 {
                emit(DirEntry {
                    name: format!("file-{i:04}").into(),
                    metadata: Metadata {
                        kind: Kind::File,
                        size: 1,
                        ..Metadata::directory()
                    },
                })?;
            }
            while !self.release.load(Ordering::Relaxed) {
                ctx.check()?;
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        }
    }
    let release = Arc::new(AtomicBool::new(false));
    let mut panel = mc::panel::Panel::new(VfsPath::new(
        Arc::new(Slow {
            release: release.clone(),
        }),
        "/".into(),
    ));
    let until = Instant::now() + Duration::from_secs(5);
    while panel.entries.is_empty() {
        panel.poll();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(panel.loading);
    assert_eq!(panel.entries.len(), 256);
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("new"), b"new").unwrap();
    panel.navigate(d.path().into());
    release.store(true, Ordering::Relaxed);
    while panel.loading {
        panel.poll();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(panel.entries.len(), 1);
    assert_eq!(panel.entries[0].name, "new");
}

#[test]
fn archive_copy_preserves_stored_timestamps_and_unix_modes() {
    use chrono::TimeZone;
    let d = tempfile::tempdir().unwrap();
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    let tar_path = d.path().join("stored.tar");
    let mut tar = tar::Builder::new(std::fs::File::create(&tar_path).unwrap());
    let mut header = tar::Header::new_gnu();
    header.set_size(7);
    header.set_mode(0o750);
    header.set_mtime(1_600_000_000);
    header.set_cksum();
    tar.append_data(&mut header, "file", &b"payload"[..])
        .unwrap();
    tar.finish().unwrap();
    drop(tar);
    let zip_path = d.path().join("stored.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let date = zip::DateTime::from_date_and_time(2020, 9, 13, 12, 26, 40).unwrap();
    zip.start_file(
        "file",
        zip::write::SimpleFileOptions::default()
            .unix_permissions(0o750)
            .last_modified_time(date),
    )
    .unwrap();
    zip.write_all(b"payload").unwrap();
    zip.finish().unwrap();
    let zip_time = SystemTime::from(
        chrono::Local
            .with_ymd_and_hms(2020, 9, 13, 12, 26, 40)
            .earliest()
            .unwrap(),
    );
    for (i, (path, expected)) in [(tar_path, time), (zip_path, zip_time)]
        .into_iter()
        .enumerate()
    {
        let archive = archives::open(path.into(), &Context::default(), |_| {}).unwrap();
        let target = d.path().join(format!("out{i}"));
        finish(&jobs::start(
            Operation::Copy,
            vec![archive.join("file")],
            target.clone().into(),
            Context::default(),
        ));
        let metadata = std::fs::metadata(target).unwrap();
        assert_eq!(metadata.modified().unwrap(), expected);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o750);
        }
    }
}
