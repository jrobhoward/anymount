# Changelog

Notable changes to `anymount`. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 1.0.0

First stable release. The public API is frozen: `ReadOnlyFs`, the value types
in it, `MountBuilder` and `Mount` will not change shape without a major
version. See `docs/GAPS.md` for what the crate does not do, and `src/types.rs`
for why none of the value types are `#[non_exhaustive]`.

### Fixed

- cfapi: directory enumeration passed dangling pointers to `CfExecute`. The
  `CF_PLACEHOLDER_CREATE_INFO` array borrowed names and file identities from a
  vector that was dropped before the call. The descriptors and the buffers
  they point into are now held together, and can only be reached through a
  closure that keeps the buffers borrowed for the call's duration.
- cfapi: unmounting deleted every entry in the mountpoint, whether or not this
  backend created it. Mounting over a directory that already held files
  destroyed them. The mountpoint must now be empty at mount time, which is
  also what makes it safe for unmount to remove everything it finds there —
  an intermediate attempt to narrow that further, by only removing entries
  still carrying `FILE_ATTRIBUTE_REPARSE_POINT`, turned out to leave
  fully-hydrated placeholder files behind indefinitely, since that attribute
  does not reliably survive to unmount. Read-only placeholder files also
  needed their `FILE_ATTRIBUTE_READONLY` cleared before removal, or deletion
  failed silently.
- NFS and cfapi: a `ReadOnlyFs::readdir` that returned a partial page had
  every entry past that page silently dropped. One `emit` call is one
  `dirlist3` or one `TRANSFER_PLACEHOLDERS`, so a short page was reported as a
  complete directory — `eof` set after the first page on NFS, and the rest of
  the entries never becoming placeholders on Windows. FUSE was unaffected in
  practice: its kernel client reissues `readdir` from the last cookie whether
  or not the reply claimed to be complete, so it re-drove the listing itself.
  `backend/readdir.rs`'s `emit` now pages until the implementation returns an
  empty result, on every backend.
- NFS: a `READ3` with an offset near `u64::MAX` overflowed while computing the
  `eof` flag — a panic in a debug build, a wrong flag in a release build.
  `FSSTAT3` had the same exposure multiplying implementor-supplied block
  counts. Both now saturate.
- NFS: a `READDIR3`/`READDIRPLUS3` whose budget could not hold even one entry
  was answered with an empty listing and `eof: false`, which invites a client
  to reissue the identical call forever. It now reports `NFS3ERR_TOOSMALL`.
- cfapi: a failed `CfDisconnectSyncRoot` abandoned the rest of teardown. The
  sync root stayed registered, which is machine-persistent state, so the
  mountpoint could never be registered again; and the handle was dropped
  anyway, freeing the context every still-armed callback holds a pointer into.
  Unmount now runs every step regardless and reports the first error at the
  end, matching what the NFS backend already did with its server thread.
- NFS: a `mount_nfs` that failed to spawn leaked the server thread and left
  its listener bound for the life of the process. Only the non-zero-exit path
  stopped and joined the thread; both paths now do.
- NFS: macOS showed the mount's 128-bit handle secret, as 32 hex characters,
  wherever it names the volume — the Finder window title, the sidebar, and
  every open and save dialog. macOS takes the volume name from the last
  component of the remote path, which was the secret itself. The export path
  is now `/export/<secret>/<label>`, where the label comes from
  `MountBuilder::fs_name`, so the name a caller already chose is the name that
  is displayed. Authorization is unchanged: the secret is still required, and
  the `MNT` handler still checks the segment before the first `/` and only
  that, so a label cannot be mistaken for it.
- All three backends trusted the byte count `ReadOnlyFs::read_at` returns. A
  count past the end of the buffer made `Vec::truncate` a no-op, so NFS
  reported a `count` larger than the data that followed it, which
  desynchronises the client; FUSE served the untouched tail of its allocation
  as file content; and cfapi panicked, which the callback boundary swallows,
  leaving the platform to wait out its fetch timeout with no completion. The
  count is now clamped to the buffer at each of the three call sites.

### Fixed (build)

- The crate did not build on any Unix other than Linux and macOS, despite
  `types.rs`, `error.rs` and the NFS wire layer all being gated on plain
  `cfg(unix)`. `libc` was declared only for the two mounting platforms, and
  `FsError::to_errno`'s `NoXattr` arm existed only for those two, leaving a
  non-exhaustive match everywhere else. `libc` now covers `cfg(unix)`, and
  `NoXattr` falls back to `ENOTSUP` off Linux and macOS. Verified against
  FreeBSD, NetBSD and illumos. Mounting is unchanged: still Linux, macOS and
  Windows only.

### Added

- `MountBuilder::threads`, setting the worker-thread count. FUSE only, and
  rejected at `mount()` time on the other backends rather than ignored.
- `impl From<FsError> for std::io::Error`, for callers bridging into
  `io::Result`. An `FsError::Io` is returned whole, keeping its kind and raw
  OS error.
- `impl Display for Ino` and `impl Display for FileHandle`, rendering the bare
  number.
- `FsError::to_errno` is now available on every platform, not only Unix, so
  the public API has the same shape on all targets.
- `Caps::empty_mountpoint` and `Caps::threads`, so the new requirements are
  declared by a backend rather than enforced by one.
- A `cfapi-mount-smoke-test` CI job that mounts, reads, checksums and unmounts
  on a Windows runner. The Windows backend previously had no runtime coverage
  anywhere.
- CI jobs for `cargo deny check advisories` (also on a weekly schedule) and
  `cargo publish --dry-run`; `cargo doc` now fails on a broken intra-doc link,
  and the MSRV job runs the tests rather than only type checking them.
- Compile-time guards on the public API's auto traits and derives
  (`tests/api_guard.rs`).

### Changed

- The NFS wire layer — XDR, RPC framing, and the MOUNT and NFS procedure
  tables — now compiles and tests on every Unix rather than only on macOS.
  Mounting is still macOS-only. Test count went from 41 to 117.
- `ReadOnlyFs::readdir` documents that it may return a partial page, and that
  an empty return is the only way to signal the end of a directory.
- `#![warn(missing_docs)]` is on, and the 33 public items that had no
  documentation now have it.
- `Cargo.toml` declares docs.rs metadata, so the published documentation
  covers all three platforms rather than only Linux. Its `description` named
  two platforms of three.
- The NFS server caps concurrent connections and reaps finished workers,
  rather than accumulating one thread per connection until unmount.
