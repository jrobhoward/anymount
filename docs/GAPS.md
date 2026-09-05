# Known gaps

What `anymount` does not do, why, and what it would take to change.
Kept honest so downstream users hit no surprises.

## Read-only

Write operations report `EROFS`. The first consumer restores from immutable
backup snapshots, so nothing in v1 needs writes. [ProjFS cannot intercept
writes at all](https://github.com/microsoft/ProjFS-Managed-API/issues/30) —
one of the reasons it was cut rather than kept as a fallback (see below) — so
a cross-platform write story would have been uneven from the start anyway.

*To change:* add write ops to `ReadOnlyFs` (or a `ReadWriteFs` supertrait).
cfapi has a real callback path for writes; Windows write support beyond that
realistically means WinFsp, which reintroduces GPLv3 — see below.

## No symlinks or hardlinks

`FileKind` has only `File` and `Directory`. ciphercask skips symlinks at
backup time, and cfapi does not model them the way FUSE does.

*To change:* add `FileKind::Symlink` plus a `readlink` op.

## Directory metadata is synthesised

Backup archives generally store files, not directories, so the tree is rebuilt
from path strings. Synthesised directories get default permissions and no
meaningful timestamps.

*To change:* nothing here — this belongs to the implementor, which knows what
its archive recorded.

## No random-access streaming from chunked archives

`read_at` takes an offset, but a content-defined-chunked archive that does not
record *plaintext* chunk lengths cannot seek: it must decode from byte 0. The
recommended workaround is materialise-on-open — restore the whole file to a
cache directory on `open`, then serve reads from it.

This costs less than it sounds, because **cfapi has no ranged-read path at
all: it fetches the entire file on first touch, unconditionally** — it
materialises whole files anyway. Confirmed empirically (`docs/PLAN.md` Phase
2, item 2): seeking straight to a 4 MiB offset in a never-touched placeholder
and reading 4 KiB, without ever reading byte 0, still produced a single
`CF_CALLBACK_TYPE_FETCH_DATA` call for the whole file — reproduced across
`CF_HYDRATION_POLICY_PARTIAL`, `CF_HYDRATION_POLICY_PROGRESSIVE`, and
unbuffered sector-aligned I/O, so neither hydration policy nor the NTFS cache
manager's own read-ahead explains it. Only the FUSE path issues random reads.

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

This was a deliberate call made in the Phase 0 spike (`docs/PLAN.md`), not an
oversight or a placeholder for later. ProjFS was checked for a capability
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
  install, confirmed via `CfGetPlatformInfo` in the Phase 0 spike, and
  `CfRegisterSyncRoot` was confirmed to register a sync root from an
  unpackaged binary.

*To change:* this would mean re-adding a Windows backend from scratch, not
restoring a stub — there is nothing here to un-comment. It would only be worth
doing given a concrete environment where cfapi genuinely fails. If that
happens, the constraint that made this hard the first time still applies:
`ProjectedFSLib.dll` only exists once `Client-ProjFS` is enabled — off by
default — so a load-time import of a `Prj*` symbol would prevent the *entire
binary* from starting rather than yielding a catchable error. Any new ProjFS
code must resolve entry points dynamically (`GetProcAddress` or
delay-loading), never link them statically.

## macOS: no FSKit backend — third-party FSKit modules do not load on this OS build

**Moot for macOS as actually shipped: the decided macOS backend is NFS, not
FUSE, so none of this section applies to normal use.** Kept in full below as
background for why FUSE (and its FSKit alternatives) were set aside — and
because `fuse`/macFUSE remains an available fallback if NFS ever turns out to
have a blocking problem of its own. See `docs/PLAN.md`'s "Revised decision"
section for the full history. Skip to the next section unless debugging the
FUSE fallback specifically.

FSKit (macOS 15.4+) is Apple's kernel-extension-free framework for user-space
filesystems, and would in principle be the modern kext-free path on macOS —
either through macFUSE's own FSKit backend, or a filesystem written directly
against FSKit. Neither is usable today. This is a platform-level finding, not
specific to `fuser` or to this crate.

**`fuser` cannot reach macFUSE's FSKit backend.** `fuser` 0.18 (the FUSE
binding this crate uses) hardwires macOS mounting to `fuse_mount_compat25`, a
legacy libfuse2-compatible C entry point (`fuser-0.18.0/build.rs`,
unconditional on `target_os = "macos"`, never probes `fuse3`). Passing
`MountOption::CUSTOM("backend=fskit")` through that entry point fails
immediately with no FSKit or XPC activity in the system log.

Tracing macFUSE's own public source (`github.com/macfuse/library`,
`github.com/macfuse/mount`, `github.com/macfuse/framework`) explains why: the
open-source C library (what `fuse.pc` resolves to, and what `fuser` links
against) has no knowledge of `backend=fskit` at all. At mount time
(`mount_darwin.c`) it always `execv()`s an external helper, `mount_macfuse`,
passing the `-o` string through verbatim — the *helper* decides what to do
with `backend=fskit`, using Swift bridge classes (`Mount/Mounter.swift`) that
build an XPC request naming `"backend": "fskit"`. `mount_macfuse` itself is
not in any of macFUSE's public repositories — it ships only as a prebuilt,
signed binary inside the `.pkg`. So there is no way to reach the FSKit path by
building macFUSE from source or by writing custom FFI against the public C
API; the routing logic is closed-source either way.

**Even macFUSE's official, signed FSKit module fails to authorize on this
machine (macOS 26.6.2, build 25G83).** After installing macFUSE 5.3.3,
launching `macfuse.app` once to register its FSKit app extensions
(`pluginkit -m` then lists `io.macfuse.app.fsmodule.macfuse` and `-local`),
System Settings → General → Login Items & Extensions → Extensions → macFUSE →
FSKit Modules shows both modules but the toggle to enable either is disabled —
it cannot even be clicked. `log stream` during the attempt shows the cause:

```
fskitd[428]: About to get current agent for 501
fskitd[428]: Received error '(null)', errno 2, retrieving team ID
```

repeated on every attempt. `fskitd` cannot resolve a Developer Team ID for the
module at all (errno 2 = ENOENT on the lookup itself), so the settings UI
never reaches an allow/deny decision. This is the same class of bug reported
upstream for a from-scratch FSKit module:
[`andrewgazelka/loaf#1`](https://github.com/andrewgazelka/loaf/issues/1),
"FSKit third-party extensions broken on macOS 26" — there `fskitd` logs an
explicit entitlement denial (`Hello FSClient! entitlement no`) rather than a
team-ID lookup failure, against earlier builds (25B78, 25C56), but the shape is
the same: `fskitd` refuses to authorize a third-party FSKit module regardless
of signing or entitlements. A from-scratch FSKit filesystem (e.g. via
`KhaosT/FSKitSample`, Apache-2.0) would very likely hit the same wall once
built, independent of whatever crate or hand-written FFI serves it.

Also unresolved, and moot until the above is fixed: whether `FSUnaryFileSystem`
can serve purely synthetic content or requires a real block-device resource.
Apple's own sample mounts against a dummy `hdiutil`-created raw disk image
rather than truly resource-less content, but macFUSE's FSKit backend
demonstrably mounts a synthetic in-memory tree, so this constraint (if it is
one) is not fundamental to FSKit — just unconfirmed from first-hand testing
here.

*Practical consequence:* mounting through this crate on macOS requires
approving macFUSE's kernel extension — and on Apple Silicon, that approval
path itself has a cost worth knowing up front (see below), not just a click.

*To change:* retest once Apple ships a `fskitd` fix (track
`andrewgazelka/loaf#1` or file a fresh report) — first against macFUSE's own
FSKit module (no crate changes needed, since `mount_macfuse` handles it), then
reconsider a `fuser` upgrade or hand-written FFI if a bare `fuse_mount`-style
entry point regains relevance. A from-scratch FSKit filesystem is a separate,
larger option gated on the same `fskitd` fix, not on anything in this crate.
Moot for the decided NFS backend either way — see the top of this section.

## macOS: approving macFUSE's kernel extension requires lowering boot security (Apple Silicon)

**Only applies to the FUSE fallback, not the decided NFS backend** — the
README's macOS row lists no install burden for the default path precisely
because NFS avoids all of this. Relevant only when deliberately using the
`fuse`/macFUSE path instead.

On Apple Silicon, there is no click-through approval for a third-party kernel
extension in ordinary System Settings — no "Driver Extensions" row appears
under Login Items & Extensions, and nothing appears under Privacy & Security
either, even after `kernelmanagerd`/`syspolicyd` have logged an explicit
`Kernel Extension BLOCKED: ... not approved to load. Please approve using
System Settings` in response to a real mount attempt. The approval surface
only exists after the machine is rebooted into **Recovery Mode**, **Startup
Security Utility** is opened, and the boot security policy is lowered from
"Full Security" to **"Reduced Security"** with **"Allow user management of
kernel extensions from identified developers"** checked. That is a standing
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
mattering; `ciphercask` (Phase 3) does not need it yet.

## NFS: hand-rolled RPC framing, not the `onc-rpc` crate

`backend/nfs/rpc.rs` hand-rolls ONC RPC (RFC 5531) record marking and
call/reply headers rather than using the `onc-rpc` crate
(`domodwyer/onc-rpc`, BSD-3-Clause, already on `deny.toml`'s allow-list).
`onc-rpc` only covers that envelope layer, not the MOUNT/NFSv3 payload XDR
(`fattr3`, `dirlistplus3`, and the rest), which needs hand-rolling regardless
— so a partial dependency saves little. `onc-rpc` remains the option if the
hand-rolled envelope ever needs replacing.

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

*To change:* use a constant-time comparison (e.g. `subtle::ConstantTimeEq`) if
this ever needs a stronger guarantee than "loopback-only."

## `allow_other` and `auto_unmount` are FUSE-only

Both are FUSE mount options. The NFS backend has no counterpart — the server
binds to loopback and authorizes with the file-handle secret rather than by uid
— and neither does cfapi, where a sync root belongs to the user running the
process. `backend/preflight.rs` rejects a request for either on a backend whose
`Caps` does not claim it, naming the backend, rather than accepting the call and
quietly doing nothing.

*To change:* nothing to change for `auto_unmount` — dropping the `Mount`
unmounts on every backend, which is what it was for. Exposing an NFS mount to
other users would mean real per-user authorization in place of the handle
secret, which `docs/PLAN.md`'s Phase 0.6 deliberately rejected.

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
