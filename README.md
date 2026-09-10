# mc

A modern dual-panel terminal file manager written in Rust and Ratatui. The crates.io package is named `mc-rs`; the binary is named `mc`. It preserves the familiar Midnight Commander keys for supported operations, with a dark interface and mouse navigation.

## Build and run

Install a current stable Rust toolchain and libarchive development libraries:

- Debian/Ubuntu: `sudo apt install libarchive-dev libssl-dev pkg-config`
- Fedora: `sudo dnf install libarchive-devel openssl-devel pkgconf-pkg-config`
- macOS ARM64: `brew install libarchive openssl@3 pkg-config`. If needed, set `PKG_CONFIG_PATH` to the `lib/pkgconfig` directory under `brew --prefix libarchive` and `OPENSSL_DIR` to `brew --prefix openssl@3`.
- Windows x64/MSVC: install libarchive with `vcpkg install libarchive:x64-windows-static-md`, set `VCPKG_ROOT` to your vcpkg directory, and `VCPKGRS_TRIPLET=x64-windows-static-md`.

From this repository:

```sh
cargo run --release -- /path/to/left /path/to/right
```

Both directories are optional and default to the current directory. `--help` and `--version` work without a terminal. Run inside a terminal with keyboard and mouse support.

With mise, install the `mc-rs` package and launch its `mc` executable. After installing the build dependencies above:

```sh
env MISE_CARGO_BINSTALL=false mise use -g cargo:mc-rs@0.2.6
mise exec cargo:mc-rs -- mc --version
mise exec cargo:mc-rs -- mc
```

This explicitly uses Cargo source installation. Without `MISE_CARGO_BINSTALL=false`, mise can install matching prebuilt GitHub release binaries using the package’s cargo-binstall metadata. For bare `mc` in Fish, put `mise activate fish | source` in `~/.config/fish/config.fish` and open a new terminal. Version 0.2.6 was verified through an isolated mise source install; the package and executable names differ intentionally.

Set your terminal font to a [Nerd Font](https://www.nerdfonts.com/) (use a Mono variant for consistent cell spacing) to display file type icons. Both panels show icons for directories, symlinks, archives, source code, documents, media, and other common file types, including inside archives. Unknown types use a generic file icon. Icons are always enabled; fonts without these glyphs may show empty boxes or unrelated characters.

Installing the font alone is insufficient: select its exact family in your terminal settings. If folders look correct but source file icons look like Chinese characters, font fallback may be selecting different fonts for the Font Awesome folder glyphs and Devicons language glyphs. For example, with CaskaydiaMono installed, add this to Alacritty's `~/.config/alacritty/alacritty.toml` (see the [Alacritty font settings](https://alacritty.org/config-alacritty.html#font)):

```toml
[font.normal]
family = "CaskaydiaMono Nerd Font Mono"
```

Alacritty inherits this family for bold and italic text unless explicitly overridden. Other terminals need the equivalent font selection in their settings. Open a new terminal after changing it; no `mc` rebuild is needed.

F3 opens the built-in read-only viewer. Enter/double-click on a regular file requires `cat` on PATH, including Windows. F4 uses `VISUAL`, then `EDITOR`, then `vi` on Unix or `notepad` on Windows. Editor arguments are parsed as quoted words and invoked directly; shell expressions are not evaluated. `cat` output stays visible until Enter returns to the file manager.

Linux and macOS binaries dynamically link libarchive, which must also be installed on the destination machine. [CI run 6](https://github.com/uxsoft/mc-rs/actions/runs/34316119517) passed native builds, Rust tests, and Clippy on Linux x64/ARM64, macOS ARM64, and Windows x64, then published 0.2.6. Interactive remote runtime testing has been performed on Linux x64; other clients still need that coverage.

## Everyday operations

- Browse independent panels, sort by name/size/date through F9, toggle hidden files, and select multiple items with Space, Insert, or right-click. Selected directories are sized recursively in the background; their size appears in the Size column, and the selection total appears in each panel’s bottom border. Hidden files are included; symlinks are counted by link length without following their targets. Unreadable entries mark totals as partial. Ctrl+R refreshes cached sizes.
- Copy and move into the opposite panel, or enter a new destination/name. Same-filesystem moves use an atomic no-replace rename where possible. Cross-filesystem moves and directory merges copy before removing source data.
- F2 renames the highlighted file or directory in place, regardless of other selections. The dialog starts with its current name; edit it, use Ctrl+U to clear, then Enter to confirm or Escape to cancel. Enter a single name, not a path. Existing targets are refused, and successful renames keep the cursor on the new name. Read-only archive entries cannot be renamed.
- Jobs run in the background. Unrelated jobs can run together; jobs with overlapping source/destination paths are rejected until the running job finishes. F9 → File → Background jobs lists every job. Use arrows or the mouse to select one, `c` to cancel it, and `r` to explicitly retry a failed/cancelled job. The selected transfer shows a per-file progress bar, average speed, and estimated remaining time for that file. Retry opens fresh SSH sessions and restarts remaining top-level sources; completed top-level sources are skipped and conflicts ask again. It does not resume partial bytes or automatically replay mutations.
- Panels show listing batches as they arrive. Idle panels refresh every 3 seconds locally and 15 seconds remotely; automatic refresh pauses during jobs, modal work, and selection. Ctrl+R always requests a refresh.
- Copies preserve file/directory modification times locally and over SSH/SFTP, and ordinary Unix permission bits when the client is Unix. FTP preserves regular-file modification times when the server advertises MFMT; FTP directory times and permissions are not portable and are not preserved. Windows clients retain local native permissions but do not translate Unix modes. Archive headers supply stored modification times and Unix modes where available; ZIP/RAR DOS times use the client’s local timezone and format precision. Gzip uses its header time or the source time. Ownership, ACLs, extended attributes, special permission bits, and symlink timestamps are not copied.
- An existing file prompts for overwrite, skip, overwrite all, or skip all. Enter defaults to skip. Directory copies merge into existing plain directories; incompatible file/directory collisions stop with an error.
- F8 defaults to the operating system's trash. Choose permanent deletion explicitly with Tab or the mouse. Trash failures never fall back to permanent deletion.
- Recursive filename search matches a case-insensitive substring, reports unreadable paths, and stops at 100,000 results. Enter navigates to and highlights the selected result. Directory symlinks are not traversed.
- Type in either pane to jump to a filename prefix, ignoring case. The query appears in the pane's bottom border; no match leaves the cursor where it was. Backspace edits the query, and a 1.5-second pause starts a new query. Escape clears it; navigation, mouse actions, and file-operation shortcuts clear it and perform their usual action. Space and `+`/`-`/`*`/`\` remain selection shortcuts. Use Ctrl+S or Alt+S for the existing persistent quick search, including spaces and wildcard patterns; press it again to cycle through matches. Browsing searches only the listed names, including directories and archive/remote entries, without additional filesystem reads.

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
| Type a filename | Jump to a matching prefix; resets after 1.5 seconds |
| Ctrl+S, Alt+S | Persistent quick filename search / next match; Esc clears |
| Alt+? | Recursive filename search |
| Alt+C | Go to directory |
| Alt+. | Toggle hidden files |
| Alt+G/R/J | Top/middle/bottom of visible panel |
| Alt+O | Open cursor directory (or current directory) in other panel and advance |
| Alt+I | Same directory in both panels |
| Ctrl+U | Swap panel contents |
| Ctrl+R, Ctrl+L | Reload directory, repaint screen |
| F1 | Help |
| F2 | Rename highlighted item in place |
| F3 / F4 | Built-in viewer / external editor |
| F5 / F6 / F7 / F8 | Copy / move / mkdir / delete |
| F9 / F10 | Menu / quit |

Esc followed by a digit substitutes for a function key (`0` means F10). Esc followed by a letter substitutes for Alt. Input dialogs support arrows, Home/End, Backspace/Delete, Ctrl+A/E/B/F/H/D/K/U. Keyboard bindings are fixed; there is no configuration system.

Click to focus a panel and position the cursor; right-click toggles selection; double-click opens; the wheel scrolls. The persistent File / View / Go / Help bar opens dropdowns beneath each label. F9 opens File; Left/Right or Tab switch menus, Up/Down select actions, Enter activates, and Esc/F9 dismisses. Mouse hover switches open menus and highlights actions; clicking outside dismisses them. Function-key labels and menu choices are clickable. In deletion dialogs, click the deletion mode, then confirm.

## Built-in file viewer

F3, the clickable **View** function key, and **View → View file** open the highlighted file in a full-screen, read-only viewer. Escape or `q` returns to the same panels, selection, and cursor. Enter/double-click retain their existing directory/archive navigation and external `cat` behavior; F4 retains the external editor.

- Text and code show line numbers, preserve indentation, and scroll in both directions. Code syntax is detected from the filename, extension, or shebang and highlighted with bundled syntaxes. Unknown binary files show hexadecimal bytes and ASCII.
- Markdown (`.md`, `.markdown`, `.mdown`, `.mkd`, case-insensitive) renders headings, emphasis, lists, task lists, quotes, tables, links, and highlighted fenced code. Prose wraps on resize; code blocks keep their layout. Image references show their descriptions and paths. Links and embedded HTML are inert; viewing never fetches referenced content.
- Images support PNG, JPEG, GIF, WebP, BMP, TIFF, and ICO. Animated and multi-image files show their first frame/page. Images fit the screen initially.
- PDFs render actual pages, including raster/scanned content, using the bundled Hayro renderer and fonts. Pages initially fit the width. Encrypted PDFs and unsupported or damaged documents may fail with an error; advanced PDF rendering features are subject to Hayro’s support. No PDF command or additional native rendering library is required.
- Native Kitty, Sixel, and iTerm2 graphics are used when protocol support and cell dimensions can be detected. Unix terminals receive a bounded capability probe; other platforms use available terminal information and environment hints. Other terminals use colored Unicode half-blocks, which provide much lower resolution, especially for PDF text. All rendering and image encoding happen in background workers.

For fastest photo/PDF rendering, use `cargo run --release -- [LEFT_DIRECTORY] [RIGHT_DIRECTORY]` or the prebuilt release. The development build has additional runtime overhead. Half-block rendering samples the visible image directly at terminal resolution, avoiding an intermediate full-resolution resize.

| Viewer controls | Action |
| --- | --- |
| Escape / `q` | Close; cancel pending loading |
| Up/Down / mouse wheel | Scroll vertically |
| Left/Right | Scroll horizontally / pan |
| PageUp/PageDown | Scroll one screen |
| Home/End | Beginning/end; End follows loading until indexing finishes |
| `+` / `-` / `0` | Image/PDF zoom in / out / initial fit |
| `n` / `p` | Next/previous PDF page |

The header shows the format, row or PDF page, and loading/rendering progress. File-operation shortcuts are consumed while viewing. Authentication and background-job conflict prompts still take priority. Image and PDF frames retain their pixel colors even with `NO_COLOR` set; the panels continue to honor that setting.

Large files are not truncated at a preview limit. Text loads progressively into private temporary files with a disk-backed row index and persisted highlighting, so distant navigation does not need to rehighlight preceding lines. A text/code line over 256 KiB remains fully viewable with plain styling; syntax highlighting restarts after that line. Markdown and PDF input use immutable, privately owned mapped snapshots; images and PDFs wait for their input to finish loading. Only visible text rows and one PDF page bitmap are retained for display. Input size is limited by available temporary storage; decoded images and rendered pages are limited to 64 megapixels (256 MiB RGBA), with additional parser/decoder working memory. Reduce zoom if a PDF page exceeds the rendering limit. Errors report failed reads, decoding, or temporary-file writes.

**Temporary storage:** F3 may write decrypted archive or remote content to private, automatically removed temporary files. These files are released when the viewer closes or its workers finish cancellation, including error paths. This changes the old F3 memory-only streaming behavior; archive browsing and its existing seek caches remain memory-only. Temporary data is not encrypted or securely erased, and OS swap/core dumps are outside this guarantee. Cancelling an in-flight network call or decoder may delay worker cleanup, but the file panels return immediately.

## Archives

Press Enter to browse ZIP-based application files just like ZIP archives: JAR/WAR/EAR; Word DOCX/DOCM/DOTX/DOTM; Excel XLSX/XLSM/XLSB/XLTX/XLTM/XLAM; PowerPoint PPTX/PPTM/POTX/POTM/PPSX/PPSM/PPAM/SLDX/SLDM; Visio VSDX/VSDM/VSSX/VSSM/VSTX/VSTM; and Office THMX themes. Extensions are case-insensitive. F3 views a member and F5 copies it out, including nested archives and remote sources. These files retain their application-specific icons where available. Legacy DOC/XLS/PPT files are not ZIP containers and are not browsable archives. Format references: [JAR](https://docs.oracle.com/javase/8/docs/technotes/guides/jar/index.html) and [Office formats](https://learn.microsoft.com/en-us/office/compatibility/office-file-format-reference).

Enter opens ZIP, RAR, tar, 7z, `.tar.gz`, `.tgz`, or `.gz` in a read-only panel. F5 copies entries to a local destination; F3 opens a file in the built-in viewer. Parent navigation at the archive root returns to the containing directory.

Archive panels keep an entry index in memory. Navigation, filename search, and directory sizing use that index; payloads are decoded only when reading/copying a member. Enter/double-click sends a regular member to `cat` on stdin. F3 uses the built-in viewer described below. Copies use the destination provider's staged write handle; local copies need space only for the file being copied, not the whole browsing tree.

ZIP (AES and ZipCrypto), 7z, and RAR can request a password. Enter submits, Esc cancels; incorrect passwords can be retried. The field is masked and credentials stay in the archive session, never in paths, command arguments, or configuration files. The application's password buffers are zeroized when dropped; decoder libraries may keep their own copies. Passwords are forgotten when the last panel, job, or open handle releases that session.

ZIP uses the Rust `zip` decoder, 7z uses `sevenz-rust2`, RAR uses `rars`, tar uses libarchive, and gzip uses flate2. No archive CLI helpers are needed. ZIP supports stored/deflated content; other compression methods may report unsupported. Multipart archives and archive modification remain unsupported. Nested archives use a memory-only seek cache capped at 64 MiB per member, with at most eight archive levels. Remote and nested RAR inputs are also limited to 64 MiB because the decoder needs an owned input buffer. Larger inputs fail with instructions to copy the archive locally. Archive seek caches never spill plaintext to disk; F3 has separate viewer temporary storage as described above. Cache limits exclude archive indexes, decoder dictionaries, and library-owned copies.

Decoded output uses a bounded channel (two 64 KiB chunks), plus decoder dictionaries and the metadata index. A validation pass precedes streaming the selected member, so password retries cannot release incorrect plaintext; this reads selected content twice. Solid archives can require decoding preceding members too. Compressed tar header scanning and standalone gzip size calculation may require decompressing the compressed stream, without writing it to disk. Some RAR5 transforms need buffered decoding, capped at 32 MiB; decoder dictionary memory is additional.

Archive paths reject absolute paths, traversal, Windows path syntax, and duplicate/conflicting names. Unsupported link/special entries fail rather than being restored as filesystem links. If the backing archive's size or modification time changes, leave and reopen it.

Local files, archives, and remote servers share a provider interface, typed locations, and owned handles inspired by Midnight Commander's VFS. See [VFS.md](VFS.md) for the architecture and provider guarantees.

## Remote connections

Use **Go → FTP / SFTP / SSH connection**, enter a URL through **Alt+C**, or pass URLs as startup locations:

```sh
mc 'sftp://alice@example.com/home/alice' /local/downloads
mc 'ssh://alice@example.com:2222/home/alice' 'ftp://user@files.example.com/public'
```

- `sftp://` uses the server's SFTP subsystem. `ssh://` works without SFTP by running an embedded Python 3 helper on a Unix server through its SSH login shell. The helper receives paths and content through stdin; filenames never become shell commands. It is not installed on the server.
- For `ssh://`, noninteractive shell startup must not read stdin or print to stdout. Keep banners and terminal utilities inside an interactive-shell guard (in Fish: `if status is-interactive` … `end`). SFTP avoids the login-shell helper.
- SSH reads `~/.ssh/config`: `Host` patterns/negation, `Include` (filename wildcards), `HostName`, `User`, `Port`, `IdentityFile`, `IdentitiesOnly`, `StrictHostKeyChecking`, and `ProxyJump` (including comma-separated hops). Explicit URL user/port overrides configuration. The first matching scalar value wins; identity files accumulate. Key paths support `~/` and `%d/%h/%n/%r/%p/%%`. This is a non-executing subset: Match, ProxyCommand, HostKeyAlias, UserKnownHostsFile, and CertificateFile are unsupported and fail explicitly when applicable. System SSH config is not read.
- Authentication tries the agent (unless IdentitiesOnly), configured/default keys, then masked keyboard-interactive/MFA and password/key-passphrase prompts. MFA responses are masked even if the server requests echo. Default keys are `~/.ssh/id_ed25519` and `~/.ssh/id_rsa`.
- Existing host keys must match `~/.ssh/known_hosts`; changed keys always fail. For an unknown host, mc displays its SHA256 fingerprint. Verify it independently and type `trust` to accept it for that connection only. It does not modify known_hosts; use your SSH client to record permanent trust. `StrictHostKeyChecking yes` disables session trust and requires a matching known_hosts entry. Each jump host is authenticated and verified separately, with at most eight hops.
- `ftp://` is plain, unencrypted FTP using passive transfers. It defaults to anonymous login when the user is omitted. Named users get a masked password prompt. FTPS is not implemented. Passwords are rejected in URLs and are never saved in configuration.
- Paths are absolute on the server. Spaces and URL delimiters can be percent-encoded. UTF-8 names are supported; control characters and backslashes are rejected. Within a remote panel, relative and absolute paths stay on that server. **Go → Local directory** returns to the process's local working directory; `file:///absolute/path` also opens a local location.
- Browse, select/size directories, search filenames, F3 view files, and copy/move/mkdir/permanently delete using the normal keys and background jobs. Remote files have no trash: F8 retains the trash default and explains that permanent deletion must be chosen explicitly. Remote editing and creating remote symlinks are not implemented.
- Uploads are staged on the destination server. SSH publishes with atomic no-replace or explicit replacement; SFTP uses server rename semantics and fails without deleting the old destination if replacement is unsupported. FTP checks for conflicts immediately before rename, but the protocol cannot prevent a race with another client's concurrent changes. Failed connections may leave staging files for manual cleanup. Mutations are never automatically retried.
- F2 uses a server-side rename for FTP, SFTP, and SSH. SSH requires an atomic no-replace rename primitive on the server (Linux `renameat2` or macOS `renamex_np`); unsupported servers fail without copying or deleting. FTP has the same concurrent-client race described above. If a connection fails during rename, inspect both names before retrying.
- Transfers involving remote providers check the source byte count, and upload commits verify the staged file size before publication. These checks detect truncation or size changes, not same-size content changes. Failed moves retain their source. Job errors include the staging location if cleanup may need attention. If publication cannot be confirmed after a disconnect, inspect the destination before retrying: the server may already have completed the rename.
- FTP seeking uses REST and new transfer connections, so servers must support REST for random access. ZIP, tar, 7z, gzip, and bounded RAR inputs can use remote sources without a local extracted tree. Archive indexing may read significant remote data.

Esc cancels connection/authentication work. Network calls have ten-second socket/SSH timeouts; cancellation can wait for an in-flight call or DNS lookup. Remote locks conservatively cover an entire user/host/port endpoint, including archive backing files. Reconnect via the connection menu after leaving an expired mount; active failed jobs are not resumed.


## Development and validation

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
python3 tests/terminal_smoke.py  # Linux/Unix pseudo-terminal integration test
python3 tests/archive_smoke.py   # encrypted archives, built-in viewer, copy, retry/cancel
python3 tests/viewer_smoke.py    # scrolling, Markdown, image/PDF rendering, input isolation
MC_TEST_GRAPHICS=kitty python3 tests/viewer_smoke.py  # native protocol and repaint regression
python3 tests/large_directory.py # 20,000 entries, keyboard response, automatic refresh
# In a Python environment with pip:
python3 -m pip install -r tests/requirements-remote.txt
python3 tests/remote_servers.py  # isolated loopback FTP/SFTP/SSH + terminal tests
python3 tests/openssh_server.py # real OpenSSH; requires openssh-server, run as a normal user
```

Tests use disposable files. The terminal smoke test redirects the Linux trash location to its own temporary directory. It exercises the built-in viewer, Enter’s external cat behavior, copying, moving, mkdir, both deletion modes, search-result selection, mouse input, resize, and terminal restoration.

## License and references

GPL-3.0-or-later; see [LICENSE](LICENSE). This project is an independent implementation inspired by [Midnight Commander](https://github.com/MidnightCommander/mc), with behavior checked against its [manual](https://source.midnight-commander.org/man/mc.html). It is not an official Midnight Commander release. RAR and 7z test fixtures come from compress-tools and carry their own MIT notice under `tests/fixtures`.
