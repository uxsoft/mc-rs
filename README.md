# mc

A modern dual-panel terminal file manager written in Rust and Ratatui. The crates.io package is named `mc-rs`; the binary is named `mc`. It preserves the familiar Midnight Commander keys for supported operations, with a dark interface and mouse navigation.

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
- Jobs run in the background. Unrelated jobs can run together; jobs with overlapping source/destination paths are rejected until the running job finishes. F9 → File → Background jobs shows progress, errors, and cancellation.
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

Click to focus a panel and position the cursor; right-click toggles selection; double-click opens; the wheel scrolls. The persistent File / View / Go / Help bar opens dropdowns beneath each label. F9 opens File; Left/Right or Tab switch menus, Up/Down select actions, Enter activates, and Esc/F9 dismisses. Mouse hover switches open menus and highlights actions; clicking outside dismisses them. Function-key labels and menu choices are clickable. In deletion dialogs, click the deletion mode, then confirm.

## Archives

Enter opens ZIP, RAR, tar, 7z, `.tar.gz`, `.tgz`, or `.gz` in a read-only panel. F5 copies entries to a local destination; F3 views a file via `cat`. Parent navigation at the archive root returns to the containing directory.

Archive panels keep an entry index in memory. Navigation, filename search, and directory sizing use that index; payloads are decoded only when reading/copying a member. `cat` receives a stream on stdin. Copies use the destination provider's staged write handle; local copies need space only for the file being copied, not the whole browsing tree.

ZIP (AES and ZipCrypto), 7z, and RAR can request a password. Enter submits, Esc cancels; incorrect passwords can be retried. The field is masked and credentials stay in the archive session, never in paths, command arguments, or configuration files. The application's password buffers are zeroized when dropped; decoder libraries may keep their own copies. Passwords are forgotten when the last panel, job, or open handle releases that session.

ZIP uses the Rust `zip` decoder, 7z uses `sevenz-rust2`, RAR uses `rars`, tar uses libarchive, and gzip uses flate2. No archive CLI helpers are needed. ZIP supports stored/deflated content; other compression methods may report unsupported. Multipart archives and archive modification remain unsupported. Nested archive browsing needs a seekable source; archive-member streams currently do not provide one. RAR currently needs a local archive file, isolated within its VFS adapter.

Decoded output uses a bounded channel (two 64 KiB chunks), plus decoder dictionaries and the metadata index. A validation pass precedes streaming the selected member, so password retries cannot release incorrect plaintext; this reads selected content twice. Solid archives can require decoding preceding members too. Compressed tar header scanning and standalone gzip size calculation may require decompressing the compressed stream, without writing it to disk. Some RAR5 transforms need buffered decoding, capped at 32 MiB; decoder dictionary memory is additional.

Archive paths reject absolute paths, traversal, Windows path syntax, and duplicate/conflicting names. Unsupported link/special entries fail rather than being restored as filesystem links. If the backing archive's size or modification time changes, leave and reopen it.

Local files and archives share a provider interface, typed locations, and owned handles inspired by Midnight Commander's VFS. See [VFS.md](VFS.md) for the architecture and future remote-backend contract.

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
python3 mc/tests/archive_smoke.py   # encrypted archives, streamed cat, copy, retry/cancel
```

Tests use disposable files. The terminal smoke test redirects the Linux trash location to its own temporary directory. It exercises cat, copying, moving, mkdir, both deletion modes, search-result selection, mouse input, resize, and terminal restoration.

See [PLAN.md](PLAN.md) for progress, architecture, acceptance criteria, and the next session's tasks.

## Publishing to crates.io

The single `.github/workflows/ci.yml` workflow builds/tests pushes to `master`, pull requests targeting `master`, and manual runs. Its **Publish to crates.io** job runs only on pushes to `master`, after all four native matrix jobs pass. GitHub releases no longer trigger publishing, and there is no separate publishing workflow or second CI matrix.

CI stamps the package version as `major.minor.<github.run_number>`. For example, a checked-in version of `0.1.0` becomes `0.1.42` for workflow run 42. The major/minor values come from `mc/Cargo.toml`; its patch value is replaced. `.github/scripts/set_ci_version.py` updates the manifest and only the matching `mc-rs` lockfile entry in each runner's checkout. Tests, release binaries, artifact names, and the crate upload use the same version. The executable remains `mc`.

Add a crates.io API token with publishing permission for `mc-rs` as the GitHub Actions repository secret `CARGO_REGISTRY_TOKEN`. Create/manage the token in [crates.io account settings](https://crates.io/settings/tokens); do not commit it. Then push the source changes to `master`. Keep `mc/LICENSE` synchronized with the root `LICENSE`; version stamping checks this.

Version changes are temporary CI changes and are not committed or pushed back. Publishing uses `--locked --allow-dirty` for the stamped checkout and Cargo's built-in package verification before upload. The publishing token is exposed only to the upload step. Publishing jobs are serialized; PR and manual runs never receive the token or publish.

GitHub reruns retain the same run number and version. If that version was already uploaded, crates.io rejects a second upload; use a new push for another version. Run numbers may have gaps from PR/manual/failed runs. See [GitHub's run-number documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/variables) and the [Cargo publishing command](https://doc.rust-lang.org/cargo/commands/cargo-publish.html).

After publication, install with `cargo install mc-rs --locked` (with the build dependencies above installed). Source repository: [uxsoft/mc-rs](https://github.com/uxsoft/mc-rs).

## License and references

GPL-3.0-or-later; see [LICENSE](LICENSE). This project is an independent implementation inspired by [Midnight Commander](https://github.com/MidnightCommander/mc), with behavior checked against its [manual](https://source.midnight-commander.org/man/mc.html). It is not an official Midnight Commander release. RAR and 7z test fixtures come from compress-tools and carry their own MIT notice under `mc/tests/fixtures`.
