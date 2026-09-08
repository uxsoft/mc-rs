# mc

A modern dual-panel terminal file manager written in Rust and Ratatui. The binary is named `mc`. It preserves the familiar Midnight Commander keys for supported operations, with a dark interface and mouse navigation.

## Build and run

Install a current stable Rust toolchain and libarchive development libraries:

- Debian/Ubuntu: `sudo apt install libarchive-dev pkg-config`
- Fedora: `sudo dnf install libarchive-devel pkgconf-pkg-config`
- macOS ARM64: `brew install libarchive pkg-config`. If needed, set `PKG_CONFIG_PATH` to the `lib/pkgconfig` directory under `brew --prefix libarchive`.
- Windows x64/MSVC: install libarchive with `vcpkg install libarchive:x64-windows-static-md`, set `VCPKG_ROOT` to your vcpkg directory, and `VCPKGRS_TRIPLET=x64-windows-static-md`.

From this repository:

```sh
cargo run --manifest-path mc/Cargo.toml --release -- /path/to/left /path/to/right
```

Both directories are optional and default to the current directory. `--help` and `--version` work without a terminal. Run inside a terminal with keyboard and mouse support.

F3 requires `cat` on PATH, including Windows. F4 uses `VISUAL`, then `EDITOR`, then `vi` on Unix or `notepad` on Windows. Editor arguments are parsed as quoted words and invoked directly; shell expressions are not evaluated. `cat` output stays visible until Enter returns to the file manager.

Linux and macOS binaries dynamically link libarchive, which must also be installed on the destination machine. CI prepares native artifacts for Linux x64/ARM64, macOS ARM64, and Windows x64. **Only Linux x64 has been validated locally; the CI matrix has not yet run.**

## Everyday operations

- Browse independent panels, sort by name/size/date through F9, toggle hidden files, and select multiple items with Space, Insert, or right-click. Selected directories are sized recursively in the background; their size appears in the Size column, and the selection total appears in each panel’s bottom border. Hidden files are included; symlinks are counted by link length without following their targets. Unreadable entries mark totals as partial. Ctrl+R refreshes cached sizes.
- Copy and move into the opposite panel, or enter a new destination/name. Same-filesystem moves use an atomic no-replace rename where possible. Cross-filesystem moves and directory merges copy before removing source data.
- Jobs run in the background. Unrelated jobs can run together; jobs with overlapping source/destination paths are rejected until the running job finishes. F9 → Background jobs shows progress, errors, and cancellation.
- An existing file prompts for overwrite, skip, overwrite all, or skip all. Enter defaults to skip. Directory copies merge into existing plain directories; incompatible file/directory collisions stop with an error.
- F8 defaults to the operating system's trash. Choose permanent deletion explicitly with Tab or the mouse. Trash failures never fall back to permanent deletion.
- Recursive filename search matches a case-insensitive substring, reports unreadable paths, and stops at 100,000 results. Enter navigates to and highlights the selected result. Directory symlinks are not traversed.

## Keys and mouse

| Keys | Action |
| --- | --- |
| Tab, Left, Right | Switch panel |
| Up/Down, Ctrl+P/N | Move cursor |
| Home/End, Alt+</> | First/last entry |
| PageUp/PageDown, Alt+V/Ctrl+V | Page |
| Ctrl+PageUp / Ctrl+PageDown | Parent / open |
| Enter | Enter directory/archive; view regular file with cat |
| Space, Insert, Ctrl+T | Toggle file/directory selection and advance |
| `+`, `-` or `\`, `*` | Select by wildcard, unselect by wildcard, invert file selection |
| Ctrl+S, Alt+S | Quick filename search; Esc clears |
| Alt+? | Recursive filename search |
| Alt+C | Go to directory |
| Alt+. | Toggle hidden files |
| Alt+G/R/J | Top/middle/bottom of visible panel |
| Alt+O | Open cursor directory (or current directory) in other panel and advance |
| Alt+I | Same directory in both panels |
| Ctrl+U | Swap panel contents |
| Ctrl+R, Ctrl+L | Reload directory, repaint screen |
| F1 | Help |
| F3 / F4 | External cat / editor |
| F5 / F6 / F7 / F8 | Copy / move / mkdir / delete |
| F9 / F10 | Menu / quit |

Esc followed by a digit substitutes for a function key (`0` means F10). Esc followed by a letter substitutes for Alt. Input dialogs support arrows, Home/End, Backspace/Delete, Ctrl+A/E/B/F/H/D/K/U. Keyboard bindings are fixed; there is no configuration system.

Click to focus a panel and position the cursor; right-click toggles selection; double-click opens; the wheel scrolls. Function-key labels and menu choices are clickable. In deletion dialogs, click the deletion mode, then confirm.

## Archives

Enter opens ZIP, RAR, tar, 7z, `.tar.gz`, `.tgz`, or `.gz` in a read-only panel. F5 copies entries to a local destination; F3 views a file via `cat`. Parent navigation at the archive root returns to the containing directory.

Archives are fully unpacked in a worker into private temporary storage before browsing. This uses disk space proportional to the uncompressed archive; opening can be cancelled. libarchive supplies ZIP/RAR/tar/7z decoding; flate2 supplies standalone gzip decompression. No `7z`, `unrar`, or `tar` executables are needed. Archive writing, nested archive mounting, password entry, and multipart archives are not supported. Backend decode failures are reported.

Archive paths are checked for absolute paths, traversal, and Windows path syntax. Links and special entries are rejected instead of restored. Duplicate file entries also fail safely. This is deliberately more restrictive than general-purpose archive extractors.

## Current limits

- No shell prompt, subshell, command execution, built-in editor/viewer, remote protocols, plugins, or customization. F2 user menus are omitted. This is not full MC feature parity.
- Copies preserve ordinary permissions, but not ownership, ACLs, extended attributes, sparse layout, or hard-link relationships. Copy timestamps are not yet preserved. Windows symlink creation needs the appropriate OS privilege; symlink overwrite can fail safely on Windows.
- Cancellation preserves already completed work and removes incomplete temporary files; it is not rollback. An interrupted multi-file move may leave completed items at the destination and remaining items at the source. Trash calls cannot be interrupted midway through an OS operation.
- Directory listings and searches run off the UI thread. Initial directory listings are delivered as a complete snapshot, not incrementally. No filesystem watcher; use Ctrl+R for external changes.
- Jobs run only while mc is open. Quit with active jobs offers cancellation and waits for you to quit again after they finish. Jobs are not persisted across sessions.

## Development and validation

```sh
cargo fmt --manifest-path mc/Cargo.toml --check
cargo clippy --manifest-path mc/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path mc/Cargo.toml
cargo build --manifest-path mc/Cargo.toml
python3 mc/tests/terminal_smoke.py  # Linux/Unix pseudo-terminal integration test
```

Tests use disposable files. The terminal smoke test redirects the Linux trash location to its own temporary directory. It exercises cat, copying, moving, mkdir, both deletion modes, search-result selection, mouse input, resize, and terminal restoration.

See [PLAN.md](PLAN.md) for progress, architecture, acceptance criteria, and the next session's tasks.

## License and references

GPL-3.0-or-later; see [LICENSE](LICENSE). This project is an independent implementation inspired by [Midnight Commander](https://github.com/MidnightCommander/mc), with behavior checked against its [manual](https://source.midnight-commander.org/man/mc.html). It is not an official Midnight Commander release. RAR and 7z test fixtures come from compress-tools and carry their own MIT notice under `mc/tests/fixtures`.
