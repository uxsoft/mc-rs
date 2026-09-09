# Virtual filesystem architecture

The application uses provider-dispatched filesystem operations for local files, archive members, FTP, SFTP, and SSH servers. Remote providers use the same panel, search, sizing, streaming, and background-job paths as local files.

## Midnight Commander source inspiration

Inspected upstream on 2026-09-08:

- [`lib/vfs/vfs.h`](https://github.com/MidnightCommander/mc/blob/master/lib/vfs/vfs.h): `vfs_class` dispatches directory, metadata, file-handle, and mutation operations.
- [`lib/vfs/path.h`](https://github.com/MidnightCommander/mc/blob/master/lib/vfs/path.h): `vfs_path_t` retains filesystem/path elements instead of treating every location as an OS path.
- [`lib/vfs/xdirentry.h`](https://github.com/MidnightCommander/mc/blob/master/lib/vfs/xdirentry.h): superblock/session ownership, cached inode/entry data, and open-file handles.
- [`lib/vfs/interface.c`](https://github.com/MidnightCommander/mc/blob/master/lib/vfs/interface.c): common operation dispatch.
- [`src/vfs/tar/tar.c`](https://github.com/MidnightCommander/mc/blob/master/src/vfs/tar/tar.c): cached archive entries and access to member contents without extracting a temporary browsing tree.
- [`src/vfs/sftpfs/sftpfs.c`](https://github.com/MidnightCommander/mc/blob/master/src/vfs/sftpfs/sftpfs.c): separate libssh2-backed metadata, directory, and owned file-handle callbacks. The Rust remote providers retain this separation under the common FileSystem contract.

This is an independent Rust implementation of those architectural ideas, not a line-by-line C translation or a claim of MC VFS API compatibility. An archive provider owns its source `VfsPath`, so sessions compose naturally; Rust `Arc` handles replace manual mount lifetimes.

## Contract

`mc/src/vfs/mod.rs` defines:

- `FileSystem`: metadata, directory listings, read/seek handles, staged output handles, mutations, canonicalization, local-path access, backing resources, and capabilities. Default mutation methods fail read-only.
- `VfsPath`: provider/session identity plus a native `PathBuf` inside that provider. Equality, hashing, ordering, and overlap checks include provider identity. Labels are for display, never path identity or credential storage. Ordinary Unix filenames retain `OsString` bytes.
- `Metadata`, `Kind`, and `DirEntry`: common listing data. A directory listing does not open file payloads. Directory entry names must be single safe path components.
- `Read + Send` handles: own their underlying session or source; handles remain usable after navigation. Seeking is optional and explicitly requested by archive parsers.
- `WriteHandle`: writes into destination-owned staging and commits only after success. Dropping aborts an incomplete copy. Local and SSH-helper writes enforce atomic no-replace publication; SFTP uses protocol rename semantics. FTP has no conditional rename and has a documented concurrent-client race at publication. No provider deletes an old destination to make replacement succeed. Network failures can prevent remote staging cleanup.
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
- `rars` exposes local-file and owned-memory inputs, but no generic reader. The authorized bounded-cache extension uses owned memory for nonlocal RAR only after checking a 64 MiB cap, with cancellation and byte-count checks while reading. Larger RAR files must be copied locally. This replaces the earlier deferral of all nonlocal RAR; decoding/indexing may reread the bounded input. Library-owned input buffers and dictionaries are additional memory.
- Archive members expose `open_seek` through `vfs::cache::SeekCache`: lazily fill a memory-only buffer, reuse bytes on backward seeks, reject members larger than 64 MiB, check cancellation, and zeroize cached plaintext on drop. No spill files. Nested mounts retain their parent sessions/backing locks and stop at eight levels. Seek-to-end uses declared size; subsequent reads may decode the entire member.
- Archive writing, multi-volume archives, and restoring links/special entries are excluded. Backend format variants may be unsupported. ZIP currently supports stored/deflate with AES/ZipCrypto.
- A source size/mtime check catches ordinary archive changes before reads. It is not an immutable source snapshot or protection against external same-size/same-time replacement.
- Cancellation is cooperative. Opaque decoder parsing/crypto and OS operations can delay cancellation; a dropped stream closes its producer channel and signals abandonment.
- Remote RAR uses the bounded owned-memory adapter above. FTP random access requires REST and starts a fresh data transfer after seeks; indexing a remote archive can be network-intensive.

## Remote providers

`vfs/remote/mod.rs` parses FTP/SFTP/SSH URLs into a credential-free endpoint and provider path. Password-bearing URLs, queries/fragments, control characters, and non-UTF-8 names are rejected. Label components are percent-encoded. SFTP and SSH share a conservative user/host/port lock identity; FTP has its own identity. `FileSystem::lock_path` never performs network I/O for a remote path, and remote lock paths cover the entire endpoint. Archive backing resources participate in these locks. SSH config aliases resolving to the same user/hostname/port share an identity; unrelated DNS aliases are not automatically unified.

Connection setup and directory validation run on a cancellable worker, using the same masked authentication channel as archives. Existing panel mounts are reused when editing their displayed URLs. Network metadata, canonicalization, and destination checks run in background workers; remote permission/transport failures are not treated as missing destination files. Startup accepts remote URLs for either panel. Go menu connection actions and Alt+C use the same resolver. Absolute paths inside a remote panel remain on that server; `file://` and Go → Local directory leave it.

`ssh.rs` uses ssh2/libssh2 with known_hosts verification before authentication. Unknown hosts require explicit session-only SHA256 fingerprint confirmation; StrictHostKeyChecking yes instead rejects them. Changed keys fail. Authentication supports the agent, custom/default keys, key passphrases, password, and keyboard-interactive/MFA through the existing masked prompt channel. `config.rs` reads a non-executing subset of ~/.ssh/config with first-value scalar precedence, accumulated IdentityFile entries, Include and Host patterns, explicit URL overrides, and ProxyJump. Unsupported routing/trust directives fail explicitly; see README for supported directives and limits. Jump sessions own nonblocking direct-tcpip bridges with bounded buffers, retained by the child session's socket; closing the child closes its bridge. No SSH command or shell-based proxy is executed.

- SFTP uses stat/lstat, streamed directory enumeration, seekable file handles, mkdir/unlink/rmdir, readlink, and no-replace rename. Writes use exclusive temporary files followed by a rename request. An unsupported overwrite fails safely instead of deleting the destination. Remote symlink creation is not advertised.
- SSH without SFTP uses the embedded `helper.py` through a fixed `python3 -u -c` command on a Unix server. User paths never enter shell syntax: JSON requests and bounded binary chunks travel on channel stdin/stdout. Read handles keep a remote descriptor open for seek/read. Uploads stage beside the destination, then publish with os.link for no-replace or os.replace after explicit overwrite approval. EOF/cancellation removes incomplete staging while the connection remains functional. No helper file is installed remotely. Python 3 and permission to execute it are required.
- FTP uses suppaftp with passive data connections pinned to the control peer address, binary transfers, bounded MLSD listings (LIST fallback), and seekable reads using REST. Read/write handles own control/data connections; metadata operations use fresh connections with the session's zeroized password. Uploads reserve a staging directory with MKD and send content there, then recheck the destination before RNFR/RNTO. FTP cannot provide an atomic no-replace condition against other clients. It never deletes an existing target to force RNTO success. No FTPS, remote trash, or link creation is advertised.

Streams transfer at most 64 KiB per application chunk. FTP listings and SSH helper responses are capped at 32 MiB; directory listings are capped at 100,000 entries. Cancellation is checked between operations/chunks, with ten-second TCP/SSH operation timeouts; OS DNS resolution is not cancellable. Lost connections may leave staging files, and completed mutations are not automatically replayed. Panels can reconnect by leaving the mount and opening a new connection. Explicit job retry opens fresh SSH sessions in its worker and restarts remaining top-level sources, with fresh conflict decisions; partial byte streams are not resumed.

For transfers involving a remote provider, the worker compares bytes read with source metadata before committing. Remote writers independently verify the expected length and staged file size (FTP listing, SFTP fstat, SSH helper fstat) before publication. This detects length mismatches, not same-size concurrent edits or content corruption. `WriteHandle::staging_location` exposes a credential-free recovery location; job errors report it after attempted cleanup. A lost publication reply is an uncertain outcome: retain the move source, report that the destination may already exist, and never automatically replay the mutation. OpenSSH SFTP v3 rejects replacing an existing target through the currently used rename API; use `ssh://` for atomic replacement.

SSH helper startup requires a noninteractive login shell that does not consume stdin or write stdout. Startup banners/tools need an interactive guard. The helper reports protocol/timeout errors with this requirement; it cannot prevent shell startup from consuming input before Python starts.

`tests/remote_servers.py` runs real loopback FTP and Paramiko SSH servers using only disposable files and a temporary HOME/known_hosts. The SSH-helper endpoint deliberately has no SFTP subsystem. `tests/remote.rs` exercises streaming, seeking, staged commit/abort, overwrite refusal, recursive jobs, remote ZIP mounting, retained handles, and host-key rejection. `remote_terminal.py` covers startup URLs, masked password retry, selection/sizing, viewing, copying, and clean exit; its FTP phase disables MLSD to exercise LIST fallback. Linux x64 CI runs this suite; native remote operation on other client platforms still needs validation.

Fault injection covers truncated server uploads and an FTP rename whose success reply is lost, checking destination/source preservation and absence of automatic retry. `tests/transfer_failure.rs` exercises failed cross-provider moves with early EOF and a connection reset. `tests/openssh_server.py` starts an unprivileged loopback OpenSSH daemon with disposable host/client keys and shell configuration; it verifies key authentication, SFTP replacement refusal, SSH helper replacement, short-upload rejection, and staging cleanup. Set `MC_TEST_SSHD` to use a standalone sshd binary. No system SSH service or user SSH files are changed. Linux x64 CI runs both server suites.


## Listings, jobs, and metadata

`FileSystem::visit_dir` delivers metadata as entries arrive, with a compatibility fallback to read_dir. Local, FTP, SFTP, and SSH-helper providers stream enumeration; panel workers send batches of 256 over a two-batch channel. The UI consumes at most eight batches per tick, sorts with cached keys, and discards superseded receivers. Directory sizing reuses listing metadata rather than issuing another stat for every file. Symlink directory classification still needs a following stat. Idle refresh intervals are 3 seconds locally and 15 seconds remotely; jobs, modal work, and selection suspend automatic refresh. This uses periodic listings, not filesystem watchers.

Jobs retain operation/source/destination specifications and a completed-top-level-source count. Per-job cancellation affects only the selected job. Explicit retry checks resource conflicts before starting and reconnects inside the worker; it never inherits overwrite-all decisions. Speed is an average over elapsed job time, including waits; progress and ETA refer to the current file, not a precomputed recursive total. Completed work remains in place on cancellation.

Local staged files set mtime before publication and directories after children. SSH/SFTP set mtime and ordinary Unix mode bits on staged files before publication, and directory metadata after children; a metadata error aborts a staged publication. FTP FEAT gates MFMT for regular files; directory times and permissions are unsupported. Windows clients do not map Unix permission modes. Archive adapters retain available ZIP/tar/7z/RAR timestamps and Unix permission modes; ZIP/RAR DOS times are interpreted in the client local timezone with their original precision. Missing archive times remain the epoch; standalone gzip uses header mtime or its source timestamp. Ownership, ACLs, extended attributes, special mode bits and symlink timestamps are outside this metadata contract.

SSH configuration and MFA behavior were checked against [OpenSSH ssh_config](https://man.openbsd.org/ssh_config) and [ssh2 keyboard-interactive callbacks](https://docs.rs/ssh2/0.9.6/ssh2/trait.KeyboardInteractivePrompt.html).

`tests/proposals.rs` covers metadata, restart/conflict semantics, encrypted nested ZIP/RAR, seek bounds/cancellation, and delivery before a listing completes. `jobs_terminal.py` exercises selected-job cancellation, concurrent independent jobs, speed/ETA, and explicit retry/re-authentication against a throttled SFTP server. `large_directory.py` checks 20,000 local entries, keyboard response, idle automatic refresh and clean shutdown. The OpenSSH fixture tests both direct aliases/custom keys and ProxyJump; Paramiko tests MFA and session-only host confirmation, including strict rejection.
