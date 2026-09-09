# mc — implementation plan and session handoff

Last updated: 2026-09-09
Current phase: Main proposals 3–7 implemented and verified on Linux x64; changes remain local. Published 0.2.6 and all four native CI build/test jobs verified. Interactive remote runtime coverage outside Linux x64 remains pending.

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
- FTP, SFTP, and SSH remote browsing are implemented through the shared MC-inspired VFS. SSH uses a Python 3 helper and does not require SFTP. See the remote implementation progress entry below and VFS.md for guarantees and limitations.
- Archive browsing retains metadata in memory and streams requested content; implement password prompts, retries, cancellation, and session-only credentials.
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
- Filename search is recursive from the active directory, cancellable, with results that can navigate to the containing directory and select the match. Do not follow directory symlinks recursively by default. Search within the current archive VFS uses the same traversal; searching unopened archive contents is deferred.
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
| `vfs` / `vfs::local` | Backend trait, session-aware paths, metadata, read/write handles, cancellation/authentication, local platform operations |
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

- Use `vfs::FileSystem` and `VfsPath` for both local and archive operations. This supersedes the initial narrow archive-only design, as explicitly requested by the user. See `VFS.md` for upstream source references and the backend contract.
- M1 must establish workable ZIP, RAR, tar, 7z, and gzip backends on all four targets. Prefer maintained libraries when suitable; external helpers are acceptable only with documented availability and actionable missing-helper errors. Only `cat` is currently assumed installed by the user.
- Enter opens an archive as a browsable location; copying entries to a local panel extracts them through the job engine. Moving/deleting archive entries is unavailable in the initial read-only model.
- Validate extracted paths, links, and destination traversal to prevent writing outside the chosen destination. Handle malicious absolute paths, `..`, and symlink escapes.
- Stream archive members to external `cat` stdin or a staged destination handle. Do not extract a browsing tree or create plaintext viewer temporary files. Archive editing remains unavailable.
- ZIP AES/ZipCrypto, 7z AES, and RAR password handling use masked prompts and session credentials. Multipart archives remain unsupported; decoder errors and capability limits are explicit.

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

### M1 — Foundation and platform feasibility (implemented; native CI verified)

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
- [x] View archive files through a bounded VFS stream into `cat` stdin (supersedes temporary extraction).
- [x] Reject unsafe extraction paths and clearly disable unsupported mutations.
- [x] Document format variants, encrypted/multipart limitations, and any runtime helpers.

Acceptance: representative fixtures for all five formats pass; archive traversal cannot escape the destination; unsupported/corrupt archives produce useful errors.

### M5b — MC-inspired VFS and lazy encrypted archives (implemented; Linux x64 verified)

- [x] Read upstream VFS class/path/inode and tar implementation sources; record references in `VFS.md`.
- [x] Introduce provider-dispatched metadata, listings, streams, staged writes, mutations, capabilities, and session-aware paths.
- [x] Migrate panels, directory sizing, filename search, copy/move/delete, resource locks, and viewer dispatch.
- [x] Replace extracted archive mounts with an immutable entry/child index and lazy decoded streams.
- [x] Add session password prompts, masking, retry, cancellation, and authenticated content validation.
- [x] Verify all five formats and encrypted ZIP/7z/RAR; corrupt ZIP detection now uses the ZIP Rust reader because compress-tools treats libarchive data warnings as success.
- [x] Complete independent VFS provider tests, terminal archive/password interaction checks, strict lint, and release rebuild.

### M6 — First-release verification and packaging (partially complete)

- [x] Complete supported-shortcut and mouse interaction review.
- [x] Run meaningful unit/integration tests, formatting, and lint checks.
- [x] Validate terminal restoration and resize/small-window behavior.
- [ ] Stress-test large directories and active-job shutdown interactively.
- [ ] Validate native behavior for Linux x64/ARM64, macOS ARM64, and Windows x64, recording any unavailable platform as unverified.
- [x] Provide installation/build instructions, supported behavior, shortcuts, and known limitations.
- [x] Build release artifacts for the four targets (CI run 6, 0.2.6).

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

Application menu follow-up:
- Replaced the centered F9 menu dialog with a persistent File / View / Go / Help bar and anchored dropdowns.
- Shared menu definitions provide action labels, shortcut hints, and dispatch. View marks the current sort and hidden-file setting.
- F9 opens File; Left/Right/Tab switch categories, Up/Down/Home/End navigate, Enter activates, Esc/F9 closes. Mouse clicks/hover and outside dismissal are supported. Dropdowns scroll on small terminals.
- File operations reuse their existing confirmation dialogs; background jobs moved to File → Background jobs.
- Verified with strict Clippy, all 22 tests (including dropdown bounds/scrolling), and PTY keyboard/mouse menu interactions. Rebuilt the Linux release.

VFS and encrypted archive follow-up:
- Studied upstream `vfs.h`, `path.h`, `xdirentry.h`, `interface.c`, and `tar.c`. Added `VFS.md` with source links, design mapping, the backend contract, and future SSH/SFTP/FTP integration work.
- Replaced temporary-directory archive mounts with provider-owned metadata/children indexes. `VfsPath` carries backend identity; local platform operations are isolated in `vfs/local.rs`. Panels, search, directory sizes, jobs, locks, and file reading dispatch through the shared interface.
- Added bounded member streams, password UI/retry/cancel, session-only credentials, and streamed `cat` stdin. Completed job records release their resource/session references in the UI. No browsing tree or plaintext viewer temporary file is created.
- Added ZIP AES/ZipCrypto, 7z AES, and RAR password handling with Rust decoders. Password tests cover wrong/correct passwords, reuse, and encrypted 7z/RAR headers. ZIP CRC verification uses the Rust ZIP reader because compress-tools accepts libarchive data warnings without surfacing checksum failure.
- Added independent in-memory-provider tests for metadata-only panel listings, cross-provider copy, and ZIP mounting over nonlocal seekable transport. Added cancellation/drop and archive lifetime/read-only tests.
- All 30 integration/contract tests, strict Clippy, formatting, existing PTY smoke, and new archive PTY smoke passed. The new PTY check verifies directory sizes from metadata, masked retry/cancel, streamed cat, password reuse, copy out, mutation rejection, and parent navigation.
- Rebuilt Linux x64 release and package. Updated CI to run the archive PTY check on Linux and include VFS/plan documents in artifacts. Other native targets remain unverified.

crates.io publishing follow-up:
- Added `.github/workflows/publish.yml`, triggered by published GitHub releases. It verifies `v<package.version>`, runs the full native CI matrix through `workflow_call`, verifies the packaged crate, and publishes with the `CARGO_REGISTRY_TOKEN` repository secret.
- Restricted workflow permissions to repository read access, disabled persisted checkout credentials in publishing jobs, and serialized publish runs. Release tag text enters the version guard through an environment variable.
- Added package README/license inclusion and restricted Cargo publishing to `crates-io`. There is no configured Git remote from which to populate a repository URL.
- YAML parsing and version-guard checks passed. Online `cargo publish --locked --dry-run --allow-dirty --registry crates-io` passed for `mc-rs@0.1.0`, including compiling the packaged source. All 30 tests, formatting, YAML checks, and matching/mismatched/injected-tag guard checks passed. No crate was uploaded. The package still warns about missing repository/homepage/documentation metadata because no Git remote is configured.
- The registry reports `mc@0.1.0` already exists. The user chose `mc-rs`; renamed the package while explicitly preserving library and binary names `mc`. Included a package-local GPL license and added a release guard to keep it synchronized with the root license. The GitHub token and hosted execution still require repository setup.

### Backend decisions and documented limits

- `zip` 2.4.2 handles ZIP including AES/ZipCrypto and CRC checks; `sevenz-rust2` 0.22.2 handles 7z; `rars` 0.9.4 handles RAR. `compress-tools` 0.16.1/system libarchive handles tar/compressed tar; `flate2` handles standalone gzip. No archive CLI helpers are required. Decoder licenses are compatible with GPL-3.0-or-later; no UnRAR-restricted implementation is linked.
- Linux/macOS discover libarchive using pkg-config. Windows/MSVC uses vcpkg with `x64-windows-static-md`. The backend advertises these platform paths; only Linux runtime behavior is verified here.
- `trash` 5.2.8 supplies Freedesktop trash, macOS trash, and Windows Recycle Bin implementations. Linux trash behavior is exercised in an isolated test environment. No permanent-delete fallback exists.
- Editor fallback is `vi` on Unix and `notepad` on Windows; VISUAL/EDITOR arguments use word parsing, never shell evaluation.
- Filename search uses case-insensitive substrings. Selection uses case-sensitive `*`/`?` wildcards, selecting files by default. Quick search supports wildcard prefixes and next-match cycling.
- Archive VFS migration supersedes full extraction: metadata indexes plus bounded streams, password UI, and session credentials. Archive writing and multipart support remain excluded. Nested mounts use memory-only seek caches capped at 64 MiB per member, with eight levels; remote/nonlocal RAR uses a bounded 64 MiB owned input. Larger inputs must be copied locally. See `VFS.md` for bounds and remaining limitations.
- Copies preserve modification times and ordinary native permissions on local and SSH/SFTP destinations; Unix mode mapping requires a Unix client. FTP MFMT preserves regular-file times when advertised, not directory times/permissions. Archive adapters retain available stored timestamps/modes. Ownership, ACLs, extended attributes, special permission bits, sparse layout, and hard-link relationships remain unsupported. Windows symlink creation depends on OS privileges; replacing a symlink may fail safely.
- Cancellation is cooperative and does not roll back completed items. OS trash calls cannot be cancelled mid-call. Jobs are not persisted across application exit.
- Listing workers deliver bounded incremental batches. Idle local/remote panels refresh periodically; selection, jobs, and modal work pause automatic refresh. Superseded receivers are discarded. There is no filesystem watcher.
- The UI uses fixed supported MC shortcuts. Omitted upstream features (shell, internal viewer/editor, customization) have no substitute shortcut actions. This does not claim full upstream MC parity.

### Validation actually completed

- `cargo fmt --manifest-path mc/Cargo.toml --check`.
- `cargo clippy --manifest-path mc/Cargo.toml --offline --all-targets -- -D warnings`.
- `cargo test --manifest-path mc/Cargo.toml --offline`: 30 integration/contract tests passed, covering recursive operations, failed/skipped moves, cancellation during copy and conflict, symlinks (including dangling links), unsafe destination directories, all five archive formats, archive traversal/links, cancelled archives, stale listing responses, Unicode input, wildcard selection, path locks, and small/normal rendering.
- `python3 mc/tests/archive_smoke.py`: Linux archive/password PTY smoke passed as detailed above.
- `python3 mc/tests/terminal_smoke.py`: Linux PTY smoke passed for cat, copy, move, mkdir, default trash, explicit permanent deletion, search-result focus, mouse function-key activation, resize, terminal restoration, a missing external editor, and trash failure without permanent-delete fallback.
- Native debug and release builds succeeded for `x86_64-unknown-linux-gnu`. Packaged `dist/mc-0.1.0-x86_64-unknown-linux-gnu.tar.gz` with the binary, license, README, plan, and VFS architecture document.
- Linux ARM64, macOS ARM64, and Windows x64 have not been compiled or run in this session. Native CI and release artifacts for those platforms remain unverified.

## Next session

1. Read this plan and README; inspect current changes before editing.
2. Review the local transfer-hardening and proposals 3–7 changes and their validation entries below before committing/pushing. CI run 6 has verified the prior remote implementation on all four native build/test targets and published 0.2.6; the new checks still need hosted execution after pushing.
3. Complete M6 interactive runtime checks for Linux ARM64, macOS ARM64, and Windows x64. Build/test success and artifacts are verified; remote terminal behavior outside Linux x64 is not.
4. Exercise larger real-world directories and archives interactively; expand tests only for failures or unresolved concerns. Audit the supported shortcut subset against the manual as functionality expands.
5. Main proposals 3–7 are implemented; review the newest validation entry below. Next candidates are interactive remote checks on other native clients, system/full SSH config semantics and certificates, byte-level transfer resume, incremental archive indexing, and large archives beyond the bounded cache. Optional FTPS, remote editing and bookmarks have not been implemented.

Run locally from the repository root:

```sh
cargo run --manifest-path mc/Cargo.toml --release -- /left/directory /right/directory
```

Deferred backlog: FTPS, remote editing/bookmarks, complete/system SSH config and certificate support, byte-level transfer resume, remote/nested RAR beyond 64 MiB, and stronger FTP publication guarantees. Archive creation/modification only if requested later.

2026-09-08 — GitHub publication:
- User authorized publishing the project to https://github.com/uxsoft/mc-rs.git. The destination has no existing branch refs.
- Added the repository URL to Cargo metadata and README. Publishing the existing local `master` history, including VFS, menu, tests, and crates.io workflows.
- This push does not create a GitHub release or publish a crate. Configure `CARGO_REGISTRY_TOKEN` before releasing `v0.1.0`. Hosted CI results still need verification.

2026-09-08 — consolidated CI and push publishing:
- Supersedes the release-triggered publishing workflow above. A single `ci.yml` runs the native matrix for master pushes, PRs targeting master, and manual runs. Only a successful master push proceeds to its dependent crates.io publishing job.
- Removed `publish.yml` and `workflow_call`; no second matrix is dispatched for publishing.
- CI uses `major.minor.GITHUB_RUN_NUMBER`, stamping the same version in Cargo.toml and the mc-rs lock entry before testing/building/publishing. Binary name remains mc. Version changes stay in runner checkouts; no version commit is pushed back.
- Uploads use CARGO_REGISTRY_TOKEN and Cargo's built-in package verification. Reruns retain their version and cannot overwrite an already-published version. PR/manual runs never publish.
- Validation passed: five version-script tests (determinism, dependency preservation, invalid run numbers, mismatched lockfile, license drift), workflow YAML/trigger/dependency assertions, and online cargo publish dry run of a disposable mc-rs@0.1.42 package. No crate was uploaded during local verification.

2026-09-08 — macOS archive lock test correction:
- Reproduced the reported core-test failure on Linux by setting TMPDIR to a symlinked directory. The test compared a canonical job resource with a raw archive filename, unlike the application's comparison of two prepared lock sets.
- Updated the test to prepare deletion resources through `jobs::resources`, documented the `overlaps` input contract, and added Unix coverage for archive locks reached through aliased parent directories, ancestor deletion, and unrelated sibling paths.
- Runtime locking behavior is unchanged. Validation passed: all 31 integration/contract tests with a symlinked TMPDIR, formatting, strict Clippy, and git diff checks. Native macOS rerun remains for GitHub Actions.

2026-09-08 — CI caching and duplicate-work reduction:
- Added Swatinem/rust-cache for every native job, restoring after toolchain/native dependency setup but before run-number version stamping. Cache identity includes target, compiler, runner image, and native-library environment. Only successful master pushes save Rust caches.
- Publisher restores the Linux x64 dependency cache read-only and verifies the package for the same explicit target. It still waits for every native job and publishes only on master pushes.
- Windows caches vcpkg binary packages, keyed by image/architecture/vcpkg revision/triplet, saving after successful dependency installation on master. vcpkg still performs installation/ABI checks on every run.
- Formatting and Python version tests now run once on Linux x64. All platform-specific Clippy/tests/release builds remain. Linux PTY tests accept MC_TEST_BINARY and reuse the release binary instead of compiling a second host debug build.
- Added per-PR cancellation of superseded matrix jobs, retaining master publishing runs. Artifact compression is level 1 and retention is 14 days.
- Validation: workflow YAML and cache-before-stamping/target/publishing-gate assertions, all five version tests, release build, both PTY suites against the release binary, and diff checks passed. Cache restoration and timing gains require hosted cold/warm runs; no measured speedup is claimed.

2026-09-08 — update Actions runtimes:
- Checked upstream latest release pages and each tagged action.yml. Updated checkout to v7.0.1, setup-python to v7.0.0, cache restore/save to v6.1.0, upload-artifact to v7.0.1, and rust-cache to v2.9.2. All declare runs.using: node24. Kept dtolnay/rust-toolchain@stable, which uses composite shell steps.
- Existing inputs remain supported; setup-python's removed pip-install input is not used, checkout's fork restrictions concern triggers we do not use, and artifact archiving remains enabled by default.
- Preserved the pending CI cache optimizations, version stamping, native matrix, and master-only publishing gate. Workflow YAML/reference/input validation and diff checks passed; hosted execution still needs verification.
- Sources: https://github.com/actions/checkout/releases/tag/v7.0.1 ; https://github.com/actions/setup-python/releases/tag/v7.0.0 ; https://github.com/actions/cache/releases/tag/v6.1.0 ; https://github.com/actions/upload-artifact/releases/tag/v7.0.1 ; https://github.com/Swatinem/rust-cache/releases/tag/v2.9.2 .

2026-09-08 — Nerd Font file type icons:
- Added icons to both panels for parent navigation, directories, symlinks, archives, source languages, configuration, documents, media, fonts, databases, and binaries. Unknown/extensionless files use a generic file glyph; common special filenames have dedicated mappings. Matching is ASCII case-insensitive and directory/link metadata takes precedence over extensions.
- Classification uses existing VFS listing metadata, so archive entries share the same rendering without extra reads or extraction. Selection markers, highlighting, mouse row positions, and filenames are preserved. No dependency or configuration system was added.
- Documented the terminal Nerd Font requirement and Mono variant spacing recommendation in README.md. Verified all 27 glyph codepoints against the upstream Nerd Fonts catalog.
- Validation passed: formatting, strict Clippy, all 31 Rust integration/contract tests (including small/normal terminal rendering), both terminal and encrypted-archive PTY smoke suites, and diff checks. Actual glyph appearance depends on the user's terminal font.

2026-09-08 — move this crate to the 0.2 release series:
- Updated mc/Cargo.toml and its matching Cargo.lock package entry to 0.2.0. CI continues deriving the patch from GITHUB_RUN_NUMBER, now publishing 0.2.<run number>; executable remains mc. Updated the README example.
- Validation passed: all five version-script tests, stamping a disposable copy of the actual manifests to 0.2.42, locked offline Cargo metadata, and diff checks. No crate was published during this change.

2026-09-08 — simplify selection styling:
- Removed the selection dot and its reserved prefix space from panel rows. Selected entries use the existing gold color plus bold text, including when the cursor moves away. File type icons and cursor background highlighting remain.
- Validation: formatting, existing small/normal terminal rendering test, and diff checks passed.

2026-09-08 — FTP, SFTP, and SSH VFS:
- Implemented FTP with suppaftp and SFTP with ssh2/libssh2. Per the user's clarification, ssh:// uses a fixed embedded Python 3 helper over an SSH exec channel, supporting Unix servers without SFTP. User paths/data travel over framed stdin/stdout, never shell interpolation; the helper is not installed remotely.
- Added URL parsing, IPv6/custom ports, percent-encoded labels, password-free identities, masked authentication/retry, strict known_hosts verification, SSH agent/default key/password authentication, and session-owned streaming/seek handles. Plain FTP is explicitly identified as unencrypted. Unsupported credential-bearing URLs, control characters, and non-UTF-8 names fail clearly.
- Added Go menu connection actions, Alt+C URL navigation, local-directory return, and remote startup arguments for either panel. Connection and directory checks use cancellable workers. Remote lock preflight does no network I/O and conservatively locks the endpoint, including archive backing resources. Remote destination lookup distinguishes missing entries from transport/permission errors.
- Normal browse/search/selection sizing, cat, recursive background copy/move, mkdir, and permanent deletion dispatch through FileSystem. Remote trash fails with instructions to explicitly choose permanent deletion. SSH and SFTP reads are seekable; FTP uses REST and fresh transfers after seeks. Remote ZIP was exercised without local extraction; RAR remains local-only.
- Added destination staging and cleanup on failed/cancelled writes. SSH commits with atomic no-replace or explicit replacement. SFTP overwrite can fail safely when a server lacks replacement support. FTP has no conditional rename: conflict checks immediately precede RNTO but cannot prevent concurrent-client races. Old targets are never deleted to force a rename. Disconnects may leave staging files; mutations are not automatically retried. These limits are documented in README/VFS.md.
- Added loopback FTP and SSH integration fixtures, with separate SFTP and SSH-without-SFTP endpoints and isolated HOME/known_hosts. Tests cover seeking, retained handles, staged abort/commit, conflict preservation, recursive upload/move/delete, remote ZIP, and unknown/changed host-key rejection. Remote PTY flows cover startup, masked incorrect-password retry, selection/sizing, cat, copy, and clean exit; FTP PTY disables MLSD to exercise LIST fallback.
- Validation passed on Linux x64: 33 standard Rust tests plus the separate remote-server contract test, all three remote PTY flows, both existing local/archive PTY suites, strict Clippy, formatting, release build, package-content checks (including helper.py), Python syntax, workflow YAML, and diff checks. Native remote runtime verification for Linux ARM64/macOS/Windows remains pending.
- Linux x64 CI now runs the remote fixtures after installing test-only Python dependencies. Added explicit Linux OpenSSL development and macOS openssl@3 build setup/cache identity. Updated README build/connection instructions and VFS architecture. No commit or publication was performed for this change.


2026-09-09 — release verification and remote transfer hardening:
- Scope authorized by “go for it”: verify release/install, then harden remote transfers before expanding features.
- Verified GitHub Actions run 34316119517 (run number 6, commit cf1fb20d7ba4871711eb76d6148ba4767d73fd07): Linux x64/ARM64, macOS ARM64, Windows x64 native jobs and crates.io publishing succeeded. crates.io 0.2.6 package VCS metadata matches that commit. This supersedes earlier “native matrix unverified” notes for build/test/artifact coverage; interactive remote runtime coverage remains Linux x64 only.
- Installed the actual published crate into disposable mise data/config/cache/state directories. `MISE_CARGO_BINSTALL=false mise install cargo:mc-rs@0.2.6` succeeded and `mise exec cargo:mc-rs@0.2.6 -- mc --version` returned `mc 0.2.6`. Default binstall lookup stalled on unavailable GitHub release binaries/rate-limited requests; documented source installation. The user's global mise installation/configuration was not changed.
- Workers reject mismatched source byte counts for transfers involving remote providers. FTP/SFTP/SSH staged writers verify expected and server-stored lengths before publication. Existing targets and move sources survive incomplete transfers. Size checks do not detect same-size edits or corruption.
- Remote write handles expose credential-free staging locations for failure diagnostics. Lost publication replies explicitly report an uncertain outcome and require destination inspection before a manual retry; mutations are never automatically replayed.
- Added injected early EOF/connection-reset move coverage, FTP/SFTP server-side truncation, short-write rejection on all protocols, and an FTP rename that succeeds but loses its reply. Tests assert preservation, cleanup, and exactly one rename attempt.
- Added a disposable unprivileged OpenSSH daemon fixture with isolated known_hosts/client keys/shell configuration. Real OpenSSH 9.6p1 verified key authentication, SFTP overwrite refusal without deleting the old target, SSH atomic replacement, and short-upload cleanup. A local Fish startup utility consumed stdin; isolated test startup and documented that noninteractive SSH helper shells must not read stdin or write stdout. Diagnostics now explain this requirement; no user shell or system SSH configuration was changed.
- Linux x64 CI now installs openssh-server and runs the additional OpenSSH fixture after the existing remote fixtures. Validation passed: 34 standard Rust tests, both remote protocol contract tests (FTP/Paramiko and real OpenSSH), three remote PTY flows, local/archive terminal suites against the release binary, release build, formatting, strict Clippy, five version-script tests, Python syntax/workflow checks, and diff review. New CI steps still need hosted execution after pushing.
- Work remains local; no commit, push, or new crate publication has been performed in this session.


2026-09-09 — remaining proposals:
- User authorized the remaining five main proposals: SSH connections, job controls, metadata, large directories, and remote/nested archives. Optional FTPS, remote editing, and bookmarks are separate follow-ups.
- Implemented non-executing SSH Host/Include/HostName/User/Port/IdentityFile/IdentitiesOnly/StrictHostKeyChecking/ProxyJump configuration, owned bounded jump tunnels, custom/encrypted keys, masked keyboard-interactive challenges, and session-only unknown-host SHA256 confirmation. Changed keys remain rejected. StrictHostKeyChecking yes forbids unknown-host confirmation. Config Match/executable proxies/custom trust files and certificates are explicitly unsupported; system configuration is not read.
- Implemented selectable job controls, per-job cancellation, per-file progress/speed/ETA, and explicit failed-job retry with fresh SSH sessions and remaining top-level sources. Conflicts ask again; no mutation is retried automatically.
- Implemented local timestamps, SSH/SFTP timestamps and ordinary Unix permissions, plus FTP MFMT for regular files when advertised. The real FTP fixture rejects MFMT on directories; directory times and modes are explicitly unsupported there. ZIP/tar/7z/RAR adapters retain available timestamps/modes, with local-time interpretation and format precision for DOS timestamps. Incremental bounded panel batches use provider listing callbacks; directory sizing reuses listing metadata. Idle panels refresh periodically and sorting caches keys.
- Implemented memory-only bounded seeking for archive members, enabling nested mounts, and bounded remote/nonlocal RAR reads. Limit: 64 MiB per cached member/nonlocal RAR, eight nested mounts. No extracted browsing tree or plaintext temporary files.
- Acceptance tests pass for local and archive metadata, restart/conflict decisions, encrypted nested ZIP/RAR, cache bounds/cancellation, and listing batches arriving before enumeration finishes. Real OpenSSH verifies aliases/custom keys, encrypted-key passphrases, ProxyJump, and staged operations. Paramiko verifies keyboard-interactive MFA, session-only host trust/strict rejection, and protocol failure cases; combined public-key-plus-MFA authentication also passes.
- Terminal verification passes for local/archive workflows, all three remote workflows, and selected-job cancellation/retry with independent simultaneous jobs against a throttled SFTP server. Added jobs_terminal.py to the Linux x64 server fixture. Large-directory PTY passes with 20,000 entries: first observed filename 0.11 s, external-change refresh 4.33 s on this Linux x64 host outside filesystem mediation. These are single-run observations, not a cross-platform performance guarantee. The first stress assertion incorrectly searched for a complete filename in differential terminal output; corrected it to observe an entry, then request a full redraw for final count.
- Added large_directory.py to Linux native CI. README/VFS describe keys, trust rules, retry semantics, cache bounds, metadata precision, refresh intervals and unsupported options. Kept the user's separate README deletions from the prior turn.
- Final validation passed: 41 standard Rust tests; separate FTP/Paramiko and OpenSSH contract suites; local/archive, three remote, job-control, and 20,000-entry terminal suites; strict Clippy; formatting; release build; five CI version tests; Python syntax/workflow validation; package-content and diff checks. Tested OpenSSH aliases, plain/encrypted custom keys and ProxyJump, plus standalone and public-key-plus-MFA. New hosted/native-client runs are pending a push; changes remain uncommitted.

2026-09-09 — diagnose file icon font fallback:
- Investigated folders rendering correctly while some file icons resemble Chinese characters. On this host, Fontconfig resolves monospace U+F07B (folder) to CaskaydiaMono Nerd Font, but U+E7A8 (Rust) and U+E73C (Python) to AR PL UMing HK. The local Alacritty config has no explicit font family. This reproduces a matching fallback issue; the user's affected terminal is not yet confirmed.
- Verified that explicitly selecting the installed CaskaydiaMono Nerd Font Mono resolves all 27 application glyphs to that family in both regular and bold styles. The codepoints use the documented Nerd Fonts Font Awesome and Devicons ranges; retained the existing icons.
- Added README troubleshooting and an Alacritty font selection example; validated the proposed TOML against the existing config without modifying it. Actual terminal rendering still needs user verification. No application code or user terminal settings changed.
