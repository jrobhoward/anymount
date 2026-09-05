# Known gaps

What `anymount` does not do, why, and what it would take to change.

## Read-only

Write operations report `EROFS`. The first consumer serves immutable,
already-captured content, so nothing in v1 needs writes. ProjFS cannot
intercept writes at all, which was one reason it was cut (see below), so a
cross-platform write story would have been uneven regardless.

*To change:* add write ops to `ReadOnlyFs` (or a `ReadWriteFs` supertrait).
cfapi has a real callback path for writes; Windows write support beyond that
realistically means WinFsp, which reintroduces GPLv3 — see below.

## No symlinks or hardlinks

`FileKind` has only `File` and `Directory`. The first consumer skips symlinks
when capturing content, and cfapi does not model them the way FUSE does.

*To change:* add `FileKind::Symlink` plus a `readlink` op.

## Directory metadata is synthesised

An archive format generally stores files, not directories, so the tree is
rebuilt from path strings. Synthesised directories get default permissions
and no meaningful timestamps.

*To change:* nothing here — this belongs to the implementor, which knows what
its source recorded.

## No random-access streaming from chunked archives

`read_at` takes an offset, but a content-defined-chunked archive that does not
record *plaintext* chunk lengths cannot seek: it must decode from byte 0. The
recommended workaround is materialise-on-open — restore the whole file to a
cache directory on `open`, then serve reads from it.

This costs less than it sounds, because cfapi has no ranged-read path at all:
it fetches the entire file on first touch, unconditionally, regardless of
hydration policy, so it materialises whole files anyway. Only the FUSE path
issues random reads.

*To change:* the archive format must record plaintext chunk offsets. The
trait does not change when it does.

## Windows: a directory, not a drive letter

cfapi projects into a directory under a virtualisation root and cannot assign
`X:`.

*To change:* WinFsp is the only Windows option that mounts a real volume, and
it is GPLv3 with a paid commercial license. It would have to live in a
separate, opt-in `anymount-winfsp` crate so it never enters a default
dependency graph.

## Windows: cfapi only, not ProjFS

`anymount` has exactly one Windows backend: cfapi. There is no `projfs`
feature, no `Backend::ProjFs`, and no ProjFS code in the tree.

ProjFS was evaluated and found to offer nothing cfapi lacks for this crate's
scope: ProjFS cannot intercept writes at all (moot for a read-only crate),
both backends hydrate through the same NTFS reparse-point/minifilter
mechanism so `mmap` would not have been differentiated, and both fetch
callbacks (`PrjGetFileDataCallback` / `CF_CALLBACK_TYPE_FETCH_DATA`) take an
offset/length, so neither is uniquely capable of ranged reads. ProjFS carried
two costs cfapi doesn't:

- Reading a file through ProjFS materialises it on local disk with no
  automatic eviction; browsing a large archive can fill the volume. cfapi's
  `STREAMING_ALLOWED` policy avoids persisting fetched data, and
  `AUTO_DEHYDRATION_ALLOWED` lets Windows Storage Sense reclaim space.
- ProjFS needs a one-time admin step,
  `Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart`.
  cfapi needs none — `CldApi.dll` ships enabled on every Windows 10 1709+
  install, and `CfRegisterSyncRoot` works from an unpackaged binary.

*To change:* this would mean adding a Windows backend from scratch, not
restoring a stub. If `ProjectedFSLib.dll` is ever used, its entry points must
be resolved dynamically (`GetProcAddress` or delay-loading) rather than
linked statically — `Client-ProjFS` is off by default, so a static import
would prevent the whole binary from starting on a machine that hasn't enabled
it.

## macOS: no FSKit backend

FSKit (macOS 15.4+) is Apple's kernel-extension-free framework for user-space
filesystems, and would in principle be a kext-free alternative to NFS. It is
not usable as of this writing: third-party FSKit modules — including
macFUSE's own signed FSKit backend — fail to authorize on current macOS
builds (`fskitd` cannot resolve a Developer Team ID for the module; tracked
upstream at
[`andrewgazelka/loaf#1`](https://github.com/andrewgazelka/loaf/issues/1)).
Moot for `anymount` either way, since NFS is the shipped macOS backend and
does not depend on FSKit — see `docs/ARCHITECTURE.md` for why NFS was chosen
over FUSE/macFUSE.

*To change:* retest once Apple ships a `fskitd` fix. Only relevant if NFS
ever needs a fallback.

## macOS: FUSE needs a kernel extension, which Apple Silicon makes hard to approve

Only applies to the FUSE fallback, not the shipped NFS backend. FUSE on
macOS goes through macFUSE, which needs a third-party kernel extension. On
Apple Silicon there is no click-through approval for that in System
Settings; approving it means booting into Recovery Mode and lowering the
machine's boot security policy — a standing change, not a one-time click.

This is why the shipped macOS backend is NFS, not FUSE: using this crate
should not require a security posture change, a reboot, or code signing as a
prerequisite.

*To change:* nothing to change in the crate. This is inherent to third-party
kernel extensions on Apple Silicon.

## No async

`ReadOnlyFs` is synchronous. The first consumer's storage trait is synchronous
top to bottom, so an async trait could not be fed by it. Concurrency comes from
serving FUSE requests on multiple threads (`Config::n_threads`).

*To change:* add an `async` feature with a parallel trait and a bridging
adapter. Worth doing only when an async consumer exists.

## No `statfs` numbers by default

The default `statfs` reports zeroed counters, so `df` shows an empty
filesystem. Implementors that know their archive size should override it.

## NFS: no per-inode handle cache — `READ3` pays an open/release round trip

The NFS backend (`backend/nfs/`) calls `fs.open`/`read_at`/`release` on every
`READ3` RPC rather than caching a handle per `Ino` across calls. This is
correctness-complete — including for the sequential read bursts a real client
issues — but pays one extra open/release round trip per read RPC. An
idle-evicting handle cache keyed by `Ino` would remove that cost.

*To change:* add a `Mutex<HashMap<Ino, (FileHandle, Instant)>>` (or similar) to
`backend/nfs/mod.rs`'s per-mount state, with an eviction sweep on read
staleness. Worth doing only once a real workload shows the round trip
mattering; the first consumer does not need it yet.

## cfapi: no per-inode handle cache — `FETCH_DATA` pays an open/release round trip

`backend/cfapi.rs`'s `stream_fetch` calls `fs.open`/`read_at`/`release` once per
`FETCH_DATA` callback rather than caching a handle per `Ino`. Since cfapi always
fetches a whole file in one callback (see this file's "Read pattern" docs),
this costs one open/release pair per file rather than per read the way the NFS
gap above does — a smaller version of the same tradeoff.

*To change:* same shape as the NFS gap above, if a real workload shows it
mattering.

## NFS: hand-rolled RPC framing, not the `onc-rpc` crate

`backend/nfs/rpc.rs` hand-rolls ONC RPC (RFC 5531) record marking and
call/reply headers rather than using the `onc-rpc` crate
(`domodwyer/onc-rpc`, BSD-3-Clause, already on `deny.toml`'s allow-list).
`onc-rpc` only covers that envelope layer, not the MOUNT/NFSv3 payload XDR
(`fattr3`, `dirlistplus3`, and the rest), which needs hand-rolling regardless
— so a partial dependency saves little. `onc-rpc` remains the option if the
hand-rolled envelope ever needs replacing.

## NFS: at most 16 concurrent connections

`backend/nfs/server.rs` serves at most `MAX_CONNECTIONS` connections at once,
reaping finished workers each time round the accept loop. The listener binds to
loopback and only this mount's own `mount_nfs` client has business connecting,
so the cap exists to stop a local process opening sockets in a loop from
costing one thread per connection until unmount. A legitimate client that hits
the cap waits in the kernel's listen backlog until a slot frees.

*To change:* raise the constant, or move to a thread pool with a work queue, if
a client is ever found that needs more than a handful of connections.

## NFS: single-fragment RPC messages only

`backend/nfs/rpc.rs`'s `read_message` closes the connection on a multi-fragment
ONC RPC message rather than reassembling one from several TCP fragments. Every
request `mount_nfs` sends in practice fits in one fragment; reassembly would
only matter for an NFS client this crate has not been exercised against.

*To change:* buffer fragments keyed by connection until the last-fragment bit
is set, then dispatch the reassembled body.

## NFS: file handle secret comparison is not constant-time

`FileHandle3::resolve` (`backend/nfs/handle.rs`) compares the client-supplied
16-byte secret with `==`, which is not constant-time. The timing side channel
this could in principle leak has not been measured. A mount is bound to
`127.0.0.1` only, which limits who could attempt this.

*To change:* use a constant-time comparison (e.g. `subtle::ConstantTimeEq`) if
this ever needs a stronger guarantee than "loopback-only."

## Windows: the mountpoint must be empty, and unmounting clears it

cfapi projects placeholders into the mountpoint rather than covering it the way
a Unix mount does, so `mount()` requires an empty directory on Windows and
rejects a non-empty one, naming the backend. Unmounting removes everything
found in the mountpoint, since nothing else reclaims it once the provider
disconnects, and nothing else has legitimate reason to have written there
during the mount.

An earlier version of this check tried to be narrower: delete only entries
still carrying `FILE_ATTRIBUTE_REPARSE_POINT`, leaving anything else in place
and logged. That assumed a placeholder always keeps that attribute, which is
false — a fully-hydrated placeholder file can lose it once the sync root
disconnects, and the attribute-based check then left it behind indefinitely.
The empty-mountpoint precondition at mount time was always the actual
safety guarantee, so removal no longer depends on the attribute surviving.

*To change:* nothing outstanding here.

## `readdir` may page, and an empty page is the only end-of-directory signal

`ReadOnlyFs::readdir` is allowed to return fewer than all the remaining
entries; `backend/readdir.rs`'s `emit` calls it again at a higher offset until
it returns an empty page. That means a short page cannot be used to mean "this
is the end" — only an empty one can. An implementation that returned a partial
page as its final answer would have the directory reported as complete at that
point.

The alternative — requiring every call to return the whole remaining tail —
was what the crate assumed before 1.0. It made FUSE quadratic on large
directories, since FUSE asks for a few kilobytes at a time and each call
rebuilt the whole remainder, and it silently truncated any paging
implementation on NFS and cfapi, where one listing pass is one reply.

*To change:* nothing. Both shapes work; an implementation that always returns
everything remaining is still correct, because its next call returns an empty
page.

## `allow_other`, `auto_unmount` and `threads` are FUSE-only

`allow_other` and `auto_unmount` are FUSE mount options. The NFS backend has no
counterpart — the server binds to loopback and authorizes with the file-handle
secret rather than by uid — and neither does cfapi, where a sync root belongs to
the user running the process.

`MountBuilder::threads` is FUSE-only for a different reason: FUSE is the only
backend that owns a worker pool. The NFS server sizes itself from the
connections its client opens, and cfapi's callbacks are dispatched by the
platform.

`backend/preflight.rs` rejects a request for any of the three on a backend whose
`Caps` does not claim it, naming the backend, rather than accepting the call and
quietly doing nothing.

*To change:* nothing to change for `auto_unmount` — dropping the `Mount`
unmounts on every backend, which is what it was for. Exposing an NFS mount to
other users would mean real per-user authorization in place of the handle
secret — see `docs/ARCHITECTURE.md`'s platform constraints for why the
current secret-based scheme was chosen over `AUTH_SYS`.

## `..` reports the directory's own inode when `lookup(dir, "..")` is unanswered

`backend/readdir.rs`'s `emit` resolves `..`'s inode with `lookup(dir, "..")` and
falls back to the directory itself when that fails. `ReadOnlyFs`'s docs ask
implementors to answer `.` and `..`, but treat it as a should: an implementation
that does not still gets a usable listing, with a cosmetically wrong `..`
fileid, rather than a failed `readdir`. Under FUSE the kernel resolves `..` from
its own dentry cache regardless, so only tools reading `d_ino` straight out of
`getdents` — `find -inum`, say — can observe it.

*To change:* answer `lookup(dir, "..")`. `examples/memfs.rs` shows the shape.

## `tracing` covers lifecycle and discarded errors, not every operation

The `tracing` feature wires `backend/trace.rs` into the places an error would
otherwise be dropped silently — a failed `ReadOnlyFs::release`, a failed unmount
during `drop` — and to mount and unmount themselves. It is not the
per-operation span coverage a profiler would want.

*To change:* instrument each adapter callback in `backend/fuse.rs` and each
procedure in `backend/nfs/nfs_proto.rs`. Worth doing when someone is actually
debugging a latency problem through the trait.
