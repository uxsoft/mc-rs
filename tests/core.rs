use mc::vfs::{Context, VfsPath};
use mc::{
    archives,
    jobs::{self, Decision, Job, Operation},
    panel::Panel,
};
use std::{
    fs,
    io::Write,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
fn start_local(
    op: Operation,
    sources: Vec<std::path::PathBuf>,
    destination: std::path::PathBuf,
    _: Option<()>,
) -> Job {
    jobs::start(
        op,
        sources.into_iter().map(Into::into).collect(),
        destination.into(),
        Context::default(),
    )
}
fn read_virtual(path: VfsPath) -> Vec<u8> {
    let mut bytes = vec![];
    std::io::Read::read_to_end(
        &mut path.fs.open_read(&path.path, &Context::default()).unwrap(),
        &mut bytes,
    )
    .unwrap();
    bytes
}
fn wait(job: &Job, decision: Decision) -> Option<String> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let mut p = job.progress.lock().unwrap();
        if let Some(c) = p.conflict.take() {
            c.reply.send(decision).unwrap();
        }
        if p.done {
            return p.error.clone();
        }
        drop(p);
        assert!(Instant::now() < until, "job timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn recursive_copy_and_move_preserve_contents() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source");
    let dest = d.path().join("dest");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::create_dir(&dest).unwrap();
    fs::write(source.join("nested/file"), "hello").unwrap();
    assert_eq!(
        wait(
            &start_local(Operation::Copy, vec![source.clone()], dest.clone(), None),
            Decision::Cancel
        ),
        None
    );
    assert_eq!(fs::read(dest.join("source/nested/file")).unwrap(), b"hello");
    assert!(source.exists());
    let renamed = d.path().join("renamed");
    assert_eq!(
        wait(
            &start_local(Operation::Move, vec![source.clone()], renamed.clone(), None),
            Decision::Cancel
        ),
        None
    );
    assert!(!source.exists());
    assert_eq!(fs::read(renamed.join("nested/file")).unwrap(), b"hello");
}
#[test]
fn skipped_move_keeps_source_and_destination() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    fs::write(&a, "new").unwrap();
    fs::write(&b, "old").unwrap();
    assert_eq!(
        wait(
            &start_local(Operation::Move, vec![a.clone()], b.clone(), None),
            Decision::Skip
        ),
        None
    );
    assert_eq!(fs::read(a).unwrap(), b"new");
    assert_eq!(fs::read(b).unwrap(), b"old");
}
#[test]
fn overwrite_is_explicit_and_failed_move_preserves_source() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    fs::write(&a, "new").unwrap();
    fs::write(&b, "old").unwrap();
    assert_eq!(
        wait(
            &start_local(Operation::Copy, vec![a.clone()], b.clone(), None),
            Decision::Overwrite
        ),
        None
    );
    assert_eq!(fs::read(b).unwrap(), b"new");
    assert!(
        wait(
            &start_local(
                Operation::Move,
                vec![a.clone()],
                d.path().join("missing/file"),
                None
            ),
            Decision::Cancel
        )
        .is_some()
    );
    assert!(a.exists());
}
#[test]
fn reject_recursive_self_copy() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("child")).unwrap();
    assert!(
        jobs::validate_destination(&d.path().into(), &d.path().join("child/copy").into()).is_err()
    );
}
#[test]
fn cancellation_at_conflict_leaves_both_files_untouched() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    fs::write(&a, "new").unwrap();
    fs::write(&b, "old").unwrap();
    let job = start_local(Operation::Move, vec![a.clone()], b.clone(), None);
    let until = Instant::now() + Duration::from_secs(5);
    while job.progress.lock().unwrap().conflict.is_none() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    job.cancel.store(true, Ordering::Relaxed);
    assert!(wait(&job, Decision::Cancel).is_some());
    assert_eq!(fs::read(a).unwrap(), b"new");
    assert_eq!(fs::read(b).unwrap(), b"old");
}
#[cfg(unix)]
#[test]
fn copy_symlink_does_not_follow_it() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    fs::write(&a, "data").unwrap();
    let link = d.path().join("link");
    std::os::unix::fs::symlink("a", &link).unwrap();
    let out = d.path().join("out");
    assert_eq!(
        wait(
            &start_local(Operation::Copy, vec![link], out.clone(), None),
            Decision::Cancel
        ),
        None
    );
    assert_eq!(fs::read_link(out).unwrap(), Path::new("a"));
}
#[cfg(unix)]
#[test]
fn recursive_copy_rejects_destination_directory_symlink() {
    let d = tempfile::tempdir().unwrap();
    let source = d.path().join("source");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("nested/file"), "new").unwrap();
    let dest = d.path().join("dest");
    fs::create_dir_all(dest.join("source")).unwrap();
    let outside = d.path().join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("source/nested")).unwrap();
    assert!(
        wait(
            &start_local(Operation::Copy, vec![source], dest, None),
            Decision::Cancel
        )
        .is_some()
    );
    assert!(!outside.join("file").exists());
}
fn tar_bytes() -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(5);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "folder/file.txt", &b"hello"[..])
        .unwrap();
    builder.into_inner().unwrap()
}
#[test]
fn zip_tar_and_gzip_are_readable() {
    let d = tempfile::tempdir().unwrap();
    let tar = d.path().join("test.tar");
    fs::write(&tar, tar_bytes()).unwrap();
    let mount = archives::open(tar.into(), &Context::default(), |_| {}).unwrap();
    assert_eq!(read_virtual(mount.join("folder/file.txt")), b"hello");
    for (name, data, target) in [
        ("test.tar.gz", tar_bytes(), "folder/file.txt"),
        ("file.txt.gz", b"hello".to_vec(), "file.txt"),
    ] {
        let path = d.path().join(name);
        let mut enc = flate2::write::GzEncoder::new(
            fs::File::create(&path).unwrap(),
            flate2::Compression::default(),
        );
        enc.write_all(&data).unwrap();
        enc.finish().unwrap();
        let m = archives::open(path.into(), &Context::default(), |_| {}).unwrap();
        assert_eq!(read_virtual(m.join(target)), b"hello");
    }
    let path = d.path().join("test.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    zip.start_file("folder/file.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"hello").unwrap();
    zip.finish().unwrap();
    let m = archives::open(path.into(), &Context::default(), |_| {}).unwrap();
    assert_eq!(read_virtual(m.join("folder/file.txt")), b"hello");
}
#[test]
fn zip_based_application_files_support_browsing_nested_mounts_and_copy() {
    let dir = tempfile::tempdir().unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "contents/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"<document>hello</document>").unwrap();
    let data = zip.finish().unwrap().into_inner();
    for extension in [
        "jar", "WAR", "ear", "docx", "DOCM", "dotx", "dotm", "xlsx", "xlsm", "xlsb", "xltx",
        "xltm", "xlam", "pptx", "pptm", "potx", "potm", "ppsx", "ppsm", "ppam", "sldx", "sldm",
        "vsdx", "vsdm", "vssx", "vssm", "vstx", "vstm", "thmx",
    ] {
        let path = dir.path().join(format!("package.{extension}"));
        fs::write(&path, &data).unwrap();
        let source: VfsPath = path.into();
        assert!(archives::supported(&source), "{extension}");
        let mount = archives::open(source, &Context::default(), |_| {}).unwrap();
        assert!(!mount.fs.capabilities().write);
        assert_eq!(
            read_virtual(mount.join("contents/document.xml")),
            b"<document>hello</document>"
        );
    }
    for extension in ["doc", "xls", "ppt", "exe"] {
        assert!(!archives::supported(
            &dir.path().join(format!("legacy.{extension}")).into()
        ));
    }
    let outer = dir.path().join("bundle.jar");
    let mut zip = zip::ZipWriter::new(fs::File::create(&outer).unwrap());
    zip.start_file("embedded.docx", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&data).unwrap();
    zip.finish().unwrap();
    let mount = archives::open(outer.into(), &Context::default(), |_| {}).unwrap();
    let inner = archives::open(mount.join("embedded.docx"), &Context::default(), |_| {}).unwrap();
    let destination = dir.path().join("copied.xml");
    let job = jobs::start(
        Operation::Copy,
        vec![inner.join("contents/document.xml")],
        destination.clone().into(),
        Context::default(),
    );
    assert_eq!(wait(&job, Decision::Cancel), None);
    assert_eq!(
        fs::read(destination).unwrap(),
        b"<document>hello</document>"
    );
}
#[test]
fn rar_and_sevenz_fixtures_are_readable() {
    for format in ["rar", "7z"] {
        let m = archives::open(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/fixtures/tree.{format}"))
                .into(),
            &Context::default(),
            |_| {},
        )
        .unwrap();
        let mut pending = vec![m];
        let mut files = 0;
        while let Some(path) = pending.pop() {
            for (p, meta) in path.read_dir(&Context::default()).unwrap() {
                if meta.kind == mc::vfs::Kind::Directory {
                    pending.push(p);
                } else {
                    read_virtual(p);
                    files += 1;
                }
            }
        }
        assert!(files > 0);
    }
}
#[test]
fn archive_paths_are_portably_validated() {
    for name in [
        "../escape",
        "/absolute",
        "C:/escape",
        "a\\..\\escape",
        "dir/../../escape",
    ] {
        assert!(archives::safe_path(name).is_err(), "{name}");
    }
    assert!(archives::safe_path("./folder/file").is_ok());
}
#[test]
fn malicious_zip_and_tar_links_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("bad.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    zip.start_file("../escape", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"evil").unwrap();
    zip.finish().unwrap();
    assert!(archives::open(path.into(), &Context::default(), |_| {}).is_err());
    assert!(!d.path().join("escape").exists());
    let path = d.path().join("bad.tar");
    let mut tar = tar::Builder::new(fs::File::create(&path).unwrap());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Symlink);
    h.set_size(0);
    h.set_mode(0o777);
    h.set_cksum();
    tar.append_link(&mut h, "link", "/tmp").unwrap();
    tar.finish().unwrap();
    assert!(archives::open(path.into(), &Context::default(), |_| {}).is_err());
}
#[test]
fn cancelled_archive_does_not_return_a_mount() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("test.tar");
    fs::write(&path, tar_bytes()).unwrap();
    assert!(
        archives::open(
            path.into(),
            &Context {
                cancel: std::sync::Arc::new(AtomicBool::new(true)),
                ..Default::default()
            },
            |_| {}
        )
        .is_err()
    );
}
#[test]
fn stale_directory_result_cannot_replace_new_location() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("first"), "").unwrap();
    fs::write(b.path().join("second"), "").unwrap();
    let mut panel = Panel::new(a.path().to_owned());
    panel.navigate(b.path().into());
    let until = Instant::now() + Duration::from_secs(3);
    while panel.loading {
        panel.poll();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(panel.entries[0].name, "second");
}
#[test]
fn unicode_wildcards() {
    assert!(mc::app::wildcard("*.rs", "hello.rs"));
    assert!(mc::app::wildcard("?afé", "café"));
    assert!(!mc::app::wildcard("*.rs", "hello.txt"));
}
#[test]
fn explicit_rename_refuses_existing_files_and_directories() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("source"), "source data").unwrap();
    fs::write(dir.path().join("existing"), "keep me").unwrap();
    fs::create_dir(dir.path().join("folder")).unwrap();
    let source: VfsPath = dir.path().join("source").into();
    for target in ["existing", "folder"] {
        assert!(
            source
                .fs
                .rename_in_place(&source.path, &dir.path().join(target), &Context::default())
                .is_err()
        );
        assert_eq!(fs::read_to_string(&source.path).unwrap(), "source data");
        assert_eq!(
            fs::read_to_string(dir.path().join("existing")).unwrap(),
            "keep me"
        );
    }
    let destination: VfsPath = dir.path().join("folder").into();
    let resources = jobs::resources(Operation::Rename, &[source], &destination);
    let target_resources = jobs::resources(Operation::Delete, &[destination], &dir.path().into());
    assert!(jobs::overlaps(&resources, &target_resources));
}
#[test]
fn render_small_and_normal_terminals() {
    for (w, h) in [(20, 5), (36, 10), (100, 30)] {
        let d = tempfile::tempdir().unwrap();
        let mut app = mc::app::App::new(d.path().to_owned(), d.path().to_owned());
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| mc::ui::draw(f, &mut app)).unwrap();
        for (category, menu) in mc::menu::MENUS.iter().enumerate() {
            app.menu = Some(mc::menu::State {
                category,
                cursor: menu.items.len() - 1,
                offset: 0,
            });
            term.draw(|f| mc::ui::draw(f, &mut app)).unwrap();
            if w >= 36 && h >= 10 {
                assert_eq!(
                    app.menu_area.y, 1,
                    "dropdown must anchor beneath the menu bar"
                );
                assert!(app.menu_area.right() <= w);
                assert!(
                    app.menu_area.bottom() < h,
                    "dropdown must preserve the shortcut bar"
                );
                let state = app.menu.as_ref().unwrap();
                assert!(state.cursor < state.offset + app.menu_area.height as usize - 2);
            }
        }
    }
}

#[test]
fn cancellation_during_copy_preserves_existing_destination() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("large");
    let b = d.path().join("existing");
    fs::File::create(&a)
        .unwrap()
        .set_len(256 * 1024 * 1024)
        .unwrap();
    fs::write(&b, "original").unwrap();
    let job = start_local(Operation::Copy, vec![a.clone()], b.clone(), None);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let mut p = job.progress.lock().unwrap();
        if let Some(c) = p.conflict.take() {
            c.reply.send(Decision::Overwrite).unwrap();
        }
        if p.bytes > 0 {
            job.cancel.store(true, Ordering::Relaxed);
            break;
        }
        assert!(Instant::now() < until);
        drop(p);
        std::thread::yield_now();
    }
    assert!(wait(&job, Decision::Cancel).is_some());
    assert_eq!(fs::read(&b).unwrap(), b"original");
    assert!(a.exists());
    assert_eq!(
        fs::read_dir(d.path()).unwrap().count(),
        2,
        "temporary copy must be cleaned up"
    );
}
#[cfg(unix)]
#[test]
fn dangling_symlinks_can_be_copied() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("dangling");
    let b = d.path().join("copy");
    std::os::unix::fs::symlink("missing", &a).unwrap();
    assert_eq!(
        wait(
            &start_local(Operation::Copy, vec![a], b.clone(), None),
            Decision::Cancel
        ),
        None
    );
    assert_eq!(fs::read_link(b).unwrap(), Path::new("missing"));
}
#[test]
fn path_locks_allow_disjoint_work_but_prevent_nested_mutations() {
    let a = vec![VfsPath::from(std::path::PathBuf::from("/source/a"))];
    let b = vec![VfsPath::from(std::path::PathBuf::from("/source/b"))];
    let nested = vec![VfsPath::from(std::path::PathBuf::from("/source/a/child"))];
    assert!(!jobs::overlaps(&a, &b));
    assert!(jobs::overlaps(&a, &nested));
}
#[test]
fn input_editing_handles_multibyte_characters() {
    use crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers as M};
    let mut value = "café".to_string();
    let mut cursor = value.len();
    mc::app::edit_input(&mut value, &mut cursor, KeyEvent::new(K::Left, M::NONE));
    mc::app::edit_input(&mut value, &mut cursor, KeyEvent::new(K::Delete, M::NONE));
    assert_eq!(value, "caf");
    mc::app::edit_input(
        &mut value,
        &mut cursor,
        KeyEvent::new(K::Char('a'), M::CONTROL),
    );
    mc::app::edit_input(
        &mut value,
        &mut cursor,
        KeyEvent::new(K::Char('é'), M::NONE),
    );
    assert_eq!(value, "écaf");
    mc::app::edit_input(
        &mut value,
        &mut cursor,
        KeyEvent::new(K::Backspace, M::NONE),
    );
    assert_eq!(value, "caf");
}

fn settled_panel(path: &Path) -> Panel {
    let mut panel = Panel::new(path.to_owned());
    let until = Instant::now() + Duration::from_secs(5);
    while panel.loading {
        panel.poll();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(2));
    }
    panel
}

#[test]
fn selected_directory_sizes_include_nested_hidden_files_and_selection_total() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("folder/nested")).unwrap();
    fs::write(d.path().join("folder/nested/data"), b"12345").unwrap();
    fs::write(d.path().join("folder/.hidden"), b"abc").unwrap();
    fs::write(d.path().join("plain"), b"xy").unwrap();
    let mut p = settled_panel(d.path());
    p.cursor = 1;
    p.toggle();
    p.step(1);
    p.toggle();
    assert_eq!(p.sources().len(), 2);
    assert_eq!(p.selection_size(), (2, 1, 0));
    let until = Instant::now() + Duration::from_secs(5);
    while p.selection_size().1 > 0 {
        p.poll();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(p.selection_size(), (10, 0, 0));
    assert_eq!(p.directory_sizes[&d.path().join("folder").into()].bytes, 8);
    p.cursor = 1;
    p.toggle();
    assert_eq!(p.selection_size(), (2, 0, 0));
    fs::write(d.path().join("folder/new"), b"new").unwrap();
    p.toggle();
    assert_eq!(
        p.selection_size().1,
        1,
        "reselection recalculates directory size"
    );
    p.navigate(d.path().join("folder").into());
    assert!(p.directory_sizes.is_empty());
    assert_eq!(p.selection_size(), (0, 0, 0));
}

#[cfg(unix)]
#[test]
fn directory_sizing_does_not_follow_symlink_cycles_or_external_targets() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("folder")).unwrap();
    fs::write(d.path().join("outside"), vec![0; 1000]).unwrap();
    std::os::unix::fs::symlink("..", d.path().join("folder/cycle")).unwrap();
    std::os::unix::fs::symlink("../outside", d.path().join("folder/link")).unwrap();
    let size = mc::panel::directory_size(&d.path().join("folder").into(), &AtomicBool::new(false));
    assert_eq!(size.bytes, 12); // bytes in the two link paths, not the target contents
    assert_eq!(size.errors, 0);
}

#[test]
fn directory_sizing_reports_errors_and_honors_cancellation() {
    let d = tempfile::tempdir().unwrap();
    assert!(
        mc::panel::directory_size(&d.path().join("missing").into(), &AtomicBool::new(false)).errors
            > 0
    );
    fs::write(d.path().join("file"), b"content").unwrap();
    assert_eq!(
        mc::panel::directory_size(&d.path().into(), &AtomicBool::new(true)).bytes,
        0
    );
}

fn encrypted_fixture(path: &Path, format: &str, headers: bool) {
    match format {
        "zip" => {
            let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
            writer
                .start_file(
                    "folder/secret.txt",
                    zip::write::SimpleFileOptions::default()
                        .with_aes_encryption(zip::AesMode::Aes256, "correct"),
                )
                .unwrap();
            writer.write_all(b"secret contents").unwrap();
            writer.finish().unwrap();
        }
        "7z" => {
            use sevenz_rust2::{
                ArchiveEntry, ArchiveWriter, Password,
                encoder_options::{AesEncoderOptions, Lzma2Options},
            };
            let mut writer = ArchiveWriter::new(fs::File::create(path).unwrap()).unwrap();
            writer.set_content_methods(vec![
                AesEncoderOptions::new(Password::new("correct")).into(),
                Lzma2Options::default().into(),
            ]);
            writer.set_encrypt_header(headers);
            writer
                .push_archive_entry(
                    ArchiveEntry::new_file("folder/secret.txt"),
                    Some(&b"secret contents"[..]),
                )
                .unwrap();
            writer.finish().unwrap();
        }
        "rar" => {
            let mut builder = rars::Builder::new(rars::ArchiveVersion::Rar50)
                .password(Some(b"correct".to_vec()))
                .header_encryption(headers);
            builder
                .add_bytes(
                    b"folder/secret.txt".to_vec(),
                    b"secret contents".to_vec(),
                    None,
                    None,
                )
                .unwrap();
            builder.write_to_path(path, None).unwrap();
        }
        _ => unreachable!(),
    }
}
#[test]
fn encrypted_archives_retry_and_reuse_session_password() {
    use std::sync::{Arc, atomic::AtomicUsize, mpsc};
    for (format, headers) in [
        ("zip", false),
        ("7z", false),
        ("7z", true),
        ("rar", false),
        ("rar", true),
    ] {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join(format!("locked.{format}"));
        encrypted_fixture(&file, format, headers);
        let (tx, rx) = mpsc::channel::<mc::vfs::AuthRequest>();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let responder = std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                let n = count.fetch_add(1, Ordering::Relaxed);
                assert!(n < 3, "password retry loop");
                assert_eq!(request.retry, n > 0);
                request
                    .reply
                    .send(Some(mc::vfs::Secret::new(
                        if n == 0 { "wrong" } else { "correct" }.into(),
                    )))
                    .unwrap();
            }
        });
        let ctx = Context {
            auth: Some(tx),
            ..Default::default()
        };
        let root = archives::open(file.into(), &ctx, |_| {})
            .unwrap_or_else(|e| panic!("{format} headers={headers}: {e:#}"));
        let path = root.join("folder/secret.txt");
        for _ in 0..2 {
            let mut bytes = vec![];
            std::io::Read::read_to_end(
                &mut path.fs.open_read(&path.path, &ctx).unwrap(),
                &mut bytes,
            )
            .unwrap_or_else(|e| panic!("{format} headers={headers}: {e:#}"));
            assert_eq!(bytes, b"secret contents");
        }
        assert_eq!(
            requests.load(Ordering::Relaxed),
            2,
            "{format} headers={headers}"
        );
        assert_eq!(
            fs::read_dir(d.path()).unwrap().count(),
            1,
            "browsing must not extract beside archive"
        );
        drop(ctx);
        responder.join().unwrap();
    }
}
#[test]
fn archive_metadata_copy_lifetime_and_read_only_capabilities() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("test.tar");
    fs::write(&file, tar_bytes()).unwrap();
    let root = archives::open(file.clone().into(), &Context::default(), |_| {}).unwrap();
    assert!(root.local_path().is_none());
    assert_eq!(root.parent().unwrap(), d.path().into());
    assert_eq!(
        mc::panel::directory_size(&root, &AtomicBool::new(false)).bytes,
        5
    );
    let folder = root.join("folder");
    let target = d.path().join("out");
    let job = jobs::start(
        Operation::Copy,
        vec![folder.clone()],
        target.clone().into(),
        Context::default(),
    );
    drop(root);
    assert_eq!(wait(&job, Decision::Cancel), None);
    assert_eq!(fs::read(target.join("file.txt")).unwrap(), b"hello");
    for op in [Operation::Move, Operation::Delete, Operation::Trash] {
        assert!(
            wait(
                &jobs::start(
                    op,
                    vec![folder.clone()],
                    d.path().join("bad").into(),
                    Context::default()
                ),
                Decision::Cancel
            )
            .is_some()
        );
    }
    // Compare prepared lock sets, as App::start does. Temporary directories can
    // contain symlink aliases (notably /var -> /private/var on macOS).
    let deletion_resources = jobs::resources(Operation::Delete, &[file.into()], &d.path().into());
    assert!(jobs::overlaps(&job.resources, &deletion_resources));
}
#[test]
fn password_cancellation_preserves_destination() {
    use std::sync::mpsc;
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("locked.zip");
    encrypted_fixture(&file, "zip", false);
    let root = archives::open(file.into(), &Context::default(), |_| {}).unwrap();
    let target = d.path().join("target");
    fs::write(&target, b"original").unwrap();
    let (tx, rx) = mpsc::channel();
    let ctx = Context {
        auth: Some(tx),
        ..Default::default()
    };
    let job = jobs::start(
        Operation::Copy,
        vec![root.join("folder/secret.txt")],
        target.clone().into(),
        ctx,
    );
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(conflict) = job.progress.lock().unwrap().conflict.take() {
            conflict.reply.send(Decision::Overwrite).unwrap();
        }
        if let Ok(request) = rx.try_recv() {
            request.reply.send(None).unwrap();
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(wait(&job, Decision::Cancel).is_some());
    assert_eq!(fs::read(target).unwrap(), b"original");
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2);
}
#[test]
fn metadata_browsing_does_not_decode_corrupt_payload() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("test.zip");
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(
            "file",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"unique payload").unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();
    let pos = bytes
        .windows(14)
        .position(|w| w == b"unique payload")
        .unwrap();
    bytes[pos] ^= 0xff;
    fs::write(&file, bytes).unwrap();
    let root = archives::open(file.into(), &Context::default(), |_| {}).unwrap();
    assert_eq!(root.read_dir(&Context::default()).unwrap().len(), 1);
    let p = root.join("file");
    let mut bytes = vec![];
    assert!(
        std::io::Read::read_to_end(
            &mut p.fs.open_read(&p.path, &Context::default()).unwrap(),
            &mut bytes
        )
        .is_err()
    );
    assert!(
        bytes.is_empty(),
        "validation must not release corrupt plaintext"
    );
}

#[cfg(unix)]
#[test]
fn archive_locks_overlap_through_symlinked_parent_paths() {
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real");
    let alias = d.path().join("alias");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    fs::write(real.join("test.tar"), tar_bytes()).unwrap();
    fs::write(real.join("unrelated"), b"keep").unwrap();
    let root = archives::open(alias.join("test.tar").into(), &Context::default(), |_| {}).unwrap();
    let destination = d.path().join("out").into();
    let copy = jobs::resources(Operation::Copy, &[root.join("folder")], &destination);
    for path in [real.join("test.tar"), alias.join("test.tar"), real.clone()] {
        let deletion = jobs::resources(Operation::Delete, &[path.into()], &destination);
        assert!(jobs::overlaps(&copy, &deletion));
    }
    let unrelated = jobs::resources(
        Operation::Delete,
        &[alias.join("unrelated").into()],
        &destination,
    );
    assert!(!jobs::overlaps(&copy, &unrelated));
}

#[test]
fn sevenz_selected_member_ignores_corrupt_later_block() {
    use sevenz_rust2::{Archive, ArchiveEntry, ArchiveWriter, Password};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("independent.7z");
    let mut writer = ArchiveWriter::new(fs::File::create(&path).unwrap()).unwrap();
    writer
        .push_archive_entry(ArchiveEntry::new_file("first"), Some(&b"intact"[..]))
        .unwrap();
    writer
        .push_archive_entry(
            ArchiveEntry::new_file("second"),
            Some(&b"later damaged contents"[..]),
        )
        .unwrap();
    writer.finish().unwrap();
    let archive = Archive::read(&mut fs::File::open(&path).unwrap(), &Password::empty()).unwrap();
    assert_ne!(
        archive.stream_map.file_block_index[0],
        archive.stream_map.file_block_index[1]
    );
    let offset = 32 + archive.pack_pos() + archive.pack_sizes()[0];
    let mut bytes = fs::read(&path).unwrap();
    bytes[offset as usize] ^= 0xff;
    fs::write(&path, bytes).unwrap();
    let mount = archives::open(path.into(), &Context::default(), |_| {}).unwrap();
    assert_eq!(read_virtual(mount.join("first")), b"intact");
    let mut damaged = mount
        .join("second")
        .fs
        .open_read(std::path::Path::new("second"), &Context::default())
        .unwrap();
    assert!(std::io::Read::read_to_end(&mut damaged, &mut vec![]).is_err());
}

#[cfg(unix)]
#[test]
fn filesystem_parents_preserve_symlinks_and_lock_aliases() {
    use mc::vfs::{FileSystem, local::Local};
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("actual/sub")).unwrap();
    fs::create_dir(dir.path().join("view")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("actual/sub"), dir.path().join("view/link"))
        .unwrap();
    fs::write(dir.path().join("actual/file"), "correct").unwrap();
    let canonical_dir = fs::canonicalize(dir.path()).unwrap();
    let alias: VfsPath = dir.path().join("view/link/../file").into();
    assert_eq!(read_virtual(alias.clone()), b"correct");
    assert_eq!(
        alias.parent().unwrap().parent().unwrap().path,
        canonical_dir
    );
    let direct: VfsPath = dir.path().join("actual/file").into();
    assert!(mc::jobs::overlaps(
        &mc::jobs::resources(Operation::Delete, &[alias], &direct),
        &mc::jobs::resources(Operation::Delete, std::slice::from_ref(&direct), &direct)
    ));
    let missing = dir.path().join("view/link/../new/leaf");
    assert_eq!(
        Local.canonical(&missing).unwrap(),
        canonical_dir.join("actual/new/leaf")
    );
    let relative: VfsPath = std::path::PathBuf::from("../../missing").into();
    assert_eq!(relative.path, std::path::PathBuf::from("../../missing"));
    assert!(
        Local
            .canonical(std::path::Path::new("missing-leaf"))
            .unwrap()
            .is_absolute()
    );
}

#[test]
fn partially_skipped_directory_move_keeps_original_children() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let destination = dir.path().join("destination");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(destination.join("source")).unwrap();
    fs::write(source.join("skip"), "source").unwrap();
    fs::write(source.join("copy"), "copy").unwrap();
    fs::write(destination.join("source/skip"), "keep").unwrap();
    let job = start_local(
        Operation::Move,
        vec![source.clone()],
        destination.clone(),
        None,
    );
    assert_eq!(wait(&job, Decision::Skip), None);
    assert_eq!(
        fs::read_to_string(destination.join("source/skip")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::read_to_string(destination.join("source/copy")).unwrap(),
        "copy"
    );
    assert!(source.join("skip").exists() && source.join("copy").exists());
}

#[cfg(windows)]
#[test]
fn windows_filesystem_forms_retain_parent_components() {
    for value in [
        r"C:\folder\link\..\leaf",
        r"..\..\leaf",
        r"\\server\share\link\..\leaf",
        r"\\?\C:\link\..\leaf",
    ] {
        let path = std::path::PathBuf::from(value);
        let virtual_path: VfsPath = path.clone().into();
        assert_eq!(virtual_path.path, path);
        assert!(
            virtual_path
                .path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        );
    }
}
