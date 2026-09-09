use mc::vfs::{Context, Kind, Metadata, Secret, remote};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[test]
fn remote_urls_reject_credentials_and_control_characters() {
    for url in [
        "ftp://user:secret@host/",
        "sftp://host/path?password=x",
        "ssh://host/#fragment",
        "ftp://host/a%0d%0aDELE%20file",
        "ssh://user%0a@host/",
        "ssh://host/a%5cb",
        "https://host/",
        "ftp://ho\nst/",
    ] {
        assert!(remote::Endpoint::parse(url).is_err(), "{url}");
    }
    let url = remote::Endpoint::parse("sftp://test%40example@[::1]:2222/a%20b/../c").unwrap();
    assert_eq!(url.host, "::1");
    assert_eq!(url.port, 2222);
    assert_eq!(url.user, "test@example");
    assert_eq!(url.path, Path::new("/c"));
}

fn context() -> Context {
    let (tx, rx) = mpsc::channel::<mc::vfs::AuthRequest>();
    std::thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            assert!(!request.resource.contains("test-password"));
            let _ = request.reply.send(Some(Secret::new(
                if request.resource.contains("One-time code") {
                    "123456"
                } else {
                    "test-password"
                }
                .into(),
            )));
        }
    });
    Context {
        auth: Some(tx),
        ..Default::default()
    }
}

#[test]
fn remote_job_lock_preflight_never_contacts_the_server() {
    use mc::vfs::{DirEntry, FileSystem, VfsPath};
    use std::path::PathBuf;
    struct NoIo;
    impl FileSystem for NoIo {
        fn id(&self) -> String {
            "ssh://test@host:22".into()
        }
        fn label(&self, _: &Path) -> String {
            self.id()
        }
        fn is_remote(&self) -> bool {
            true
        }
        fn lock_path(&self, _: &Path) -> PathBuf {
            "/".into()
        }
        fn metadata(&self, _: &Path, _: bool, _: &Context) -> anyhow::Result<Metadata> {
            panic!("UI performed remote metadata I/O")
        }
        fn read_dir(&self, _: &Path, _: &Context) -> anyhow::Result<Vec<DirEntry>> {
            unreachable!()
        }
        fn open_read(&self, _: &Path, _: &Context) -> anyhow::Result<Box<dyn Read + Send>> {
            unreachable!()
        }
        fn canonical(&self, _: &Path) -> anyhow::Result<PathBuf> {
            panic!("UI performed remote canonicalization")
        }
    }
    let from = VfsPath::new(Arc::new(NoIo), "/from".into());
    let to = from.join("elsewhere");
    let locks = mc::jobs::resources(mc::jobs::Operation::Copy, &[from], &to);
    assert!(locks.iter().all(|p| p.path == Path::new("/")));
}

fn finish(job: &mc::jobs::Job) {
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while !job.progress.lock().unwrap().done {
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    let error = job.progress.lock().unwrap().error.clone();
    assert!(error.is_none(), "{error:?}");
}

#[test]
#[ignore = "run via tests/remote_servers.py against disposable local servers"]
fn remote_server_contracts() {
    let ctx = context();
    for key in ["MC_TEST_FTP", "MC_TEST_SFTP", "MC_TEST_SSH"] {
        let uri = std::env::var(key).unwrap();
        let root = remote::connect(&uri, &ctx).unwrap_or_else(|e| panic!("{key}: {e:#}"));
        assert!(!root.display().contains("test-password"));
        let entries = root.read_dir(&ctx).unwrap();
        assert!(
            entries
                .iter()
                .any(|(p, m)| p.file_name().unwrap() == "hello.txt" && m.size == 11)
        );
        assert_eq!(
            root.join("folder").metadata(false, &ctx).unwrap().kind,
            Kind::Directory
        );
        let input = root.join("hello.txt");
        let mut reader = input.fs.open_seek(&input.path, &ctx).unwrap();
        reader.seek(SeekFrom::End(-5)).unwrap();
        let mut tail = String::new();
        reader.read_to_string(&mut tail).unwrap();
        assert_eq!(tail, "world");
        reader.seek(SeekFrom::Start(0)).unwrap();
        let mut all = String::new();
        reader.read_to_string(&mut all).unwrap();
        assert_eq!(all, "hello world");
        drop(reader);

        let metadata = Metadata {
            kind: Kind::File,
            size: 7,
            modified: std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000),
            permissions: mc::vfs::mode_permissions(Some(0o640)),
        };
        let target = root.join("written space ' quote.txt");
        let mut writer = target.fs.create(&target.path, false, &ctx).unwrap();
        writer.write_all(b"payload").unwrap();
        assert!(target.metadata(false, &ctx).is_err());
        writer.commit(&metadata).unwrap();
        let stored = target.metadata(false, &ctx).unwrap();
        assert_eq!(
            stored
                .modified
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1_600_000_000
        );
        if key != "MC_TEST_FTP" {
            assert_eq!(
                mc::vfs::permission_mode(&stored),
                mc::vfs::permission_mode(&metadata)
            );
        }
        let rar = mc::archives::open(root.join("sample.rar"), &ctx, |_| {}).unwrap();
        let mut pending = vec![rar];
        let mut count = 0;
        while let Some(path) = pending.pop() {
            for (p, m) in path.read_dir(&ctx).unwrap() {
                if m.kind == Kind::Directory {
                    pending.push(p);
                } else {
                    let mut bytes = vec![];
                    p.fs.open_read(&p.path, &ctx)
                        .unwrap()
                        .read_to_end(&mut bytes)
                        .unwrap();
                    assert_eq!(bytes.len() as u64, m.size);
                    count += 1;
                }
            }
        }
        assert!(count > 0);

        let mut data = String::new();
        target
            .fs
            .open_read(&target.path, &ctx)
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "payload");
        let mut duplicate = target.fs.create(&target.path, false, &ctx).unwrap();
        duplicate.write_all(b"badbad!").unwrap();
        assert!(duplicate.commit(&metadata).is_err());
        let mut data = String::new();
        target
            .fs
            .open_read(&target.path, &ctx)
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(
            data, "payload",
            "no-clobber must preserve existing contents"
        );

        let mut short = target.fs.create(&target.path, true, &ctx).unwrap();
        assert!(short.staging_location().is_some());
        short.write_all(b"bad").unwrap();
        assert!(
            short.commit(&metadata).is_err(),
            "Short upload must not replace an existing file"
        );
        let mut data = String::new();
        target
            .fs
            .open_read(&target.path, &ctx)
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "payload");
        let mut replacement = target.fs.create(&target.path, true, &ctx).unwrap();
        replacement.write_all(b"updated").unwrap();
        let result = replacement.commit(&metadata);
        if key == "MC_TEST_SFTP" {
            assert!(result.is_err(), "fixture rejects SFTP v3 overwrite");
        } else {
            result.unwrap();
        }
        if key != "MC_TEST_SSH" {
            let marker = root.join("fault-truncate-upload");
            marker.fs.mkdir(&marker.path, &ctx).unwrap();
            let truncated = root.join("truncated.txt");
            let mut writer = truncated.fs.create(&truncated.path, false, &ctx).unwrap();
            writer.write_all(b"payload").unwrap();
            let error = writer.commit(&metadata).unwrap_err();
            assert!(
                format!("{error:#}").contains("unexpected byte count"),
                "{error:#}"
            );
            assert!(truncated.metadata(false, &ctx).is_err());
            marker.fs.remove(&marker.path, true, &ctx).unwrap();
        }
        if key == "MC_TEST_FTP" {
            let uncertain = root.join("reply-lost.txt");
            let mut writer = uncertain.fs.create(&uncertain.path, false, &ctx).unwrap();
            writer.write_all(b"payload").unwrap();
            let error = writer.commit(&metadata).unwrap_err();
            assert!(
                format!("{error:#}").contains("could not be confirmed"),
                "{error:#}"
            );
            let mut bytes = String::new();
            uncertain
                .fs
                .open_read(&uncertain.path, &ctx)
                .unwrap()
                .read_to_string(&mut bytes)
                .unwrap();
            assert_eq!(
                bytes, "payload",
                "The server completed the rename even though its response was lost"
            );
            uncertain.fs.remove(&uncertain.path, false, &ctx).unwrap();
        }

        let aborted = root.join("aborted.txt");
        let mut partial = aborted.fs.create(&aborted.path, false, &ctx).unwrap();
        partial.write_all(b"partial").unwrap();
        drop(partial);
        assert!(aborted.metadata(false, &ctx).is_err());
        let cancelled = Context {
            cancel: Arc::new(AtomicBool::new(false)),
            ..ctx.clone()
        };
        let mut partial = aborted.fs.create(&aborted.path, false, &cancelled).unwrap();
        partial.write_all(b"partial").unwrap();
        cancelled.cancel.store(true, Ordering::Relaxed);
        assert!(partial.commit(&metadata).is_err());
        assert!(aborted.metadata(false, &ctx).is_err());

        let folder = root.join("made");
        folder.fs.mkdir(&folder.path, &ctx).unwrap();
        folder.fs.remove(&folder.path, true, &ctx).unwrap();
        assert!(folder.fs.trash(&target.path, &ctx).is_err());
        let local = tempfile::tempdir().unwrap();
        let job = mc::jobs::start(
            mc::jobs::Operation::Copy,
            vec![input.clone()],
            local.path().into(),
            ctx.clone(),
        );
        let until = std::time::Instant::now() + Duration::from_secs(30);
        while !job.progress.lock().unwrap().done {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            job.progress.lock().unwrap().error.is_none(),
            "{:?}",
            job.progress.lock().unwrap().error
        );
        assert_eq!(
            std::fs::read(local.path().join("hello.txt")).unwrap(),
            b"hello world"
        );
        let archive = mc::archives::open(root.join("sample.zip"), &ctx, |_| {}).unwrap();
        let member = archive.join("inside.txt");
        let mut data = String::new();
        member
            .fs
            .open_read(&member.path, &ctx)
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "archive payload");
        // Recursive uploads, same-provider moves, and recursive permanent deletes
        // exercise the actual background worker, not just transport handles.
        std::fs::create_dir(local.path().join("upload")).unwrap();
        std::fs::write(local.path().join("upload/nested.txt"), b"nested").unwrap();
        finish(&mc::jobs::start(
            mc::jobs::Operation::Copy,
            vec![local.path().join("upload").into()],
            root.clone(),
            ctx.clone(),
        ));
        finish(&mc::jobs::start(
            mc::jobs::Operation::Move,
            vec![root.join("upload")],
            root.join("renamed"),
            ctx.clone(),
        ));
        assert!(root.join("upload").metadata(false, &ctx).is_err());
        assert_eq!(
            root.join("renamed/nested.txt")
                .metadata(false, &ctx)
                .unwrap()
                .size,
            6
        );
        finish(&mc::jobs::start(
            mc::jobs::Operation::Delete,
            vec![root.join("renamed")],
            root.clone(),
            ctx.clone(),
        ));
        assert!(root.join("renamed").metadata(false, &ctx).is_err());
        // Handles and archive mounts retain the remote session after panels leave it.
        let mut retained = input.fs.open_read(&input.path, &ctx).unwrap();
        target.fs.remove(&target.path, false, &ctx).unwrap();
        drop(root);
        let mut data = String::new();
        retained.read_to_string(&mut data).unwrap();
        assert_eq!(data, "hello world");
    }
    let unknown = std::env::var("MC_TEST_UNKNOWN_SSH").unwrap();
    let error = remote::connect(&unknown, &Context::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("not trusted"), "{error}");
    let known =
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".ssh/known_hosts");
    let before = std::fs::read(&known).unwrap();
    let (tx, rx) = mpsc::channel::<mc::vfs::AuthRequest>();
    std::thread::spawn(move || {
        while let Ok(r) = rx.recv() {
            let answer = if r.confirmation {
                assert!(r.resource.contains("SHA256:"));
                "trust"
            } else {
                "test-password"
            };
            let _ = r.reply.send(Some(Secret::new(answer.into())));
        }
    });
    let trusted = Context {
        auth: Some(tx),
        ..Default::default()
    };
    remote::connect_path(&unknown, &trusted).unwrap();
    assert_eq!(
        std::fs::read(&known).unwrap(),
        before,
        "Trust must be session-only"
    );
    let config = known.parent().unwrap().join("config");
    std::fs::write(&config, "Host *\n StrictHostKeyChecking yes\n").unwrap();
    let error = remote::connect_path(&unknown, &trusted)
        .unwrap_err()
        .to_string();
    assert!(error.contains("StrictHostKeyChecking"));
    std::fs::remove_file(config).unwrap();
    let mfa = std::env::var("MC_TEST_MFA").unwrap();
    remote::connect(&mfa, &ctx).unwrap();
    remote::connect(&mfa.replace("mfa@", "mfa-key@"), &ctx).unwrap();
    let changed = std::env::var("MC_TEST_CHANGED_SSH").unwrap();
    let error = remote::connect(&changed, &ctx).unwrap_err().to_string();
    assert!(error.contains("host key changed"), "{error}");
}

#[test]
#[ignore = "run via tests/openssh_server.py with an isolated OpenSSH daemon"]
fn openssh_contracts() {
    let endpoint = std::env::var("MC_TEST_OPENSSH").unwrap();
    // No password provider: this must authenticate with the disposable key.
    let ctx = Context::default();
    for (scheme, alias) in [
        ("sftp", "direct"),
        ("ssh", "direct"),
        ("sftp", "viajump"),
        ("ssh", "viajump"),
        ("sftp", "encrypted"),
    ] {
        let ctx = if alias == "encrypted" {
            let (tx, rx) = mpsc::channel::<mc::vfs::AuthRequest>();
            std::thread::spawn(move || {
                while let Ok(r) = rx.recv() {
                    assert!(r.resource.contains("passphrase"));
                    let _ = r.reply.send(Some(Secret::new("test-passphrase".into())));
                }
            });
            Context {
                auth: Some(tx),
                ..Default::default()
            }
        } else {
            ctx.clone()
        };
        let root = remote::connect(&format!("{scheme}://{alias}{endpoint}"), &ctx).unwrap();
        let hello = root.join("hello.txt");
        let mut text = String::new();
        hello
            .fs
            .open_read(&hello.path, &ctx)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "hello world");
        let meta = Metadata {
            kind: Kind::File,
            size: 7,
            modified: std::time::SystemTime::now(),
            permissions: None,
        };
        let target = root.join("target");
        let mut writer = target.fs.create(&target.path, false, &ctx).unwrap();
        writer.write_all(b"payload").unwrap();
        writer.commit(&meta).unwrap();
        let mut short = target.fs.create(&target.path, true, &ctx).unwrap();
        short.write_all(b"short").unwrap();
        assert!(short.commit(&meta).is_err());
        let mut replace = target.fs.create(&target.path, true, &ctx).unwrap();
        replace.write_all(b"updated").unwrap();
        let result = replace.commit(&meta);
        if scheme == "ssh" {
            result.unwrap();
        } else {
            assert!(
                result.is_err(),
                "OpenSSH SFTP v3 rename must not delete the old destination to replace it"
            );
        }
        let mut text = String::new();
        target
            .fs
            .open_read(&target.path, &ctx)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(
            text,
            if scheme == "ssh" {
                "updated"
            } else {
                "payload"
            }
        );
        target.fs.remove(&target.path, false, &ctx).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while root.read_dir(&ctx).unwrap().iter().any(|(p, _)| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".mc-upload-")
        }) {
            assert!(
                std::time::Instant::now() < until,
                "staging cleanup did not finish"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
