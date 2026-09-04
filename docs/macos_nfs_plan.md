# macOS NFSv3 backend for `anymount`

## Context

`anymount` mounts a read-only filesystem from user space on Linux (FUSE),
macOS, and Windows (Cloud Files API). macOS's mechanism was decided in
`docs/PLAN.md`'s Phase 0/0.6 spikes: a from-scratch, unprivileged NFSv3
server mounted with the OS's built-in `mount_nfs` client — no macFUSE, no
kernel extension, no Reduced Security boot policy, no root. That decision
was fully proven out in a throwaway spike (not in the tree; only its
detailed write-up survives, in `docs/PLAN.md`'s "Phase 0.6" section), but
`backend/nfs.rs` itself was never built — macOS currently compiles with
**no** mount backend at all, and `mount()` there returns
`FsError::Unsupported`.

This plan turns that spike into the real `backend/nfs.rs`. It was produced
by a design pass (a Plan subagent) that read the existing FUSE/cfapi
backends, the trait, the error type, and the full Phase 0.6 spike write-up,
and worked out the exact RFC 1813 (NFSv3) / RFC 1094 & 1813 Appendix I
(MOUNT protocol) wire shapes needed, since guessing XDR field order/types
is the likeliest place to get this subtly wrong. Nothing has been
implemented yet — this is the plan to execute later.

Unlike the Windows cfapi backend (which needs a Windows box) or the FUSE
macOS fallback (which needs macFUSE + kext approval), this backend can be
built and verified **end to end on an ordinary macOS dev machine**, since
`mount_nfs` ships in `/sbin` on every Mac with no extra install — the
verification plan in §14 below is real, not aspirational.

## 1. Module/file layout

A submodule tree, not one file, given how much protocol machinery is
involved:

```
src/backend/nfs/
  mod.rs             // module doc comment, mount()/NfsHandle, MountBuilder wiring, secret gen,
                      // mount_nfs invocation, unmount ordering
  xdr.rs             // Reader/Writer: bounds-checked u32/u64/bool/opaque/string primitives
  rpc.rs             // ONC RPC record marking + call/reply header encode/decode, opaque_auth
                      // skip/encode, dispatch scaffolding (prog, vers, proc) -> accept_stat
  mount_proto.rs      // MOUNT program (100005 v3): NULL/MNT/UMNT/EXPORT, mountres3
  nfs_proto.rs        // NFS program (100003 v3): GETATTR/LOOKUP/ACCESS/READ/READDIR(PLUS)/
                      // FSSTAT/FSINFO/PATHCONF, fattr3/post_op_attr/wcc_data helpers,
                      // the write-op rejection table
  handle.rs           // FileHandle3 encode/decode/resolve, secret (arc4random_buf)
  server.rs           // TcpListener accept loop, per-connection thread, dispatch by program

src/backend/readdir_cookie.rs   // extracted from fuse.rs's private `cookie` module, no cfg gate
```

Test files, per `CLAUDE.md`'s `*_tests.rs` sibling convention (each
registered with `#[cfg(test)] #[path = "..."] mod ...;`):

```
src/backend/nfs/xdr_tests.rs
src/backend/nfs/handle_tests.rs
src/backend/nfs/nfs_proto_tests.rs
src/backend/readdir_cookie_tests.rs   // unconditionally compiled — the point of extracting it
```

`fuse_tests.rs` keeps its `Call`/`one_call`/`full_listing` FUSE-
`ReplyDirectory`-shaped simulation (specific to FUSE's "add returns bool
for buffer-full" contract), but its `for_entry`/`trait_offset` property
tests move to `readdir_cookie_tests.rs` since the functions themselves move
to `readdir_cookie.rs`. `fuse.rs` becomes `use
crate::backend::readdir_cookie as cookie;`, same public shape (`DOT`,
`DOTDOT`, `trait_offset`, `for_entry`), all `cfg`-free — so the module and
its proptest suite compile and run on every platform, not just Linux CI as
today.

`backend/mod.rs` gates the new module: `#[cfg(all(target_os = "macos",
feature = "nfs"))] pub(crate) mod nfs;`, mirroring `fuse`'s Linux gate.

## 2. RPC/XDR layer (`rpc.rs`, `xdr.rs`)

### Record marking (RFC 5531 §11)

Each TCP-carried RPC message is preceded by a 4-byte fragment header: high
bit = last-fragment flag, low 31 bits = length. **v1 scope: single-fragment
messages only**, matching the spike. `rpc::read_message(stream) ->
Option<Vec<u8>>`:

- Read 4 bytes; closed mid-header → `None` (clean EOF, not a panic).
- `last = (hdr & 0x8000_0000) != 0`; `len = hdr & 0x7fff_ffff`.
- Cap `len` at a fixed ceiling (e.g. 256 KiB) and reject/close the
  connection if exceeded, rather than allocating an attacker-controlled
  buffer.
- If `!last`, close the connection cleanly (documented v1 limitation).
- Read exactly `len` bytes, short-read-safe (loop until filled or closed).

Writing a reply: buffer the full body first, then one 4-byte header with
the last-fragment bit set and the exact length, then the body.

### `xdr::Reader<'a>` (`&'a [u8]`, cursor `usize`)

All read methods return `Option<T>`, never index/slice without a prior
bounds check:

- `read_u32() -> Option<u32>` — 4 bytes, big-endian.
- `read_u64() -> Option<u64>` — 8 bytes big-endian (XDR `hyper`).
- `read_bool() -> Option<bool>` — `read_u32()` mapped `0 => false, _ =>
  true`.
- `read_opaque_fixed<const N: usize>() -> Option<[u8; N]>` — exactly `N`
  bytes, no length prefix (used for `cookieverf3`, 8 bytes, already a
  multiple of 4 so no padding).
- `read_opaque_var(max: u32) -> Option<Vec<u8>>` — `len = read_u32()?`;
  reject if `len > max`; read `len` bytes; skip `(4 - len % 4) % 4` padding
  bytes, bounds-checked.
- `read_string(max: u32) -> Option<OsString>` — same framing, then
  `OsString::from_vec` (NFS filenames are opaque bytes, no UTF-8
  requirement).
- `skip(n: usize) -> Option<()>` — used to skip `opaque_auth` bodies
  without decoding cred contents (`AUTH_SYS`'s claimed uid/gid is
  decorative per the spike, but the bytes must still be skipped correctly
  to reach the procedure args).

### `xdr::Writer` (`Vec<u8>`)

`write_u32`, `write_u64`, `write_bool`, `write_opaque_fixed(&[u8])` (pads to
4), `write_opaque_var(&[u8])` (length prefix + data + pad),
`write_string(&OsStr)` (same as `write_opaque_var` over
`as_encoded_bytes()`), and `len() -> usize`.

`len()` is needed by `READDIR(PLUS)`'s budget check: encode one candidate
entry into a **scratch `Writer`**, check `scratch.len()` against the
remaining budget, and only then append it to the main writer — this is the
mechanism that fixes the spike's "dump everything, TCP tolerates it" trap
(see §5).

### RPC call/reply headers (RFC 5531 §9)

Call header:

```
xid: u32
mtype: u32           // must be 0 (CALL)
rpcvers: u32         // must be 2
prog: u32            // 100005 (MOUNT) or 100003 (NFS)
vers: u32            // must be 3 for both
proc: u32
cred: opaque_auth    // flavor: u32, body: opaque<400>
verf: opaque_auth    // flavor: u32, body: opaque<400>
<procedure-specific args...>
```

`opaque_auth` decode: `flavor = read_u32()?`, `read_opaque_var(400)?` for
the body — read to advance the cursor correctly, never inspected for
authorization (matches the spike's "`AUTH_SYS` is decorative" finding).

Reply header (`MSG_ACCEPTED`, the only case this server sends — malformed
`rpcvers` is the one `MSG_DENIED` special case):

```
xid: u32              // echoed from the call
mtype: u32 = 1         // REPLY
reply_stat: u32 = 0    // MSG_ACCEPTED
verf: opaque_auth      // AUTH_NONE: flavor=0, body=opaque<0>
accept_stat: u32        // 0=SUCCESS,1=PROG_UNAVAIL,2=PROG_MISMATCH,3=PROC_UNAVAIL,4=GARBAGE_ARGS,5=SYSTEM_ERR
<on SUCCESS: procedure-specific result>
<on PROG_MISMATCH: low:u32, high:u32>
```

`rpc::dispatch(stream)` reads one message, decodes the call header, routes
on `(prog, vers, proc)`:

- unknown `prog` → `PROG_UNAVAIL (1)`.
- known `prog`, `vers != 3` → `PROG_MISMATCH (2)`, body `low=3, high=3`.
- known `(prog, 3)`, unknown `proc` → `PROC_UNAVAIL (3)`.
- otherwise hand the remaining `Reader` to `mount_proto`/`nfs_proto`'s
  handler, which returns `SUCCESS` plus its own result (its own result
  already carries an `nfsstat3`/`mountstat3` inside — RPC-level success and
  NFS-level failure are different layers, must not be conflated).
- a `Reader` that runs out of bytes mid-decode → `GARBAGE_ARGS (4)`, never
  a panic.

### `onc-rpc` crate — evaluated, not used

`onc-rpc` (`domodwyer/onc-rpc`, BSD-3-Clause, on `deny.toml`'s allow-list)
is a real sans-I/O, sync, zero-alloc ONC RPC envelope crate. It would only
cover record marking + call/reply headers, not MOUNT/NFSv3 payload XDR
(`fattr3`, `dirlistplus3`, etc.), which still needs hand-rolling regardless.
**Recommendation: hand-roll for v1**, matching the spike — `rpc.rs` is
small and directly testable on its own. Record `onc-rpc` in `docs/GAPS.md`
as the option if the hand-rolled envelope ever needs replacing. No new
dependency either way.

## 3. `FileHandle3` (`handle.rs`)

```rust
pub(super) struct FileHandle3 {
    secret: [u8; 16],
}

impl FileHandle3 {
    pub(super) fn new_random() -> Self {
        let mut secret = [0u8; 16];
        // SAFETY: arc4random_buf writes exactly `len` bytes into a valid,
        // correctly-sized buffer we own; it cannot fail and needs no seeding.
        unsafe { libc::arc4random_buf(secret.as_mut_ptr().cast(), secret.len()) };
        Self { secret }
    }

    pub(super) fn encode(&self, ino: Ino) -> [u8; 24] {
        let mut out = [0u8; 24];
        out[..16].copy_from_slice(&self.secret);
        out[16..].copy_from_slice(&ino.0.to_be_bytes());
        out
    }

    /// `None` on wrong length or secret mismatch — both map to
    /// `NFS3ERR_NOENT` / `MNT3ERR_ACCES`, never a panic.
    pub(super) fn resolve(&self, bytes: &[u8]) -> Option<Ino> {
        if bytes.len() != 24 || bytes[..16] != self.secret {
            return None;
        }
        Some(Ino(u64::from_be_bytes(bytes[16..24].try_into().ok()?)))
    }
}
```

`libc::arc4random_buf` is sound: a real Darwin/BSD libc CSPRNG needing no
seeding, `libc` is already a macOS target dependency (`types.rs`'s
`getuid`/`getgid`), and this avoids adding `rand` as a new runtime
dependency. No constant-time comparison in v1 — note as unmeasured in
`docs/GAPS.md`, matching the spike's own posture.

The secret also needs 32-lowercase-hex rendering for the `MNT` export path
(`/export/<hex>`) and the `mount_nfs` command line — trivial
`format!("{:02x}", b)` per byte, no dependency needed.

## 4. `FsError` → `nfsstat3` mapping (`error.rs`)

Parallel to `to_errno`'s `#[cfg(unix)]` idiom, this one is
`#[cfg(all(target_os = "macos", feature = "nfs"))]`:

```rust
#[cfg(all(target_os = "macos", feature = "nfs"))]
pub(crate) fn to_nfsstat3(&self) -> u32 {
    match self {
        Self::NotFound => 2,               // NFS3ERR_NOENT
        Self::PermissionDenied => 13,      // NFS3ERR_ACCES
        Self::NotADirectory => 20,         // NFS3ERR_NOTDIR
        Self::IsADirectory => 21,          // NFS3ERR_ISDIR
        Self::InvalidArgument => 22,       // NFS3ERR_INVAL
        Self::NoXattr => 10004,            // NFS3ERR_NOTSUPP
        Self::ReadOnly => 30,              // NFS3ERR_ROFS
        Self::Unsupported(_) => 10004,     // NFS3ERR_NOTSUPP
        Self::Io(_) => 5,                  // NFS3ERR_IO
        Self::Other(_) => 10006,           // NFS3ERR_SERVERFAULT
        Self::Context { errno_as, .. } => errno_as.to_nfsstat3(),
    }
}
```

Named `const`s for the status codes live in `nfs_proto.rs`, not repeated
magic numbers at call sites.

## 5. NFS program 100003 v3 (`nfs_proto.rs`) — exact wire shapes (RFC 1813 §3)

Common types:

- `fhandle3` — variable opaque, max 64 bytes, always 24 here.
- `ftype3` — `1=NF3REG, 2=NF3DIR` only (no symlink variant in `FileKind`).
- `specdata3` (rdev) — `{ u32=0; u32=0; }`.
- `nfstime3` — `{ u32 seconds; u32 nseconds; }`, from
  `SystemTime::duration_since(UNIX_EPOCH)`, saturating to 0 on error,
  truncating to `u32` (valid to year 2106) rather than panicking.
- `fattr3`:
  ```
  type: ftype3
  mode: u32          // FileAttr::perm widened
  nlink: u32
  uid: u32
  gid: u32
  size: u64
  used: u64          // = size, no sparse-file concept
  rdev: specdata3
  fsid: u64          // constant per mount
  fileid: u64        // = ino.0
  atime, mtime, ctime: nfstime3
  ```
- `post_op_attr` = `bool attributes_follow; if true { fattr3 }` — `true`
  except where the object couldn't be `getattr`'d.
- `post_op_fh3` = `bool handle_follows; if true { fhandle3 }`.
- `wcc_data` = `pre_op_attr` (`bool=false`, 4 zero bytes — never mutates) +
  `post_op_attr` — used only by the write-rejection table.

### `GETATTR3` (proc 1)

Args: `{ fhandle3 object; }`. Resolve handle → `NFS3ERR_NOENT` if
`handle.resolve` fails, else `to_nfsstat3(fs.getattr(ino).err())`.
Res: `union switch(nfsstat3){ NFS3_OK: { fattr3 obj_attributes; } default:
void; }`.

### `LOOKUP3` (proc 3)

Args: `{ diropargs3 what; }`, `diropargs3 = { fhandle3 dir; filename3
name; }`. Forwards `name` **verbatim** — including `"."`/`".."` — to
`fs.lookup(dir_ino, name)`; zero special-casing here (the convention that
implementors answer both is documented on the trait, §9).
Res: `union switch(nfsstat3){ NFS3_OK: { fhandle3 object; post_op_attr
obj_attributes; post_op_attr dir_attributes; } default: { post_op_attr
dir_attributes; } }` (best-effort `fs.getattr(dir_ino)` on the error arm).

### `ACCESS3` (proc 4)

Args: `{ fhandle3 object; u32 access; }` (`READ=1, LOOKUP=2, MODIFY=4,
EXTEND=8, DELETE=16, EXECUTE=32`). Granted `= requested & (READ | LOOKUP |
EXECUTE)` — `MODIFY`/`EXTEND`/`DELETE` never granted, enforcing read-only
independent of `perm` bits.
Res: `union switch(nfsstat3){ NFS3_OK: { post_op_attr obj_attributes; u32
access; } default: { post_op_attr obj_attributes; } }`.

### `READ3` (proc 6)

Args: `{ fhandle3 file; u64 offset; u32 count; }`. `count` capped
server-side at `FSINFO`'s `rtmax` before allocating the buffer (untrusted
client value must not drive unbounded allocation).
Handler: `fs.open(ino)` → `fs.read_at(handle, offset, &mut buf[..count])`
→ `fs.release(handle)` (release error logged, not propagated, matching
the trait's contract). `eof` from a `getattr`: `offset + n >= attr.size`.
Res: `union switch(nfsstat3){ NFS3_OK: { post_op_attr file_attributes; u32
count; bool eof; opaque data<>; } default: { post_op_attr
file_attributes; } }`.

**v1 tradeoff, explicit**: per-RPC `open`/`read_at`/`release`, not a
handle cache keyed by `Ino`. Correctness-complete (the spike proved it
works, including for sequential bursts) but pays an open/release round
trip per RPC. Ship this for v1; file the idle-evicting cache as a
`docs/GAPS.md` follow-up rather than build it now — real complexity/bug
surface disproportionate to what v1's consumer (`ciphercask`, Phase 3)
needs today.

### `READDIR3` (proc 16) / `READDIRPLUS3` (proc 17)

Args (`READDIR3`): `{ fhandle3 dir; cookie3 cookie; cookieverf3 cookieverf;
u32 count; }` (`cookie3 = u64`, `cookieverf3` = **fixed** 8-byte opaque, no
length prefix).
Args (`READDIRPLUS3`): `{ fhandle3 dir; cookie3 cookie; cookieverf3
cookieverf; u32 dircount; u32 maxcount; }`.

`cookieverf3`: a constant 8-byte value per mount (e.g. all-zero, or the
first 8 bytes of the secret) is always valid — content is immutable for
the mount's life, so there's no verifier-mismatch case.

Cookie scheme: reuse `readdir_cookie::{DOT, DOTDOT, trait_offset,
for_entry}` exactly as `fuse.rs` does — `cookie == 0` == FUSE's `offset ==
0` (serve `.` at cookie `DOT`); `cookie == DOT` serves `..` at
`DOTDOT` (target `Ino` via `fs.lookup(dir_ino, "..")`); `cookie >=
DOTDOT` resumes trait entries via `trait_offset`/`for_entry`.

**Entry packing (the part the spike got wrong first)**: for each candidate
entry, encode into a **scratch `Writer`** (`fileid: u64, name: filename3,
cookie: cookie3` for `READDIR3`; add `name_attributes: post_op_attr,
name_handle: post_op_fh3` for `READDIRPLUS3`), check `scratch.len()`
against the remaining budget (`count`/`maxcount` minus bytes already
committed, including fixed `dirlist3` overhead) **before** appending and
advancing the cursor. If it doesn't fit, stop — the resume cookie is the
last entry actually written, not the one that didn't fit. `eof = true`
only once the *trait's* `readdir` call for the next offset returns empty
(not just "ran out of budget this call").

Res (`READDIR3`): `union switch(nfsstat3){ NFS3_OK: { post_op_attr
dir_attributes; cookieverf3 cookieverf; dirlist3 reply; } default: {
post_op_attr dir_attributes; } }`, `dirlist3 = { entry3 *entries; bool
eof; }` (XDR optional-linked-list: `bool value_follows; if true { fileid3;
filename3; cookie3; <recurse> }`, terminated by `value_follows=false`).
Res (`READDIRPLUS3`) same shape, `entryplus3` adds `post_op_attr
name_attributes; post_op_fh3 name_handle;` per entry.

### `FSSTAT3` (proc 18)

Args: `{ fhandle3 fsroot; }`. Map from `fs.statfs()`: `tbytes = blocks *
frsize`, `fbytes = bfree * frsize`, `abytes = bavail * frsize`,
`tfiles/ffiles/afiles = files/ffree/ffree`, `invarsec = u32::MAX` (content
immutable for the mount's life).

### `FSINFO3` (proc 19)

`rtmax/rtpref = 1_048_576`, `rtmult = 4096`, `wtmax/wtpref` same as
`rtmax`, `wtmult = 4096` (writes always rejected regardless, but must be
present/nonzero), `dtpref = 32768`, `maxfilesize = u64::MAX`, `time_delta
= { 1, 0 }`, `properties = FSF3_HOMOGENEOUS (0x0008)` only — no
`FSF3_LINK`/`FSF3_SYMLINK`/`FSF3_CANSETTIME`.

### `PATHCONF3` (proc 20)

`linkmax=1; name_max=255; no_trunc=true; chown_restricted=true;
case_insensitive=false; case_preserving=true;` (`name_max` matches
`StatFs::default().namelen`).

### `NULL` (proc 0, both programs)

No args, no result body — `SUCCESS` with nothing following.

### Write/unsupported procedures — clean rejection table

Every one produces a syntactically valid NFSv3 result (RPC `SUCCESS`,
NFS-level error), never `PROC_UNAVAIL`/malformed. Each error arm has a
fixed shape independent of args, so a handler doesn't even need to decode
the request body:

| Procedure | proc # | `nfsstat3` | Default-arm body |
|---|---|---|---|
| `SETATTR3` | 2 | `NFS3ERR_ROFS` (30) | `wcc_data` (8 zero bytes) |
| `CREATE3` | 8 | `NFS3ERR_ROFS` | `wcc_data` |
| `MKDIR3` | 9 | `NFS3ERR_ROFS` | `wcc_data` |
| `SYMLINK3` | 10 | `NFS3ERR_NOTSUPP` (10004) | `wcc_data` |
| `MKNOD3` | 11 | `NFS3ERR_ROFS` | `wcc_data` |
| `REMOVE3` | 12 | `NFS3ERR_ROFS` | `wcc_data` |
| `RMDIR3` | 13 | `NFS3ERR_ROFS` | `wcc_data` |
| `RENAME3` | 14 | `NFS3ERR_ROFS` | two `wcc_data` (16 zero bytes) |
| `LINK3` | 15 | `NFS3ERR_ROFS` | `post_op_attr` + `wcc_data` |
| `WRITE3` | 7 | `NFS3ERR_ROFS` | `wcc_data` |
| `COMMIT3` | 21 | `NFS3ERR_ROFS` | `wcc_data` |
| `READLINK3` | 5 | `NFS3ERR_NOTSUPP` | `post_op_attr` (4 zero bytes) |

`SYMLINK3`/`READLINK3` get `NFS3ERR_NOTSUPP` (no such concept in
`FileKind`), not `NFS3ERR_ROFS` (would-mutate) — a real, worth-preserving
distinction.

## 6. MOUNT program 100005 v3 (`mount_proto.rs`, RFC 1813 Appendix I)

### `MOUNTPROC3_NULL` (proc 0) — same as NFS `NULL`.

### `MOUNTPROC3_MNT` (proc 1)

Args: `{ dirpath }` (XDR string, max `MNTPATHLEN = 1024`). Expect exactly
`"/export/" + 32 lowercase hex chars`. Malformed shape → `MNT3ERR_NOENT
(2)`. Correct shape, wrong secret → `MNT3ERR_ACCES (13)` (matches the
spike's observed `mount_nfs` "Permission denied, exit 13"). Match →
success.
Res: `union switch(mountstat3 fhs_status){ MNT3_OK (0): { fhandle3
fhandle; int auth_flavors<>; } default: void; }`. On success, `fhandle =
handle.encode(ROOT_INO)`, `auth_flavors = [1]` (`AUTH_SYS`).

### `MOUNTPROC3_UMNT` (proc 3)

Args: `{ dirpath }`. Res: `void`. Accepted unconditionally (single-export
server; real teardown happens at `Mount::unmount()`).

### `MOUNTPROC3_EXPORT` (proc 5)

Args: `void`. Res: single `bool=false` (empty export list) — cheap to
answer correctly.

`MOUNTPROC3_DUMP`/`UMNTALL` not implemented — fall through to the RPC
dispatcher's generic `PROC_UNAVAIL`, itself a well-formed response;
`mount_nfs` doesn't need them for this flow.

## 7. Server lifecycle (`mod.rs`, `server.rs`)

### `mount()`

1. `FileHandle3::new_random()`.
2. `TcpListener::bind("127.0.0.1:0")`, read the assigned port via
   `local_addr()`.
3. Spawn an accept-loop thread: `set_nonblocking(true)`, loop `accept()`
   with a short sleep (~50ms) checking a shared `Arc<AtomicBool>` stop
   flag; each accepted connection gets its own worker thread reading/
   dispatching RPC messages until EOF or a protocol error. `fs: Arc<F>`
   cloned into each connection thread (`ReadOnlyFs: Send + Sync` already
   required).
4. Run `mount_nfs` via `std::process::Command`:
   ```
   mount_nfs -o vers=3,tcp,port=<N>,mountport=<N>,noresvport,soft,timeo=20,retrans=2 \
       127.0.0.1:/export/<hex secret> <mountpoint>
   ```
   Nonzero exit → `FsError::Io`/`FsError::Other` with captured stderr as
   context.
5. Return `NfsHandle { mountpoint, stop: Arc<AtomicBool>, server_thread:
   Option<JoinHandle<()>> }`.

### `NfsHandle::unmount()`

Order matters: client-side mount torn down **first** (so no new request
can arrive), *then* the server stopped (so nothing is left in-flight to
hang on).

1. **Direct `unmount(2)` syscall**, not a `/sbin/umount` subprocess:
   `libc::unmount(mountpoint_cstr.as_ptr(), 0)` with a `// SAFETY:`
   comment (valid NUL-terminated `CString`, no flags, syscall doesn't
   retain the pointer past the call) — the same primitive `/sbin/umount`
   itself calls, permitted for a regular user unmounting their own mount
   (mirrors `fusermount3 -u`'s privilege model). `EBUSY` → `FsError::Io`,
   do not proceed to stop the server.
2. Set the stop flag, join the accept-loop thread (bounded by its own
   poll interval, so this returns promptly).
3. Drop the `TcpListener`, freeing the port.

`Drop` calls the same path, swallowing errors — matches `Mount`'s existing
contract.

### Crash/timeout behavior

Already covered by `soft,timeo=20,retrans=2` baked into step 4 above — a
mount-time decision, not a `MountBuilder` knob (`allow_other`/
`auto_unmount` are FUSE-specific concepts that don't apply here).

## 8. `examples/memfs.rs` fix

Add `parents: HashMap<Ino, Ino>` built alongside `nodes` (`ROOT_INO ->
ROOT_INO`, `Ino(4) -> ROOT_INO` for `subdir`), and handle `.`/`..`
explicitly in `lookup()` before the children-map fallback:

```rust
fn lookup(&self, parent: Ino, name: &OsStr) -> Result<FileAttr> {
    if name == OsStr::new(".") {
        return self.attr(parent);
    }
    if name == OsStr::new("..") {
        let p = *self.parents.get(&parent).ok_or(FsError::NotFound)?;
        return self.attr(p);
    }
    let Node::Dir(children) = self.node(parent)? else {
        return Err(FsError::NotADirectory);
    };
    let ino = *children.get(name).ok_or(FsError::NotFound)?;
    self.attr(ino)
}
```

This is the crate's reference implementation, so it should point at the
new `fs.rs` doc note (§9) explaining *why* — a real implementor copying
this pattern needs the same handling.

## 9. `fs.rs` doc addition

Add a subsection to `ReadOnlyFs`'s doc comment (near "Inode lifetime"):

> `.` and `..` — FUSE's kernel client never sends these names to
> `lookup`; it resolves them from its own dentry cache. NFS clients have
> no such cache and issue real wire `LOOKUP` calls for both.
> Implementations intended to work under the NFS backend should answer
> `lookup(dir, ".")` with `dir`'s own attributes and `lookup(dir, "..")`
> with the parent's, the way `examples/memfs.rs` does. `readdir` must
> still never return either — the backend synthesizes them, obtaining
> `..`'s target `Ino` the same way, via `lookup(dir, "..")`.

## 10. Manifest and dispatch wiring

**`Cargo.toml`**:
```toml
[features]
default = ["fuse", "cfapi", "nfs"]
...
# NFSv3 server via the built-in `mount_nfs` client (macOS only). Hand-rolled
# RPC/XDR over std::net — no new dependency; `libc::arc4random_buf` (already a
# macOS target dependency) supplies the per-mount secret. Inert off macOS.
nfs = []
```
No change to `[target.'cfg(target_os = "macos")'.dependencies]` —
`libc = "0.2"` already covers `arc4random_buf`.

**`src/mount.rs`** — add to `Backend`:
```rust
/// NFSv3 via the built-in `mount_nfs` client. macOS only.
Nfs,
```

**`src/backend/mod.rs`**:
- `#[cfg(all(target_os = "macos", feature = "nfs"))] pub(crate) mod nfs;`
- `MountHandle::Nfs(nfs::NfsHandle)` variant + `unmount()` arm.
- `mount()`: add the `Backend::Nfs` arm, cfg-gated to macOS+nfs.
- `auto_mount()`: add a macOS arm returning `MountHandle::Nfs(...)`; update
  the final `Unsupported` message.
- `unavailable()`: add `Backend::Nfs => FsError::Unsupported("the \`nfs\`
  backend requires macOS and the \`nfs\` feature")`.

**`src/lib.rs` / `examples/probe.rs`**: replace the macOS "no backend
compiled in yet" branches with a real probe — `any_backend_available()`
gains `|| cfg!(all(target_os = "macos", feature = "nfs"))`; `probe.rs`'s
macOS branch reports the backend as compiled in (nothing to probe at
runtime the way cfapi's platform-version check does — `mount_nfs` ships on
every Mac, state that plainly).

## 11. Docs

- **`docs/PLAN.md`**: close Phase 0.6 with a short "done" note (matching
  Phase 1's pattern), pointing at this build; update the "Feature flags"
  table's `nfs` row from "not implemented yet" to real/default-on.
- **`docs/GAPS.md`**: add "Handle cache — not implemented" (the v1
  per-`READ` open/release tradeoff, §5); note `onc-rpc` as the option if
  hand-rolled RPC framing needs replacing; note the unmeasured
  timing-side-channel posture on the secret comparison.
- **`README.md`**: NFS row moves from "decided, not implemented" to
  "working" (once verified), matching the FUSE row's phrasing.
- **`CLAUDE.md`**: update "What this is" (macOS now has a real backend)
  and the Architecture `backend/` bullet (add `nfs.rs`/`nfs/`); add an NFS
  entry to "Platform constraints worth knowing before editing a backend"
  (secret-in-handle auth model, `soft`/`timeo` rationale, single-fragment
  RPC limitation).
- **`.github/workflows/ci.yml`**: fix the now-stale "macOS has no backend
  in the tree yet" comments in the `build` and `cross-check` jobs.

## 12. New CI job: `nfs-mount-smoke-test`

Mirrors the Linux `mount-smoke-test` job, on `macos-latest` (unattended-
capable — no kext approval needed):

```yaml
nfs-mount-smoke-test:
  name: nfs mount smoke test / macos
  runs-on: macos-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - name: Mount memfs over NFS and verify contents
      run: |
        set -euo pipefail
        mkdir -p /tmp/anymount-nfs-ci
        cargo build --example memfs
        ( sleep 25 ) | cargo run -q --example memfs -- /tmp/anymount-nfs-ci &
        sleep 5
        mount | grep -F /tmp/anymount-nfs-ci
        test "$(cat /tmp/anymount-nfs-ci/hello.txt)" = "Hello from anymount!"
        got=$(shasum -a 256 /tmp/anymount-nfs-ci/numbers.txt | cut -d' ' -f1)
        want=$(seq 1 100 | shasum -a 256 | cut -d' ' -f1)
        test "$got" = "$want"
        test "$(cat /tmp/anymount-nfs-ci/subdir/nested.txt)" = "I am nested."
        find /tmp/anymount-nfs-ci | sort
        wait
        ! mount | grep -qF /tmp/anymount-nfs-ci
```

Note `shasum -a 256`, not `sha256sum` (not a default macOS binary). `find`
here also exercises `..`-driven navigation and `LOOKUP` correctness end to
end — the specific gap this task closes. Genuinely new coverage, not
redundant with `build`'s macOS leg (compiles only) or `cross-check`
(`check` only).

## 13. Unit/property test list

**`xdr_tests.rs`**:
- Round-trip tests for `u32`/`u64`/`bool`/`opaque_var`/`string`.
- `reader____truncated_input____returns_none_not_panic` for each
  primitive.
- Property test: `reader____arbitrary_truncated_prefix_of_any_valid_encoding____never_panics`
  — `proptest`-generate a valid encoding, re-decode every prefix length
  `0..full_len`, assert `None`/correct, never a panic.
- Property test: `opaque_var____arbitrary_bytes____roundtrips_through_write_then_read`.

**`handle_tests.rs`**:
- `resolve____encoded_by_same_handle____round_trips_to_original_ino`.
- `resolve____wrong_secret____is_none`.
- `resolve____wrong_length____is_none` (23- and 25-byte inputs).
- `resolve____all_zero_guess____is_none` (mirrors the spike's
  attack-simulation finding).
- Property test: `resolve____any_ino____round_trips`.

**`nfs_proto_tests.rs`**:
- `fattr3____from_file_attr____maps_size_perm_kind_correctly` (file and
  directory).
- `to_nfsstat3____every_fs_error_variant____maps_to_a_distinct_or_documented_status`.
- `write_op_rejection____every_mutating_proc____encodes_rofs_or_notsupp_with_correct_wcc_shape`
  — table-driven over §5's table, asserting exact body byte length.
- `readdirplus____budget_smaller_than_one_entry____returns_zero_entries_and_a_resumable_cookie`,
  `readdirplus____budget_covers_everything____returns_full_listing_with_eof_true`.
- Property test:
  `readdirplus____any_entry_count_and_maxcount_budget____reassembles_exactly_dot_dotdot_then_every_entry_in_order`
  — NFS-specific analogue of `fuse_tests.rs`'s `full_listing`, driving the
  real XDR-encoding budget check instead of the abstract simulation.

**`readdir_cookie_tests.rs`**: the existing `for_entry`/`trait_offset`
tests, moved verbatim from `fuse_tests.rs`, now compiling and running
unconditionally.

## 14. Verification plan (real, on this machine)

1. `cargo run --example probe` — confirm NFS reported available.
2. `mkdir -p /tmp/anymount-nfs-demo && cargo run --example memfs --
   /tmp/anymount-nfs-demo` (after the `.`/`..` fix).
3. From a second shell: `mount | grep anymount-nfs-demo` (type `nfs`,
   `soft`); `ls -lR`, `find`, `stat` at every nesting level, `cat` each
   file; `cd /tmp/anymount-nfs-demo/subdir && ls ../..` to specifically
   exercise `..` navigation; `shasum -a 256 numbers.txt` against `seq 1
   100 | shasum -a 256` computed independently.
4. `nfsstat -m` to confirm `soft,timeo=20,retrans=2` took effect.
5. Press Enter in the `memfs` process to trigger `Mount::unmount()`;
   confirm `mount` no longer lists the mountpoint, no stale entry.
6. Crash-recovery check: start `memfs`, `kill -9` it directly (bypassing
   `unmount()`), attempt a read from a shell with the mount open; confirm
   clean failure within ~6-20s (`Operation timed out`), not a hang or
   system dialog; then `umount /tmp/anymount-nfs-demo` manually and
   confirm success.
7. Extend `examples/memfs.rs` (or a throwaway local variant, not
   committed) with a directory large enough to force multi-call
   `READDIRPLUS` paging; confirm `ls`/`find` still lists everything
   correctly.

## Critical files

- `src/backend/fuse.rs` — structural pattern to mirror (module doc style,
  `mount()`/`*Handle` shape, cookie module, small helper functions).
- `src/backend/fuse_tests.rs` — proptest conventions to follow.
- `src/backend/cfapi.rs` — stub-backend doc-comment convention.
- `src/backend/mod.rs` — `MountHandle`/dispatch wiring (§10).
- `src/mount.rs` — `Backend` enum, `MountBuilder` (§10).
- `src/error.rs` — `FsError`, `to_errno`'s cfg-twin idiom to mirror (§4).
- `src/fs.rs` — `ReadOnlyFs` trait doc, `.`/`..` convention note (§9).
- `examples/memfs.rs` — reference implementation, needs the `.`/`..` fix
  (§8).
- `Cargo.toml` — new `nfs` feature (§10).
- `docs/PLAN.md`, `docs/GAPS.md`, `README.md`, `CLAUDE.md`,
  `.github/workflows/ci.yml` — doc/CI updates (§11, §12).

## Verification of the plan itself

Standard gates apply once built: `cargo test`, `cargo clippy --all-targets
-- -Dwarnings`, `cargo fmt --all -- --check`, `cargo deny check licenses
bans sources`, both cross-compile checks, `cargo +1.88.0 check
--all-targets` (MSRV) — plus the real-tools mount verification in §14,
which this machine can actually run end to end.
