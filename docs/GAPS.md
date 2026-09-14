# Known gaps

What `anymount` does not do, why, and what it would take to change. One
section per gap, each ending in what changing it would cost.

`README.md` summarizes these for someone deciding whether to use the crate;
`docs/ARCHITECTURE.md` covers why each backend was chosen over its
alternatives, which is a different question from what the chosen one cannot
do.

## Scope

### Read-only

Write operations report `EROFS`. Read-only is the scope rather than a stage,
and the trait has no write ops to leave unimplemented.

*To change:* add write ops to `ReadOnlyFs` (or a `ReadWriteFs` supertrait).
cfapi has a real callback path for writes, and ProjFS has none at all, so a
Windows write story beyond cfapi's callbacks realistically means WinFsp, which
reintroduces GPLv3 — see `docs/ARCHITECTURE.md`'s licensing table.

### No symlinks or hardlinks

`FileKind` has only `File` and `Directory`. There is no `readlink` op, and
cfapi does not model links the way FUSE does.

*To change:* add `FileKind::Symlink` plus a `readlink` op. `FileKind` is not
`#[non_exhaustive]`, so adding a variant is a 2.0.

### No extended attributes or alternate data streams

`ReadOnlyFs::listxattr` and `getxattr` exist with defaults that report no
attributes — an empty list and `FsError::NoXattr` respectively — and only the
FUSE backend calls them. `backend/nfs/` and `backend/cfapi.rs` never do, so an
implementation that overrides both still reports no attributes on macOS and
Windows. Windows alternate data streams are not modelled at all: cfapi
projects one stream per placeholder.

NFSv3 has no xattr protocol of its own, which is the reason the NFS side is
unimplemented rather than merely unfinished. `FsError::NoXattr` maps to
`NFS3ERR_NOTSUPP` and `STATUS_NOT_SUPPORTED` for the paths that can return it.

*To change:* on Windows, cfapi can carry a stream per placeholder, so
`getxattr` could back a named stream. On macOS it would mean an NFSv4 server
rather than NFSv3, which is a different backend, not an addition to this one.

### No `statfs` numbers by default, so `df` reports an empty volume

The default `statfs` reports zeroed counters. The FUSE and NFS backends pass
whatever it returns straight through, so an implementation that knows its own
size can override it and get correct `df` output on Linux and macOS. cfapi
never asks, so the override has no effect on Windows.

*To change:* override `statfs`. Nothing in the crate needs to change for
Linux or macOS; Windows would need a sync-root quota reported through cfapi's
own registration, which this crate does not set.

### No async

`ReadOnlyFs` is synchronous. Concurrency comes from serving requests on
several threads rather than from an async runtime: FUSE runs a worker pool
sized by `MountBuilder::threads`, the NFS server takes one thread per
connection, and cfapi's callbacks are dispatched by the platform.

*To change:* add an `async` feature with a parallel trait and a bridging
adapter. Worth doing only when an async consumer exists.

## The trait contract

### Only FUSE issues ranged reads

`read_at` takes an offset, but the backends use it differently. cfapi has no
ranged-read path: it fetches an entire file on first touch, unconditionally,
regardless of hydration policy. The NFS backend reads at whatever offsets its
client asks for, which in practice is sequential. Only FUSE issues random
reads at arbitrary offsets.

An implementation backed by a format that cannot seek — a content-defined
chunked archive that records no plaintext chunk offsets, say — should
materialize the whole file on `open` and serve reads from a cache. That costs
less than it sounds, because cfapi materializes whole files anyway.
`ReadOnlyFs`'s "Read patterns differ by backend" rustdoc has the detail.

*To change:* nothing in the trait. This is a property of each platform's
fetch path, and an implementation that can seek already benefits on FUSE.

### `FileAttr`'s convenience constructors fill in fixed metadata

`FileAttr::dir` and `FileAttr::file` supply defaults rather than real
metadata: mode `0o555` for directories, `nlink` 2, and
`SystemTime::UNIX_EPOCH` for all three timestamps. They exist so a small
implementation can be written in a few lines, not because those values are
right.

This matters most for an archive format that stores files but not the
directories containing them, where the tree is rebuilt from path strings and
there is no recorded metadata for a directory to report.

*To change:* build `FileAttr` with a struct literal instead. Every field is
public and the type is not `#[non_exhaustive]`, so nothing in the crate needs
to change.

### `readdir` may page, and an empty page is the only end-of-directory signal

`ReadOnlyFs::readdir` is allowed to return fewer than all the remaining
entries; `backend/readdir.rs`'s `emit` calls it again at a higher offset until
it returns an empty page. A short page therefore cannot mean "this is the
end" — only an empty one can. An implementation that returned a partial page
as its final answer would have the directory reported as complete at that
point.

The alternative — requiring every call to return the whole remaining tail —
made FUSE quadratic on large directories, since FUSE asks for a few kilobytes
at a time and each call rebuilt the whole remainder, and it silently truncated
any paging implementation on NFS and cfapi, where one listing pass is one
reply.

*To change:* nothing. Both shapes work; an implementation that always returns
everything remaining is still correct, because its next call returns an empty
page.

### `..` reports the directory's own inode when `lookup(dir, "..")` is unanswered

`backend/readdir.rs`'s `emit` resolves `..`'s inode with `lookup(dir, "..")`
and falls back to the directory itself when that fails. `ReadOnlyFs`'s docs
ask implementors to answer `.` and `..`, but treat it as a should: an
implementation that does not still gets a usable listing, with a cosmetically
wrong `..` fileid, rather than a failed `readdir`. Under FUSE the kernel
resolves `..` from its own dentry cache regardless, so only tools reading
`d_ino` straight out of `getdents` — `find -inum`, say — can observe it.

*To change:* answer `lookup(dir, "..")`. `examples/memfs.rs` shows the shape.

### `allow_other`, `auto_unmount` and `threads` are FUSE-only

`allow_other` and `auto_unmount` are FUSE mount options. The NFS backend has
no counterpart — it authorizes with the file-handle secret rather than by uid
— and neither does cfapi, where a sync root belongs to the user running the
process.

`MountBuilder::threads` is FUSE-only for a different reason: FUSE is the only
backend that owns a worker pool. The NFS server sizes itself from the
connections its client opens, and cfapi's callbacks are dispatched by the
platform.

`backend/preflight.rs` rejects a request for any of the three on a backend
whose `Caps` does not claim it, naming the backend, rather than accepting the
call and quietly doing nothing.

*To change:* nothing to change for `auto_unmount` — dropping the `Mount`
unmounts on every backend, which is what it was for. Exposing an NFS mount to
other users would mean real per-user authorization in place of the handle
secret — see `docs/ARCHITECTURE.md`'s seams and invariants for why the current
secret-based scheme was chosen over `AUTH_SYS`.

### An OS-initiated unmount is not reported to the process

The mount can be taken down by someone other than the process serving it:
ejecting the volume in Finder, `umount` on macOS, `fusermount3 -u` on Linux.
Nothing in the crate prevents that, and nothing reports it. `Mount` has no
"still mounted" query and no callback for the mount going away.

An OS unmount detaches the client side and no more. Reads stop, and the
mountpoint goes back to whatever it held before. The server keeps running: the
NFS server thread stays in its accept loop, and on the local-socket transport
its socket directory stays under `/tmp`, until the `Mount` is dropped or
`unmount` is called. Nothing is corrupted — the filesystem is read-only, and a
read in flight gets an error rather than wrong data — but a long-lived process
that mounts, is ejected, and never tears down the handle accumulates a thread
and a directory each time.

Teardown itself is idempotent, so an unmount that has already happened is not
reported as a failure. `unmount(2)` gives `EINVAL` for a path that is no longer
a mount point, which is indistinguishable by errno from a real failure, so the
NFS backend checks whether the path is still a mount point — a mounted
directory and its parent have different device numbers — and treats "already
gone" as success. FUSE reaches the same answer through `fuser`, which tests the
FUSE device before unmounting a second time. A genuine failure, such as `EBUSY`
from a file still open on the mount, is still reported.

A process killed by a signal is the worse case, and an eject invites one: the
mount is gone, so the process looks finished, and `Drop` does not run on `SIGINT`
or `SIGKILL`. On macOS that leaves the mount in the mount table pointing at a
server that no longer exists, plus a `.anymount-<hex>` socket directory in
`/tmp`; clear them with `umount <mountpoint>` and by removing the directory. On
Linux, `fusermount3 -u <mountpoint>` clears the mount, and
`MountBuilder::auto_unmount` avoids it in the first place — on FUSE only, and
only for a mount that is not owner-private.

Windows has no equivalent affordance, since a cfapi sync root is a projected
directory rather than a volume Explorer can eject. The hazard there is the same
signal case for a different reason: a sync root registration outlives the
process, so a run that never reaches `unmount` leaves one behind. This section
was verified on macOS and read off `fuser`'s source for Linux; the Windows
behaviour is not tested.

*To change:* reporting an OS unmount means either polling the mountpoint or a
per-backend watch, and there is no macOS equivalent of the FUSE session ending
to hang the latter on. A caller that needs it can poll `Mount::mountpoint`
itself with `statfs` or `/proc/mounts`, which is what an added API would do. The
API is frozen at 1.0 regardless, so this would be a 2.0 question.

### `tracing` covers lifecycle and discarded errors, not every operation

The `tracing` feature wires `backend/trace.rs` into the places an error would
otherwise be dropped silently — a failed `ReadOnlyFs::release`, a failed unmount
during `drop` — and to mount and unmount themselves. It is not the
per-operation span coverage a profiler would want.

*To change:* instrument each adapter callback in `backend/fuse.rs` and each
procedure in `backend/nfs/nfs_proto.rs`. Worth doing when someone is actually
debugging a latency problem through the trait.

## Linux

### FUSE needs `fusermount3` on the system

The Linux backend mounts by executing `fusermount3`, which comes from the
distribution's `fuse3` package and is the one install step any platform
requires. This is the trade for not linking libfuse: no LGPL code enters the
link, and mounts work unprivileged, at the cost of a binary that has to be
present. A machine without it fails at `mount()` rather than at build time.

*To change:* nothing worth changing. Linking libfuse would remove the
dependency on the helper and add an LGPL one, which `deny.toml` refuses by
design.

## macOS

### The loopback fallback is readable by any local account

The macOS backend prefers an `AF_UNIX` socket and falls back to loopback TCP.
Only the fallback has this gap; `Mount::nfs_uses_local_socket` reports which
path a mount took, and `MountBuilder::nfs_require_local_socket` refuses the
fallback outright.

On the local-socket path the root file handle goes to `mount(2)` in the
argument buffer, so the MOUNT protocol never runs and no credential goes into
a path. Nothing listens on the network, and reaching the server means opening
a socket at mode 0600 inside a directory at mode 0700. The mount table shows
the socket path and a decorative label, with no credential in either:

```text
</tmp/.anymount-baa6e763d48286a1/s>:/anymount-memfs on /private/tmp/anymount-demo
```

The fallback runs `mount_nfs` over loopback, which records an export path in
the system mount table. The mount table is system-wide, and `nfsstat -m`
reprints the same path for any local account, so the value in it is public for
the life of the mount:

```text
127.0.0.1:/export/771c61056ffa820a2ce973f5a78954c4/anymount-memfs on /private/tmp/demo
```

Two independent random values keep that from granting anything. The public
value authorizes `MNT` and nothing else. The value that prefixes file handles
is never written anywhere a reader can reach, and is not derived from the
public one. `MNT` is answered only until `mount_nfs` exits, which is safe
because `MNT` is issued exactly once per mount lifetime — a soft mount that
loses its connection reconnects and carries on serving without asking again.
So by the time the public value reaches a surface another account can read,
there is no call left to spend it on.

What remains is a race. The public value is on `mount_nfs`'s command line
while the mount is being established, where a same-uid process or root can
read it — macOS restricts `KERN_PROCARGS2` to the calling uid, so `ps` shows
other accounts only the executable path. A process polling for it could beat
the legitimate client to `MNT`. Loopback also has no per-user isolation, so
any local account can connect to the server and guess; the value is 128 bits.

Stronger access control on this transport was looked for and not found. A
reserved source port needs root, and unprivileged mounting is a property the
backend is built around. Peer credentials identify nothing, because the kernel
owns the client end of the connection: `LOCAL_PEERCRED` reports uid 0 whichever
account mounted. `RPCSEC_GSS` needs a Kerberos realm and a GSS implementation
at both ends. Reading the mount arguments back to validate them needs the
`NFS_MOUNTINFO` sysctl, which is entitlement-gated and answers `EPERM` without
it. What remains is to avoid the transport, which is what the local socket
does.

*To change:* nothing further on the fallback itself — it is the fallback
because it cannot be made as good as the primary path on documented
interfaces. Build without the `nfs-tcp` feature, or set
`nfs_require_local_socket`, and the gap is gone along with the transport.
Prefer the builder option where it matters: cargo features unify, so another
crate in the dependency graph enabling `nfs-tcp` restores the fallback for
everyone, while the builder option cannot be overridden from outside.

### Reconnection over the local socket is only half-tested

The server's half is covered: `server_tests.rs` drives the accept loop over an
`AF_UNIX` socket through more disconnect-and-redial cycles than
`MAX_CONNECTIONS` allows, and requires every one to be answered. That also pins
the worker reaping, without which the seventeenth redial would wait in the
listen backlog until unmount.

The client's half is not. Whether the macOS kernel NFS client redials a
`ticotsord` connection the server has closed cannot be provoked from outside
the process: the server closes a connection only on EOF, a malformed header or
a write error, and none of those can be aimed at the kernel's connection while
leaving the mount up.

What has been measured on macOS 26 (Darwin 25.6.0, arm64) is the behaviour
either side of that question. A server stopped with `SIGSTOP` leaves reads
blocked, and they fail with `Operation timed out` somewhere between 15 and 30
seconds — so the soft mount does bound a stalled server over this transport,
though on a longer horizon than `timeo=20,retrans=2` suggests, since the client
backs off between retransmissions. After `SIGCONT` the next read succeeds. The
connection is the same one throughout: the kernel holds it across the stall
rather than replacing it, so a stall exercises recovery but not reconnection. A
server that exits outright gives the same bounded `Operation timed out`, and
leaves the mount in place with nothing behind it.

*To change:* the server would need a way to close an accepted connection while
still listening, reachable from a test. That is a test seam in `server.rs`
rather than a change to how the backend runs, and it would make the client half
testable on a Mac with a real mount.

### The local-socket transport depends on a private macOS interface

`mount_nfs` exposes none of the three attributes the local-socket path needs:
its option table has no file-handle option and no mount-from option, and its
local-socket parsing is not reachable from the command line. All three share
one prerequisite, a hand-built `mount(2)` argument buffer whose layout
`<nfs/nfs.h>` declares behind `__APPLE_API_PRIVATE`. The attribute numbers are
in the header; the shape of the buffer is not.

Two undocumented things are relied on, not one. The buffer layout is the
larger. The smaller is the socket netid, `ticotsord`: `mount_nfs(8)` documents
`proto=<netid>` as accepting `tcp`, `udp`, `tcp6` and `udp6` only, and the
shipped binary parses `ticotsord` and `ticlts` without listing them.

The exposure is to a change in behaviour, not to a change in the SDK. Nothing
is compiled against the private header: there is no build script and no C in
the crate, the attribute numbers are transcribed into Rust constants, and the
only libc calls involved are `mount(2)` and `unmount(2)`, both of which have
man pages. A macOS that removed or renamed the declarations would not break the
build.

A macOS that changes the encoding does break the path, and three things bound
the damage. A wrong buffer is rejected rather than partly honored — a length
read in the wrong byte order returns `ENOMEM`, an over-long buffer returns
`E2BIG`, and a misaligned attribute stream returns `ENOMEM`. `mount` then
checks the root back through the mountpoint rather than trusting the return
code. And a failure falls back to `nfs-tcp` where it is compiled in, so the
outcome is a working mount with weaker properties rather than no mount.

`mount_args.rs` pins the encoded bytes in a unit test, so a change made here
is visible without a mount. That test cannot see a change made by Apple.

*To change:* build without `nfs-local-socket`, which removes the encoder along
with the transport and leaves a backend that uses documented interfaces only,
at the cost of the disclosure described in the section above. The durable fix
for having both is FSKit, which is not usable — see "No FSKit backend" below.

### No per-inode handle cache — `READ3` pays an open/release round trip

The NFS backend (`backend/nfs/`) calls `fs.open`/`read_at`/`release` on every
`READ3` RPC rather than caching a handle per `Ino` across calls. This is
correctness-complete — including for the sequential read bursts a real client
issues — but pays one extra open/release round trip per read RPC. An
idle-evicting handle cache keyed by `Ino` would remove that cost.

*To change:* add a `Mutex<HashMap<Ino, (FileHandle, Instant)>>` (or similar) to
`backend/nfs/mod.rs`'s per-mount state, with an eviction sweep on read
staleness. Worth doing once a real workload shows the round trip mattering.

### At most 16 concurrent connections

`backend/nfs/server.rs` serves at most `MAX_CONNECTIONS` connections at once,
reaping finished workers each time round the accept loop. Only this mount's own
kernel NFS client has business connecting, so the cap exists to stop a local
process opening sockets in a loop from costing one thread per connection until
unmount. It matters on the loopback fallback, which any local account can
reach; on the local-socket transport the socket's permissions already decide
who may connect. A legitimate client that hits the cap waits in the kernel's
listen backlog until a slot frees.

*To change:* raise the constant, or move to a thread pool with a work queue, if
a client is ever found that needs more than a handful of connections.

### Single-fragment RPC messages only

`backend/nfs/rpc.rs`'s `read_message` closes the connection on a multi-fragment
ONC RPC message rather than reassembling one from several TCP fragments. Every
request `mount_nfs` sends in practice fits in one fragment; reassembly would
only matter for an NFS client this crate has not been exercised against.

*To change:* buffer fragments keyed by connection until the last-fragment bit
is set, then dispatch the reassembled body.

### Hand-rolled RPC framing, not the `onc-rpc` crate

`backend/nfs/rpc.rs` hand-rolls ONC RPC (RFC 5531) record marking and
call/reply headers rather than using the `onc-rpc` crate
(`domodwyer/onc-rpc`), which is BSD-3-Clause and so would pass `deny.toml`'s
licence allow-list. It covers only that envelope layer, not the MOUNT/NFSv3
payload XDR (`fattr3`, `dirlistplus3`, and the rest), which needs hand-rolling
regardless — so a partial dependency saves little.

*To change:* swap `rpc.rs`'s envelope for `onc-rpc` and keep the payload XDR.
Worth doing only if the hand-rolled envelope turns out to need maintenance.

### No FSKit backend

FSKit (macOS 15.4+) is Apple's kernel-extension-free framework for user-space
filesystems, and would in principle be a second kext-free option alongside NFS.
It is not usable: third-party FSKit modules — including macFUSE's own signed
FSKit backend — fail to authorize on macOS 15.x builds (`fskitd` cannot resolve
a Developer Team ID for the module; tracked upstream at
[`andrewgazelka/loaf#1`](https://github.com/andrewgazelka/loaf/issues/1)).
This does not affect the shipped backend, which is NFS and does not depend on
FSKit.

*To change:* retest once Apple ships a `fskitd` fix. Only relevant if NFS ever
needs a replacement.

## Windows

### A directory, not a drive letter

cfapi projects into a directory under a virtualization root and cannot assign
`X:`.

*To change:* WinFsp is the only Windows option that mounts a real volume, and
it is GPLv3 with a paid commercial license. It would have to live in a
separate, opt-in `anymount-winfsp` crate so it never enters a default
dependency graph.

### The mountpoint must be empty, and unmounting clears it

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
The empty-mountpoint precondition at mount time was always the actual safety
guarantee, so removal no longer depends on the attribute surviving.

*To change:* nothing outstanding here.

### No per-inode handle cache — `FETCH_DATA` pays an open/release round trip

`backend/cfapi.rs`'s `stream_fetch` calls `fs.open`/`read_at`/`release` once per
`FETCH_DATA` callback rather than caching a handle per `Ino`. Since cfapi always
fetches a whole file in one callback (see "Only FUSE issues ranged reads"
above), this costs one open/release pair per file rather than per read the way
the NFS gap does — a smaller version of the same tradeoff.

*To change:* same shape as the NFS gap above, if a real workload shows it
mattering.

### A ProjFS backend would have to resolve its entry points dynamically

There is no ProjFS backend: no `projfs` feature, no `Backend::ProjFs`, and no
ProjFS code in the tree. `docs/ARCHITECTURE.md` has why cfapi was chosen over
it.

Recording the constraint here because it is easy to miss for anyone adding one
later: if `ProjectedFSLib.dll` is ever used, its entry points must be resolved
dynamically, with `GetProcAddress` or delay-loading, rather than linked
statically. `Client-ProjFS` is an optional Windows feature that is off by
default, and a static import would stop the whole binary from starting on a
machine that has not enabled it — including binaries that never mount anything.

*To change:* this would mean adding a Windows backend from scratch, not
restoring a stub.
