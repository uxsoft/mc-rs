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
            &jobs::start(Operation::Copy, vec![source.clone()], dest.clone(), None),
            Decision::Cancel
        ),
        None
    );
    assert_eq!(fs::read(dest.join("source/nested/file")).unwrap(), b"hello");
    assert!(source.exists());
    let renamed = d.path().join("renamed");
    assert_eq!(
        wait(
            &jobs::start(Operation::Move, vec![source.clone()], renamed.clone(), None),
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
            &jobs::start(Operation::Move, vec![a.clone()], b.clone(), None),
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
            &jobs::start(Operation::Copy, vec![a.clone()], b.clone(), None),
            Decision::Overwrite
        ),
        None
    );
    assert_eq!(fs::read(b).unwrap(), b"new");
    assert!(
        wait(
            &jobs::start(
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
    assert!(jobs::validate_destination(d.path(), &d.path().join("child/copy")).is_err());
}
#[test]
fn cancellation_at_conflict_leaves_both_files_untouched() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    fs::write(&a, "new").unwrap();
    fs::write(&b, "old").unwrap();
    let job = jobs::start(Operation::Move, vec![a.clone()], b.clone(), None);
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
            &jobs::start(Operation::Copy, vec![link], out.clone(), None),
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
            &jobs::start(Operation::Copy, vec![source], dest, None),
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
    let mount = archives::open(tar, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(
        fs::read(mount.temp.path().join("folder/file.txt")).unwrap(),
        b"hello"
    );
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
        let m = archives::open(path, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(fs::read(m.temp.path().join(target)).unwrap(), b"hello");
    }
    let path = d.path().join("test.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    zip.start_file("folder/file.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"hello").unwrap();
    zip.finish().unwrap();
    let m = archives::open(path, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(
        fs::read(m.temp.path().join("folder/file.txt")).unwrap(),
        b"hello"
    );
}
#[test]
fn rar_and_sevenz_fixtures_are_readable() {
    for format in ["rar", "7z"] {
        let m = archives::open(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/tree.{format}")),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(
            walkdir::WalkDir::new(m.temp.path())
                .into_iter()
                .filter_map(Result::ok)
                .any(|e| e.file_type().is_file())
        );
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
    assert!(archives::open(path, &AtomicBool::new(false), |_| {}).is_err());
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
    assert!(archives::open(path, &AtomicBool::new(false), |_| {}).is_err());
}
#[test]
fn cancelled_archive_does_not_return_a_mount() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("test.tar");
    fs::write(&path, tar_bytes()).unwrap();
    assert!(archives::open(path, &AtomicBool::new(true), |_| {}).is_err());
}
#[test]
fn stale_directory_result_cannot_replace_new_location() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("first"), "").unwrap();
    fs::write(b.path().join("second"), "").unwrap();
    let mut panel = Panel::new(a.path().to_owned());
    panel.navigate(b.path().to_owned());
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
fn render_small_and_normal_terminals() {
    for (w, h) in [(20, 5), (36, 10), (100, 30)] {
        let d = tempfile::tempdir().unwrap();
        let mut app = mc::app::App::new(d.path().to_owned(), d.path().to_owned());
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| mc::ui::draw(f, &mut app)).unwrap();
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
    let job = jobs::start(Operation::Copy, vec![a.clone()], b.clone(), None);
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
            &jobs::start(Operation::Copy, vec![a], b.clone(), None),
            Decision::Cancel
        ),
        None
    );
    assert_eq!(fs::read_link(b).unwrap(), Path::new("missing"));
}
#[test]
fn path_locks_allow_disjoint_work_but_prevent_nested_mutations() {
    let a = vec![std::path::PathBuf::from("/source/a")];
    let b = vec![std::path::PathBuf::from("/source/b")];
    let nested = vec![std::path::PathBuf::from("/source/a/child")];
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
    assert_eq!(p.directory_sizes[&d.path().join("folder")].bytes, 8);
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
    p.navigate(d.path().join("folder"));
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
    let size = mc::panel::directory_size(&d.path().join("folder"), &AtomicBool::new(false));
    assert_eq!(size.bytes, 12); // bytes in the two link paths, not the target contents
    assert_eq!(size.errors, 0);
}

#[test]
fn directory_sizing_reports_errors_and_honors_cancellation() {
    let d = tempfile::tempdir().unwrap();
    assert!(
        mc::panel::directory_size(&d.path().join("missing"), &AtomicBool::new(false)).errors > 0
    );
    fs::write(d.path().join("file"), b"content").unwrap();
    assert_eq!(
        mc::panel::directory_size(d.path(), &AtomicBool::new(true)).bytes,
        0
    );
}
