# Virtual filesystem architecture

The application uses provider-dispatched filesystem operations for local files and archive members. SSH/SFTP/FTP are future providers, not special cases to add throughout panels and jobs.

## Midnight Commander source inspiration

Inspected upstream on 2026-09-08:

- [`lib/vfs/vfs.h`](https://github.com/MidnightCommander/mc/blob/master/lib/vfs/vfs.h): `vfs_class` dispatches directory, metadata, file-handle, and mutation operations.
- [`lib/vfs/path.h`](https://github.com/MidnightCommander/mc/blob/master/lib/vfs/path.h): `vfs_path_t` retains filesystem/path elements instead of treating every location as an OS path.
- [`lib/vfs/xdirentry.h`](https://github.com/MidnightCommander/mc/blob/master/lib/vfs/xdirentry.h): superblock/session ownership, cached inode/entry data, and open-file handles.
- [`lib/vfs/interface.c`](https://github.com/MidnightCommander/mc/blob/master/lib/vfs/interface.c): common operation dispatch.
- [`src/vfs/tar/tar.c`](https://github.com/MidnightCommander/mc/blob/master/src/vfs/tar/tar.c): cached archive entries and access to member contents without extracting a temporary browsing tree.

This is an independent Rust implementation of those architectural ideas, not a line-by-line C translation or a claim of MC VFS API compatibility. An archive provider owns its source `VfsPath`, so sessions compose naturally; Rust `Arc` handles replace manual mount lifetimes.

## Contract

`mc/src/vfs/mod.rs` defines:

- `FileSystem`: metadata, directory listings, read/seek handles, staged output handles, mutations, canonicalization, local-path access, backing resources, and capabilities. Default mutation methods fail read-only.
- `VfsPath`: provider/session identity plus a native `PathBuf` inside that provider. Equality, hashing, ordering, and overlap checks include provider identity. Labels are for display, never path identity or credential storage. Ordinary Unix filenames retain `OsString` bytes.
- `Metadata`, `Kind`, and `DirEntry`: common listing data. A directory listing does not open file payloads. Directory entry names must be single safe path components.
- `Read + Send` handles: own their underlying session or source; handles remain usable after navigation. Seeking is optional and explicitly requested by archive parsers.
- `WriteHandle`: writes into destination-owned staging and commits only after success. Dropping aborts an incomplete copy. Providers must enforce no-replace semantics and preserve existing data on failed overwrite/cancellation.
- `Context`: cooperative cancellation and an authentication request channel. Workers request credentials; the UI renders a masked modal and responds through a per-request channel.

Local operations live in `vfs/local.rs`. Panels, size scans, recursive search, and jobs use the common interface. Same-provider moves attempt rename before streamed copy/remove fallback. Job locks include canonical source/target locations and underlying archive resources. A provider reporting read-only cannot be a move/delete source or copy destination.

## Archive sessions

`archives.rs` is a read-only provider with an immutable path-to-entry map and parent-to-children index. Implicit directory entries are synthesized. Entries hold metadata and original decoder member identifiers, never temporary OS paths. The root's parent resolves to the source archive's containing VFS directory.

ZIP headers are read without decoding payloads; tar headers are scanned with skipped bodies; 7z/RAR parsers build header metadata. Encrypted headers request a password before listing. With plaintext headers, browsing works without a password and the first encrypted read prompts. Metadata-based selection sizing and filename search work inside archives.

Opening a member creates a decoder worker and a bounded two-chunk channel. Before sending bytes, a validation pass checks the requested content with the password, retrying authentication failures. A successful password is retained only by the archive session. The member is then decoded into the stream. F3 prepares the first bytes while the TUI can handle password requests, then sends the stream to external `cat` stdin. F5 consumes the same interface into a staged destination handle.

No extracted browsing tree or plaintext viewer temporary file is created. Explicitly copying out naturally writes plaintext at the chosen destination. The application's secret buffers use `zeroize`; decoder-internal copies, OS swap, and core dumps are not covered by that guarantee.

## Bounds and limitations

- The output queue is bounded, not the entire process: indexes and decoder dictionaries consume additional memory. Indexing rejects more than one million explicit entries. RAR5 buffered transforms are capped at 32 MiB; the cap excludes dictionaries and other decoder state.
- A selected member is decoded twice for validation and delivery. Solid 7z/RAR may need preceding members; gzip/tar.gz scans can be sequential. Standalone gzip is scanned for its true uncompressed size, since its trailer size wraps at 4 GiB.
- `rars` exposes file-backed and in-memory archive inputs, but no public generic reader transport. Its adapter currently requires `local_path`; do not emulate remote support by loading an entire remote archive into a `Vec`. Add a seekable-source decoder adapter before enabling remote RAR.
- Archive members expose sequential reads, not seekable files. Nested mounts therefore return an explicit seeking error. Future nested support needs a bounded seek cache or a decoder providing random access, with a clearly stated spill policy.
- Archive writing, multi-volume archives, and restoring links/special entries are excluded. Backend format variants may be unsupported. ZIP currently supports stored/deflate with AES/ZipCrypto.
- A source size/mtime check catches ordinary archive changes before reads. It is not an immutable source snapshot or protection against external same-size/same-time replacement.
- Cancellation is cooperative. Opaque decoder parsing/crypto and OS operations can delay cancellation; a dropped stream closes its producer channel and signals abandonment.
- The UI's current “go to” parser handles native absolute paths, relative paths within the active provider, and unchanged panel labels. It is not a remote URI parser. Network authentication, host verification, reconnect policy, latency handling, and URI parsing are still required for remote backends.

## Adding SSH/SFTP/FTP

Implement `FileSystem` for a connection/session. Keep credentials in that session or an authentication service, never the `id`, path, or label. Supply metadata and children without downloading files; read handles own connection state and stream content. Advertise only supported capabilities. Implement staged writes/commit with the server's guarantees and fail explicitly where atomic replacement or trash is unavailable.

Path canonicalization must use server semantics; don't apply local `std::fs` to remote paths. Distinct endpoints/sessions need stable, credential-free identities. Connection setup and metadata preflight must move off the UI thread where existing local fast-path assumptions would otherwise block. Panels, search traversal, copy streaming, conflict decisions, and directory sizing should need no backend-specific branches.

Independent in-memory-provider tests exercise panel listings, cross-provider copies, source lifetime, and archive mounting over a seekable nonlocal source. They are contract tests, not evidence that an SSH implementation already exists.
