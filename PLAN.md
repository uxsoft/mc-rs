# mc — implementation plan and session handoff

Last updated: 2026-09-08
Current phase: First usable implementation complete on Linux x64. M1–M5 functionality implemented; M6 cross-platform native verification and non-Linux release artifacts remain pending.

## Working agreement

- Read this file before resuming implementation.
- Keep this file updated in the same work session as implementation changes: mark completed items, record validation actually run, and update the next action and unresolved decisions.
- Distinguish implemented and verified behavior from planned behavior. Do not mark a milestone complete merely because code exists.
- Preserve the user's decisions below. Resolve ordinary implementation details autonomously; record assumptions here.
- The user has now authorized implementation. Continue implementation and verification without asking again for ordinary development steps.

## Product decisions confirmed by the user

- Build a modern file manager in Rust and Ratatui; binary remains named `mc`.
- Preserve MC shortcuts strictly for supported operations, including mouse support. Modernize the appearance with a restrained dark theme, clear panel focus, and a familiar function-key bar.
- Prioritize everyday browsing, moving, copying, and deleting.
- Initial scope: dual panels, navigation, sorting, selection, copy/move/delete, mkdir, filename search, external editor launching, and archives.
- Platforms: Linux x86_64 and ARM64, macOS ARM64, Windows x86_64.
- Support background file operations with progress and cancellation.
- Delete dialog offers trash or permanent deletion; trash is the default.
- View files using the external `cat` command. Assume `cat` is available on Windows too. Do not implement a built-in viewer or editor.
- No MC shell functionality: no command prompt, persistent subshell, Ctrl+O shell switching, or command execution interface. Directly launching the viewer/editor is still in scope.
- Required archive formats: ZIP, RAR, tar, 7z, and gzip.
- Filename search only; no content search.
- SFTP, FTP, and SSH-based remote browsing are deferred.
- No customization system or importing MC configurations, keymaps, skins, extension rules, or user menus.
- GPL-3 license is acceptable. Planned project license identifier: GPL-3.0-or-later, matching upstream; retain attribution for adapted material.

## Sources and compatibility references

- Canonical upstream: https://github.com/MidnightCommander/mc
- Official source confirmation: https://midnight-commander.org/source-code/
- Upstream manual: https://source.midnight-commander.org/man/mc.html
- Upstream README and license: https://github.com/MidnightCommander/mc#readme and https://github.com/MidnightCommander/mc/blob/master/COPYING

Use upstream behavior, documentation, and relevant source as references for keyboard and file-operation semantics. This is a modern implementation, not a requirement for complete feature parity or a line-by-line port. Verify detailed bindings against upstream before implementing them; record the supported subset in project documentation.

## Defaults to use unless later changed

- Archive support initially means browsing and extracting. Archive creation and in-place modifications were not explicitly requested and are deferred. Do not present archive entries as writable files.
- Gzip is a compressed stream rather than a directory container: support decompression of `.gz` and browsing/extracting compressed tar archives such as `.tar.gz`.
- Filename search is recursive from the active directory, cancellable, with results that can navigate to the containing directory and select the match. Do not follow directory symlinks recursively by default. Archive-content search is deferred.
- F4 launches an external editor selected from `VISUAL`, then `EDITOR`, then a platform fallback determined during implementation. This conventional environment integration is not an application customization system. Report a missing editor clearly.
- File viewing suspends the TUI, invokes `cat` directly with the selected path, and leaves its output visible until the user returns to the TUI. Handle terminal restoration on errors and cancellation. No pager or internal viewer.
- Remote destinations, plugin systems, archive writing, content search, embedded terminals, and a built-in editor are outside the first release.

## Architecture

### Workspace and module boundaries

Keep the existing `mc/` Rust package and binary. Start with one package with a testable library and thin binary entry point; split crates only when a concrete need emerges.

Proposed modules:

| Module | Responsibility |
| --- | --- |
| `app` | Application state, action dispatch, focus, dialogs, and event reduction |
| `ui` | Ratatui rendering, layout, theme, dialogs, function-key bar, and mouse hit regions |
| `input` | Terminal events to semantic actions; fixed MC-compatible bindings |
| `panel` | Directory location, listing, sorting, selection, cursor, and refresh reconciliation |
| `fs` | Local filesystem access, metadata, path identity, and platform differences |
| `jobs` | Background operation planning, execution, progress, conflicts, and cancellation |
| `archives` | Format detection, listings, extraction, and backend capability reporting |
| `search` | Recursive filename search and streamed results |
| `external` | Direct viewer/editor process launching and terminal lifecycle |

Select exact dependency versions during implementation from current official documentation. Ratatui is fixed by the requirements. Choose a cross-platform terminal event backend, trash implementation, and archive backends after checking platform support. Avoid introducing an async runtime unless the chosen implementation needs one; bounded worker threads and channels are the initial design.

### Event loop and state

- One UI thread owns state and renders it; workers never draw or mutate panel state directly.
- Translate keyboard/mouse events into semantic actions, then update state through a central dispatcher.
- Workers send bounded/coalesced events for listings, search results, job progress, conflicts, and completion so large jobs cannot overwhelm the UI.
- Tag listing/search requests with identifiers and discard stale responses after navigation.
- Keep filesystem I/O and archive indexing off the rendering path. Refresh affected panels after jobs while preserving cursor and selection where entries still exist.
- Maintain explicit modes for panels, menus, dialogs, search results, and jobs to avoid shortcut ambiguity.

### Panels and navigation

- Independent left/right locations, sorting, scroll positions, and selections. Tab changes focus.
- Store real paths with `PathBuf`/`OsString`; display strings must not become filesystem identities. Handle Unix non-UTF-8 names and Windows drive/UNC paths.
- Distinguish directories, regular files, symlinks, and archive locations. Keep archive entry identifiers separate from local paths.
- Provide parent navigation, hidden-file visibility, selection toggles, sorting controls, and readable size/time metadata.
- Mouse navigation, selection, scrolling, panel focus, dialogs, and function-key activation must share the same semantic actions as keyboard input.
- Preserve navigation responsiveness on large directories and slow storage.

### File operation engine

- Represent each job with sources, destination, operation, state, progress, and cancellation signal. Keep execution independent of the TUI for testing.
- Operate on selected items, or the cursor item when no selection exists, using MC behavior as the reference.
- Copy directories recursively; move by rename where possible, with copy-then-delete across filesystems. Never remove a source before its corresponding copy succeeds.
- Define collision handling explicitly: ask, overwrite, skip, and apply-to-remaining options. Prevent same-file copies and copying a directory into its own descendants.
- Preserve symlinks as links by default; document which permissions, timestamps, and other metadata each platform preserves.
- Copy to temporary destination files where practical, then finalize successful copies. Cancellation must leave pre-existing destination data intact and clean up only job-owned temporary data. Report partial completion for multi-file jobs.
- Cancellation is cooperative between chunks/entries; do not promise rollback of already completed work.
- Trash and permanent deletion are distinct operations. If trash is unavailable, report it and require explicit permanent-delete selection; never silently fall back to permanent deletion.
- Jobs continue while browsing. Keep progress, errors, and conflict prompts accessible. Prevent unsafe overlapping operations on the same paths. Confirm quitting while jobs are running.

### Archive access

- Provide a narrow archive interface for listing and extraction, with capability/error reporting per format. Avoid building a generic remote VFS before it is needed.
- M1 must establish workable ZIP, RAR, tar, 7z, and gzip backends on all four targets. Prefer maintained libraries when suitable; external helpers are acceptable only with documented availability and actionable missing-helper errors. Only `cat` is currently assumed installed by the user.
- Enter opens an archive as a browsable location; copying entries to a local panel extracts them through the job engine. Moving/deleting archive entries is unavailable in the initial read-only model.
- Validate extracted paths, links, and destination traversal to prevent writing outside the chosen destination. Handle malicious absolute paths, `..`, and symlink escapes.
- Extract entries needed by `cat` or an external editor to owned temporary storage. Initially allow viewing only inside archives; do not imply editor changes will be written back.
- Surface unsupported encryption, corrupt archives, and unsupported format variants explicitly. Document encrypted/multipart archive support based on backend capability; these are not yet promised.

### External processes and terminal ownership

- Use process APIs directly, with arguments separate from program names; never interpolate paths into shell commands.
- Suspend raw mode, mouse capture, and the alternate screen before external programs; restore them reliably afterward, including spawn failures and interrupts.
- Maintain background-job event delivery while an external process runs without writing TUI output onto that process's terminal.
- Treat filenames beginning with options safely when invoking `cat` and support spaces and non-ASCII paths.

## Milestones and acceptance criteria

### M0 — Requirements and handoff (complete)

- [x] Locate canonical MC source and documentation.
- [x] Record user requirements, explicit exclusions, and implementation defaults.
- [x] Inspect starter package and write this resumable plan.

### M1 — Foundation and platform feasibility (implemented; native matrix pending)

- [x] Add Ratatui and terminal lifecycle management; retain binary name `mc`.
- [x] Create testable application/action structure and restrained dark dual-panel layout.
- [x] Add GPL license and relevant attribution.
- [ ] Establish build/check coverage for all target architectures and runtime validation on available native platforms.
- [x] Verify archive backend strategy for every required format and target; record dependencies and capability limits here.
- [x] Verify trash support strategy on Linux, macOS, and Windows.

Acceptance: application opens, resizes, exits, and restores the terminal; backend feasibility is documented. Cross-compilation alone is not evidence of native runtime correctness.

### M2 — Browsing, keyboard, and mouse (implemented; Linux smoke verified)

- [x] Load directories asynchronously into independent panels.
- [x] Implement navigation, sorting, hidden files, selection, and fixed MC-compatible shortcuts.
- [x] Implement mouse focus, navigation, scrolling, and selection.
- [x] Add menus/dialog infrastructure and function-key bar.
- [x] Handle permission errors, disappearing files, symlinks, unusual filenames, and Windows locations.

Acceptance: everyday browsing works with keyboard and mouse; large-directory reads do not block input; supported shortcuts are checked against upstream.

### M3 — Copy, move, mkdir, and deletion (implemented; Linux verified)

- [x] Implement background jobs, visible progress, cancellation, and conflict prompts.
- [x] Implement recursive copies, renames, cross-filesystem moves, and mkdir.
- [x] Add trash-default deletion dialog with explicit permanent-delete option.
- [x] Reconcile panel contents after operations and show partial failures clearly.
- [x] Handle overlapping jobs and exit with active jobs.

Acceptance: real operations succeed in disposable test directories; tests cover collisions, cancellation, nested directories, symlinks, failed copies, and unavailable trash. Source data survives failed moves.

### M4 — Filename search and external tools (implemented; Linux smoke verified)

- [x] Add recursive, cancellable filename search and result navigation.
- [x] Implement F3 external `cat` viewing with readable output and return-to-TUI flow.
- [x] Implement F4 external editor launching for local files.
- [x] Verify terminal recovery after cat and missing-editor failures.
- [ ] Verify concurrent background job completion/conflicts while external tools own the terminal.

Acceptance: search remains responsive; viewer/editor paths containing spaces work; failures restore a usable terminal.

### M5 — Required archives (implemented; all formats tested on Linux)

- [x] Browse and extract ZIP, RAR, tar, and 7z.
- [x] Decompress gzip streams and browse/extract `.tar.gz`.
- [x] Route extraction through background jobs with progress/cancellation where backend permits.
- [x] View archive files through temporary extraction and `cat`.
- [x] Reject unsafe extraction paths and clearly disable unsupported mutations.
- [x] Document format variants, encrypted/multipart limitations, and any runtime helpers.

Acceptance: representative fixtures for all five formats pass; archive traversal cannot escape the destination; unsupported/corrupt archives produce useful errors.

### M6 — First-release verification and packaging (partially complete)

- [x] Complete supported-shortcut and mouse interaction review.
- [x] Run meaningful unit/integration tests, formatting, and lint checks.
- [x] Validate terminal restoration and resize/small-window behavior.
- [ ] Stress-test large directories and active-job shutdown interactively.
- [ ] Validate native behavior for Linux x64/ARM64, macOS ARM64, and Windows x64, recording any unavailable platform as unverified.
- [x] Provide installation/build instructions, supported behavior, shortcuts, and known limitations.
- [ ] Build release artifacts for the four targets.

Acceptance: browsing/copying/moving/deleting and required archives are usable, checks are recorded, and platform limitations are explicit. Publishing artifacts is a separate action from preparing them.

## Validation strategy

- Test file-operation correctness with temporary directories and fault cases, especially data-loss boundaries, conflicts, cancellation, and cross-filesystem fallback.
- Test archive extraction with benign and traversal/link fixtures. Avoid relying only on tests that mirror implementation details.
- Test action mapping, panel selection reconciliation, and dialog defaults. Use Ratatui render tests selectively for meaningful layout/focus behavior.
- Perform interactive terminal smoke checks for keyboard, mouse, external processes, and restoration; capture actual platform coverage.
- Use CI to build/check each target, with native runtime checks where runners are available. Record compile-only coverage separately.

## Current repository state and progress log

2026-09-08 — planning:
- Inspected the existing Rust 2024 `mc` package (Hello World, no dependencies) and wrote this plan.
- The starter package was untracked. No pre-existing code or user changes were discarded.

2026-09-08 — implementation:
- Implemented a library plus thin binary, using Ratatui 0.30.2 and Crossterm 0.29; exact dependencies are locked in `mc/Cargo.lock`.
- Added asynchronous directory snapshots, independent panels, selection, sorting, mouse navigation, fixed MC bindings, quick search, and editable dialogs.
- Added recursive filename search with cancellation, result selection, unreadable-path count, and a 100,000-result cap.
- Added background copy/move/mkdir/trash/delete jobs, byte/file counters, cancellation, conflict decisions, and conservative path locks allowing disjoint jobs to run simultaneously.
- Same-filesystem moves first attempt an atomic no-replace rename; cross-device moves and directory merges use copy-before-delete. File copies stage in destination-owned temporary files before finalization. Symlinks are preserved; special files are rejected.
- Added F3 external `cat` and F4 editor, terminal suspend/resume, Ctrl+C handling, panic cleanup, missing-program reporting, and a return prompt after cat.
- Added read-only ZIP/RAR/tar/7z/gzip archive panels. Archive loading unpacks fully into private temporary storage in a cancellable worker. F5 copies extracted entries to a local panel. Traversal paths, links/special entries, and duplicate files are rejected.
- Added GPL-3.0-or-later license, README, fixture attribution, and a native GitHub Actions matrix for all four targets. CI is configured but has not been dispatched or observed running.

Selection follow-up:
- Space now toggles files/directories and advances, matching Insert/Ctrl+T. Right-click continues to toggle without advancing.
- Selecting a directory queues a recursive size scan in a background worker (one queued traversal at a time per panel). The Size column displays a calculating indicator, then logical bytes; the panel’s bottom border shows the combined selected size.
- Counts hidden files and link lengths without following symlinks. Unreadable entries mark totals as partial. Deselecting cancels an active scan; navigating, refreshing, and dropping the panel cancel/discard stale scans. Reselecting recalculates a directory.
- Added tests for nested/hidden-file totals, deselection/reselection, cancellation, inaccessible/missing paths, and symlink cycles. Extended the PTY smoke test to select two directories with Space, verify their combined size, then copy and move both.

Layout follow-up:
- Removed the filename/Parent directory row and Ready/job-status row above the function-key bar. Panels now use the two reclaimed rows.
- Selection totals remain visible in each panel’s bottom border. Directory sizes remain in the Size column; background-job details are available from F9 → Background jobs.
- Page navigation and visible-row shortcuts now use the actual panel height.
- Verified with formatting, strict Clippy, all 22 integration tests, and the PTY smoke test. Rebuilt the Linux x64 release binary and package.

### Backend decisions and documented limits

- `compress-tools` 0.16.1 uses system libarchive (3.7.2 on this host) for ZIP/RAR/tar/7z/compressed tar; `flate2` handles standalone gzip. No archive CLI helpers are required.
- Linux/macOS discover libarchive using pkg-config. Windows/MSVC uses vcpkg with `x64-windows-static-md`. The backend advertises these platform paths; only Linux runtime behavior is verified here.
- `trash` 5.2.8 supplies Freedesktop trash, macOS trash, and Windows Recycle Bin implementations. Linux trash behavior is exercised in an isolated test environment. No permanent-delete fallback exists.
- Editor fallback is `vi` on Unix and `notepad` on Windows; VISUAL/EDITOR arguments use word parsing, never shell evaluation.
- Filename search uses case-insensitive substrings. Selection uses case-sensitive `*`/`?` wildcards, selecting files by default. Quick search supports wildcard prefixes and next-match cycling.
- Archives are initially fully extracted, requiring uncompressed-size temporary disk space. No archive writing, nested mounts, password UI, or multipart support. Malformed and unsupported archives report errors.
- Copies preserve ordinary permissions but not timestamps, ownership, ACLs, extended attributes, sparse layout, or hard-link relationships. Windows symlink creation depends on OS privileges; replacing a symlink may fail safely.
- Cancellation is cooperative and does not roll back completed items. OS trash calls cannot be cancelled mid-call. Jobs are not persisted across application exit.
- Listing workers deliver full snapshots; no filesystem watcher. Superseded receivers are discarded, preventing stale results from replacing current navigation.
- The UI uses fixed supported MC shortcuts. Omitted upstream features (shell, internal viewer/editor, customization, remote access) have no substitute shortcut actions. This does not claim full upstream MC parity.

### Validation actually completed

- `cargo fmt --manifest-path mc/Cargo.toml --check`.
- `cargo clippy --manifest-path mc/Cargo.toml --offline --all-targets -- -D warnings`.
- `cargo test --manifest-path mc/Cargo.toml --offline`: 22 integration tests passed, covering recursive operations, failed/skipped moves, cancellation during copy and conflict, symlinks (including dangling links), unsafe destination directories, all five archive formats, archive traversal/links, cancelled archives, stale listing responses, Unicode input, wildcard selection, path locks, and small/normal rendering.
- `python3 mc/tests/terminal_smoke.py`: Linux PTY smoke passed for cat, copy, move, mkdir, default trash, explicit permanent deletion, search-result focus, mouse function-key activation, resize, terminal restoration, a missing external editor, and trash failure without permanent-delete fallback.
- Native debug and release builds succeeded for `x86_64-unknown-linux-gnu`. Packaged `dist/mc-0.1.0-x86_64-unknown-linux-gnu.tar.gz` with the binary, license, README, and plan.
- Linux ARM64, macOS ARM64, and Windows x64 have not been compiled or run in this session. Native CI and release artifacts for those platforms remain unverified.

## Next session

1. Read this plan and README; inspect current changes before editing.
2. Run the configured native CI matrix when repository hosting/runners are available. Resolve any macOS/Windows build, libarchive linking, trash, or terminal differences. Do not mark those platforms verified until actual results exist.
3. Complete M6 native runtime checks and artifact generation for Linux ARM64, macOS ARM64, and Windows x64. Private repositories may need a different ARM runner entitlement/label.
4. Exercise larger real-world directories and archives interactively; expand tests only for failures or unresolved concerns. Audit the supported shortcut subset against the manual as functionality expands.
5. Consider incremental archive indexing, metadata preservation, and improved per-job controls as follow-up improvements; preserve the requested feature scope.

Run locally from the repository root:

```sh
cargo run --manifest-path mc/Cargo.toml --release -- /left/directory /right/directory
```

Deferred backlog: SFTP, FTP, and SSH-based remote access; archive creation/modification only if requested later.
