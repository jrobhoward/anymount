# Changelog

Notable changes to `anymount`. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- macOS: a second NFS transport, an `AF_UNIX` socket with the root file handle
  handed to `mount(2)` directly. The MOUNT protocol never runs, nothing listens
  on the network, and no credential reaches the system mount table — access is
  decided by the socket's file permissions, mode 0600 inside a directory at
  mode 0700. It is tried first and falls back to loopback TCP, because it needs
  an argument buffer whose layout macOS does not document.
- `MountBuilder::nfs_require_local_socket` turns that fallback into a mount
  error, for content that must not be reachable by other local accounts.
  Rejected by name on the FUSE and cfapi backends, and in a build without the
  `nfs-local-socket` feature.
- `Mount::nfs_uses_local_socket` reports which transport a live mount got.
  False on every other platform.
- Two cargo features, `nfs-local-socket` and `nfs-tcp`, both on by default and
  both implying `nfs`. Either alone builds only that transport; `nfs` with
  neither is now a compile error rather than a backend that cannot mount. They
  trade privacy against how much of the mechanism macOS documents:
  `nfs-tcp` alone relies on nothing undocumented, `nfs-local-socket` alone
  keeps a mount private to the user that made it, and both on tries the second
  and falls back to the first. `README.md` has the comparison. Note that cargo
  features unify: a dependency graph where anything enables `nfs-tcp` gets the
  fallback back, so `nfs_require_local_socket` is the only way to guarantee its
  absence.
- Equality on the value types: `PartialEq`/`Eq` on `FileAttr` and `StatFs`,
  and `PartialEq`/`Eq`/`Hash` on `DirEntry`. An implementor's own tests assert
  on these types, which previously meant comparing field by field.
- `Hash`, `PartialOrd` and `Ord` on `FileKind`, so a listing can be grouped or
  sorted by kind; the order is declaration order, files before directories.
  `Hash` on `Backend`, so it can key a map.
- `From<u64>` for `Ino` and `FileHandle`, and `From<Ino>`/`From<FileHandle>`
  for `u64`. The public field already allowed this; the trait is what generic
  code reaches for.
- `# Errors` sections on every fallible public method, naming the `FsError`
  variants an implementor should return and a caller should expect. The
  `ReadOnlyFs` contract was previously only inferable from the backends.
- `#[must_use]` on `MountBuilder`'s methods and the `Mount` accessors. A
  builder chain whose result is dropped configures nothing, and used to
  compile without complaint.
- CI: a `cargo semver-checks` job guarding the frozen surface. It compares
  against the newest release on crates.io and reports that there is nothing to
  compare until the first one is published.
- `Cargo.lock` is committed. The MSRV and packaging jobs build `--locked`, so
  the declared floor is checked against the versions the repository pins rather
  than whatever resolved that morning. One job discards the lockfile and
  resolves fresh, which is what still catches a dependency publishing a release
  this crate cannot build against.

### Changed

- `FsError::context` now reports the error it wraps as
  [`Error::source`](https://doc.rust-lang.org/std/error/trait.Error.html#method.source).
  The chain used to stop at the explanation, so a caller using `anyhow` saw the
  context message and nothing underneath it. `Display` is unchanged.
- `FsError::Context` is `#[non_exhaustive]`, which makes `FsError::context` the
  only way to build one, as its documentation already said. Matching the
  variant now needs a `..` rest pattern.
- macOS: the value in the `MNT` export path and the value prefixing file
  handles are now drawn independently, where they used to be one secret. The
  export path reaches the system mount table and `nfsstat -m`; the handle
  secret no longer does. `MNT` also stops being answered once `mount_nfs` has
  exited, which is safe because `MNT` is issued exactly once per mount and
  reconnection does not repeat it. Together these mean the published value
  opens nothing by the time it can be read.
- macOS: the loopback mount now passes `ro`, matching the read-only scope the
  crate already had and the `MNT_RDONLY` the local-socket path sets.

### Fixed

- macOS: `Mount::unmount` no longer reports an error when the mount has already
  been taken down from outside the process — ejecting the volume in Finder, or
  running `umount`. `unmount(2)` answers `EINVAL` for a path that is no longer a
  mount point, which was propagated as a failure even though teardown had
  otherwise succeeded; the backend now checks whether the path is still a mount
  point and treats "already gone" as success, matching what the FUSE backend
  already did. A genuine failure, `EBUSY` from a file still open on the mount
  for instance, is still reported. `docs/GAPS.md` covers what an OS-initiated
  unmount does and does not do.

- macOS: `READDIR3` and `READDIRPLUS3` replies carried the first eight bytes of
  the file-handle secret as the `cookieverf3`, publishing half of it to anyone
  able to read a directory listing. The verifier is now a constant, which is
  all it needs to be: a mount's content is immutable for its lifetime.
- macOS: the file-handle secret comparison is now constant-time. It was
  previously `==`, and mattered little while the same secret was readable from
  the mount table; splitting the two values is what made it worth fixing.

### Documented

- `README.md` is reorganised around what a reader decides in order: what the
  crate mounts and what it will not do, then the trait, then the limits, then
  the feature flags. The detail it used to carry on the macOS NFS transports
  now lives in `docs/GAPS.md`. Screenshots of the `memfs` example mounted on
  each platform are in `docs/screenshots/`, kept out of the published package.
- `docs/GAPS.md` gains a section on extended attributes, which `README.md`
  listed as a limitation but which was catalogued nowhere: `listxattr` and
  `getxattr` are called only by the FUSE backend, so overriding them has no
  effect on macOS or Windows. It also gains one on the Linux backend needing
  `fusermount3` installed, the one install step any platform requires.
- `docs/GAPS.md` is grouped by area — scope, the trait contract, then one
  section per platform — instead of a flat list, and the cases for cfapi over
  ProjFS and for NFS over macFUSE have moved to `docs/ARCHITECTURE.md`, which
  is where the choice between backends belongs. What stays in `docs/GAPS.md`
  is the constraint a ProjFS backend would still face: its entry points have to
  be resolved dynamically, since `Client-ProjFS` is off by default and a static
  import would stop the binary starting on a machine without it.
- Corrected in `docs/GAPS.md`: there is no FUSE fallback on macOS, so the
  macFUSE note no longer implies one; the thread-count knob is
  `MountBuilder::threads`, not `fuser`'s internal `Config::n_threads`;
  `deny.toml` allows licences rather than named crates, so `onc-rpc` is not
  "on the allow-list"; and the cfapi read-pattern cross-reference pointed at a
  section of `docs/GAPS.md` that does not exist.
- `docs/ARCHITECTURE.md` gains the parts that make it an architecture doc
  rather than an annotated file listing: the request path, with a diagram of
  how a read reaches `ReadOnlyFs` by three different routes; the mount
  lifecycle as a sequence diagram; and which threads call the trait on each
  backend, with who owns the implementation. `lib.rs` and its `probe` module
  are in the module map now. The sections that restated `README.md`,
  `CHANGELOG.md` or `docs/GAPS.md` — design constraints, status, and the
  per-platform constraint list — are gone, replaced by one on the seams a new
  backend goes through. `README.md`'s backend section is a pointer to it
  rather than a second copy of the argument.
- Spelling is consistent across `README.md` and `docs/`: `-ize` verbs, `-our`
  nouns. `docs/ARCHITECTURE.md` previously had "materialise" and
  "materializes" in the same file.
- Corrected in `CLAUDE.md`: the NFS gating note predated the transport split
  and named the wrong files. `local.rs`, `tcp.rs` and `NfsHandle` are what is
  macOS-gated.
- `docs/NFS_ACCESS_CONTROL_OPTIONS.md` is removed. It was the design note
  behind the macOS access-control decision, and that decision shipped: the
  local socket with the root handle supplied directly, the two-secret split
  behind it, and `nfs_require_local_socket` to refuse the fallback. It had also
  been shipping to crates.io, unlike the other working documents. Two findings
  that lived only there have moved into `docs/GAPS.md` — that reconnection over
  the local socket is untested, and what was ruled out before settling for the
  handle secret: a reserved source port needs root, peer credentials report
  uid 0 because the kernel owns the client end, `RPCSEC_GSS` needs Kerberos, and
  the sysctl that would read mount arguments back is entitlement-gated.

### Added

- A test covering the NFS server across a dropped connection: the accept loop
  is driven over an `AF_UNIX` socket through more disconnect-and-redial cycles
  than `MAX_CONNECTIONS` allows, requiring every one to be answered. It pins
  the worker reaping as much as the reconnection — without it the seventeenth
  redial waits in the listen backlog until unmount.

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

### Documented

- The macOS NFS backend's per-mount secret is not confidential: `mount_nfs`
  publishes it in the system mount table, so a mount is readable by any local
  process rather than only by the user that created it. This follows from the
  mechanism, since the export path is the only channel for handing a credential
  to the OS's own client. `README.md`, the crate docs and `Backend::Nfs` now
  carry the caveat, `docs/GAPS.md` records it with the alternatives that do not
  apply and a two-secret design that would narrow it, and `SECURITY.md`'s scope
  no longer implies that holding the secret is difficult.

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
