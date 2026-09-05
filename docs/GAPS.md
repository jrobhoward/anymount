# Known gaps

What `anymount` does not do, why, and what it would take to change.
Kept honest so downstream users hit no surprises.

## Read-only

Write operations report `EROFS`. The first consumer serves immutable,
already-captured content, so nothing in v1 needs writes. [ProjFS cannot intercept
writes at all](https://github.com/microsoft/ProjFS-Managed-API/issues/30) —
one of the reasons it was cut rather than kept as a fallback (see below) — so
a cross-platform write story would have been uneven from the start anyway.

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

*To change:* the archive format must record plaintext chunk offsets. The trait
does not change when it does.

## Windows: a directory, not a drive letter

cfapi projects into a directory under a virtualisation root and cannot assign
`X:`.

*To change:* WinFsp is the only Windows option that mounts a real volume — and
it is GPLv3 with a paid commercial license. It would have to live in a separate,
opt-in `anymount-winfsp` crate so it never enters a default dependency graph.

## Windows: cfapi only — ProjFS was evaluated and cut, not deferred

`anymount` has exactly one Windows backend: cfapi. There is no `projfs`
feature, no `Backend::ProjFs`, and no ProjFS code in the tree.

This was a deliberate call, not an oversight or a placeholder for later.
ProjFS was checked for a capability
cfapi lacks and none was found for this crate's scope: ProjFS cannot intercept
writes at all (moot for a read-only crate anyway), both backends hydrate
through the same NTFS reparse-point/minifilter mechanism so `mmap` would not
have been differentiated, and both fetch callbacks
(`PrjGetFileDataCallback` / `CF_CALLBACK_TYPE_FETCH_DATA`) take an
offset/length, so neither would have been uniquely capable of ranged reads.
Meanwhile ProjFS carried two real costs cfapi doesn't:

- Reading a file through ProjFS materialises it on local disk with no
  automatic eviction; browsing a large archive can fill the volume. cfapi's
  `STREAMING_ALLOWED` policy avoids persisting fetched data, and
  `AUTO_DEHYDRATION_ALLOWED` lets Windows Storage Sense reclaim space.
- ProjFS needs a one-time admin step,
  `Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart`.
  cfapi needs none — `CldApi.dll` ships enabled on every Windows 10 1709+
  install, and `CfRegisterSyncRoot` works from an unpackaged binary.

*To change:* this would mean re-adding a Windows backend from scratch, not
restoring a stub — there is nothing here to un-comment. It would only be worth
doing given a concrete environment where cfapi genuinely fails. If that
happens, the constraint that made this hard the first time still applies:
`ProjectedFSLib.dll` only exists once `Client-ProjFS` is enabled — off by
default — so a load-time import of a `Prj*` symbol would prevent the *entire
binary* from starting rather than yielding a catchable error. Any new ProjFS
code must resolve entry points dynamically (`GetProcAddress` or
delay-loading), never link them statically.

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

## macOS: approving macFUSE's kernel extension requires lowering boot security (Apple Silicon)

Only applies to the FUSE fallback, not the decided NFS backend — the
README's macOS row lists no install burden for the default path precisely
because NFS avoids all of this. Relevant only when deliberately using the
`fuse`/macFUSE path instead.

On Apple Silicon, there is no click-through approval for a third-party kernel
extension in ordinary System Settings — no "Driver Extensions" row appears
under Login Items & Extensions, and nothing appears under Privacy & Security
either, even after `kernelmanagerd`/`syspolicyd` have logged an explicit
`Kernel Extension BLOCKED: ... not approved to load. Please approve using
System Settings` in response to a real mount attempt. The approval surface
only exists after the machine is rebooted into Recovery Mode, Startup Security
Utility is opened, and the boot security policy is lowered from "Full Security"
to "Reduced Security" with "Allow user management of kernel extensions from
identified developers" checked. That is a standing
change to the machine's boot-time security posture, not scoped to this crate
or reversible with a single click — worth deciding deliberately rather than
doing as a side effect of setting up a dev environment.

*To change:* nothing to change in the crate. Document the real cost plainly
(here, and to anyone setting up a macOS dev machine on the FUSE fallback)
rather than downplay it.

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
`127.0.0.1` only, which limits who could ever attempt this, but the posture
is unmeasured, not verified safe.

Still true at 1.0: this was reviewed before the 1.0 tag and left as is, on the
same loopback-only reasoning above, not overlooked.

*To change:* use a constant-time comparison (e.g. `subtle::ConstantTimeEq`) if
this ever needs a stronger guarantee than "loopback-only."

## Windows: the mountpoint must be empty, and unmounting clears it

cfapi projects placeholders into the mountpoint rather than covering it the way
a Unix mount does, so `mount()` requires an empty directory on Windows and
rejects a non-empty one, naming the backend. Unmounting removes the entries the
backend created, since nothing else reclaims them once the provider
disconnects.

The removal is narrow on purpose. Only entries still carrying
`FILE_ATTRIBUTE_REPARSE_POINT` are deleted; anything else is left in place and
logged. The precise cloud reparse tag is not checked, which would need
`GetFileInformationByHandleEx(FileAttributeTagInfo)` and a handle opened with
`FILE_FLAG_OPEN_REPARSE_POINT` — more FFI than the residual risk justifies once
an empty mountpoint is already a precondition.

The failure mode of being too cautious is a warning and a stray file, which
then fails the emptiness check at the next mount. The failure mode of being too
eager is deleting a file the backend did not create, so the check errs toward
leaving things alone.

*To change:* read the reparse tag and match it against the Cloud Files tags,
if a case ever appears where a placeholder loses its reparse point.

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
