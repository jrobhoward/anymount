# NFS access control: options

Scope note: this file is a design note, not one of the six documents listed in
`CLAUDE.md`. It exists to hold the comparison behind a decision. Once a
decision is taken, the outcome belongs in `docs/ARCHITECTURE.md`, the residual
limitation in `docs/GAPS.md`, and this file can go.

All findings below were verified on one machine running macOS 26, Darwin
25.6.0, arm64. Nothing here has been checked on an earlier macOS.

## The problem

The macOS backend authorizes with a 128-bit per-mount secret that does two
jobs. It prefixes every file handle, and it is the first segment of the export
path that `MNT` checks. The export path reaches the system mount table, which
is not per-user, so the secret is published for the life of the mount:

```text
127.0.0.1:/export/d6450b78ce6d629f159eb7c0b262f06f/anymount-memfs on /private/tmp/anymount-demo
```

Any local account can read that and reach the server on loopback. Holding the
secret grants everything: mounting the content again elsewhere, or reading it
over the wire without going through the original mountpoint.

Two separate properties are at stake, and an option can fix one without the
other:

- Disclosure, meaning whether the credential appears on a surface another
  local user can read.
- Reachability, meaning whether another local user can connect to the server
  at all.

## Corrections to the current write-up

Two statements in `docs/GAPS.md` need adjustment before any option is weighed.

**The `ps` exposure does not extend to other users.** macOS restricts
`KERN_PROCARGS2` to the calling uid. Reading a root process's arguments as an
unprivileged user fails with `EINVAL`, and `ps` prints only the executable
path for a process owned by someone else. The export path on the `mount_nfs`
command line is therefore visible to same-uid processes and to root, not to
other accounts.

**The mount table is not the only surface.** `nfsstat -m` prints the file
system locations of every NFS mount, including the export path with the secret
in it. It carries the `com.apple.private.system-nfssvc` entitlement, is
world-executable, and is not setuid. Any option that leaves the credential in
the export path leaks it through this tool even if the mount table entry is
changed.

## What the mount ABI offers

macOS `mount_nfs` is not setuid and carries no entitlements, so `mount(2)` for
NFS is available to any user. Its argument buffer is an XDR blob whose layout
is declared in `<nfs/nfs.h>` behind `__APPLE_API_PRIVATE`. Three attributes in
that buffer matter here:

- `NFS_MATTR_FH` supplies the root file handle directly, so the MOUNT protocol
  never runs and no credential goes into a path.
- `NFS_MATTR_MNTFROM` sets `f_mntfromname` to a fixed string, independent of
  the connection parameters.
- `NFS_MATTR_LOCAL_NFS_PORT` and `NFS_MATTR_LOCAL_MOUNT_PORT` carry a Unix
  domain socket path, with a socket type of `ticotsord`.

`mount_nfs` exposes none of these three on its command line. Its option table
has no file-handle option and no mount-from option, and its local-socket
parsing is not reachable from the command line: a hostname is rejected with
"no usable addresses for host", and a socket path as the host hangs in
multicast DNS resolution. All three therefore share one prerequisite, which is
a hand-built argument buffer passed to `mount(2)`. The private interface is the
delivery mechanism, not any individual attribute.

## Options

| Option | Removes disclosure | Removes reachability | Public interface only | Failure mode |
|---|---|---|---|---|
| A. Document only | no | no | yes | none |
| B. Two-secret split | partly | no | yes | mount fails |
| C. Direct file handle | yes | no | no | mount fails |
| D. Fixed mount-from string | no | no | no | silent |
| E. Unix domain socket | yes | yes | no | mount fails |

### A. Document only

The status quo. The commit that added the caveat to `README.md`, `SECURITY.md`
and `docs/GAPS.md` is this option.

Cost is nothing. The limitation stands: content other local users should not
see cannot be mounted with this backend.

### B. Two-secret split

Draw two independent random values. A public value stays in the export path
and authorizes `MNT`. A private value, never written to the mount table,
prefixes file handles. `MNT` returns a root handle carrying the private value,
and the server stops honouring `MNT` once `mount_nfs` has exited.

This is the design already recorded in `docs/GAPS.md`, and its open question is
now answered. `MNT` is issued exactly once per mount lifetime. Dropping the
server's connection mid-mount causes the kernel client to reconnect on its own
and continue serving reads, listings and checksums without a second `MNT`.
Refusing later `MNT` calls does not break reconnection.

Two details go with it. `FileHandle3::cookieverf` returns the first eight bytes
of the secret and `nfs_proto.rs` writes it into every `READDIR3` reply, so it
would publish half the private value; it needs its own value or a constant.
Constant-time comparison in `FileHandle3::resolve` also stops being moot once
the handle secret is no longer readable from the mount table.

This is the only option that stays entirely on the supported interface. It
narrows the gap rather than removing it: the public value is still readable
from the mount table and from `nfsstat`, and a process polling for it can race
the legitimate client during setup.

### C. Direct file handle

Call `mount(2)` with a hand-built argument buffer, and pass the root handle in
`NFS_MATTR_FH`. The MOUNT protocol is not used, so `mount_proto.rs` and the
secret-in-path parsing can be deleted. The export path becomes decorative and
carries no credential.

Verified over ordinary TCP with an export path of `/anymount`. The mount
succeeded as an unprivileged user, directory listing and file reads worked, and
a checksum matched one computed independently. Neither the mount table nor
`nfsstat -m` contained the secret.

This closes disclosure on its own. It does not close reachability: the server
still listens on loopback, and any local process can connect to it. What such a
process cannot do is obtain a root handle, because there is no `MNT` to ask and
the handle secret is never published.

### D. Fixed mount-from string

Set `NFS_MATTR_MNTFROM` so the mount table shows a chosen name.

Verified over TCP. The mount table showed the chosen string. `nfsstat -m` still
printed the file system locations path with the secret in it.

This is presentation, not access control, and it must not be counted as a
mitigation. Its failure mode is the reason to keep it out of the security
argument: if a future release ignores the attribute, the locations path is
shown instead and nothing signals the change. Every other option here fails by
failing to mount. On the local transport the attribute is already ignored, and
the mount name is built from the socket path.

### E. Unix domain socket

Serve on an `AF_UNIX` socket instead of loopback, and mount with
`NFS_MATTR_LOCAL_NFS_PORT` plus a socket type of `ticotsord`. The socket path
must also appear as the file system locations address; setting the attribute
alone produces a timeout with no connection.

Verified end to end. The crate's own wire layer served a small filesystem over
a socket at mode 0600 inside a directory at mode 0700, mounted unprivileged
through `mount(2)` with the root handle supplied directly, with no MOUNT
protocol and no listening TCP socket anywhere on the machine. Listing and
reading through the mount both worked. The published mount name was the socket
path and a decorative export label, with no credential in either.

Access control becomes ordinary file permissions on the socket. Peer
credentials do not contribute: the kernel makes the connection and
`LOCAL_PEERCRED` reports uid 0.

Two constraints. `sun_path` is limited to 104 bytes on macOS, so the socket
cannot live under a long temporary directory. Reconnect behaviour over this
transport has not been exercised, unlike the TCP case.

### Ruled out

- **Reserved source port.** Requires root, and `mount_nfs` is not setuid.
  Unprivileged mounting is a property this backend is built around.
- **Peer credentials over TCP.** The kernel owns the client end of the
  connection, so there is no user process to identify. Confirmed with `lsof`,
  where only the server side names a process.
- **`RPCSEC_GSS`.** Requires a Kerberos realm and a GSS implementation at both
  ends.
- **Reading mount arguments back for validation.** The `NFS_MOUNTINFO` sysctl
  is entitlement-gated and returns `EPERM` without it.

Not evaluated: FSKit, Apple's userspace filesystem framework on macOS 15 and
later. It would replace the NFS mechanism rather than adjust it, and it brings
extension packaging and a higher OS floor. It is a 2.0-scale question.

## How risky the private interface is

The risk applies equally to options C, D and E, since all three need the same
argument buffer. Choosing the socket over the file handle alone does not change
the kind of exposure, only the degree.

The buffer's framing and the attributes a normal mount uses are exercised by
every NFS mount on every Mac. An encoder written against the header produced a
buffer byte-identical to the one the shipped client sends for an ordinary
mount, which covers the framing, the flags bitmaps, the version, the socket
type, the ports, the timeouts, the file system locations and the mount flags.
Two attributes needed here do not appear in that buffer, and the local-socket
attributes are the least exercised of all.

The header alone is not sufficient to get the encoding right. The mount flags
field is 64 bits, not the 32 that the surrounding attributes suggest. Nothing
in the header says so.

The mitigating property is that a wrong buffer fails loudly. A length read in
the wrong byte order returns `ENOMEM`, an over-long buffer returns `E2BIG`, and
a misaligned attribute stream returns `ENOMEM` when the kernel reads a string
length out of the wrong place. No silent partial mount was observed in any of
these cases.

## Recommendation

Use the private interface, but never depend on it. Two paths, tried in order.

**Primary: Unix domain socket with the root handle supplied directly.** This is
option E with option C inside it. It removes disclosure and reachability
together, and it makes the constant-time comparison gap moot, since no local
process can reach the server to time anything.

**Fallback: the current mechanism with the two-secret split.** This is option
B. It runs whenever the primary path fails for any reason, including a future
macOS that changes the argument encoding. It stays entirely on the supported
interface, and it is better than today's behaviour rather than merely equal to
it.

Option D is not part of the recommendation. It can be set on the primary path
for a readable mount name, and nothing may depend on it.

A middle tier of TCP with a direct file handle is not worth adding. It uses the
same encoder as the primary path, so it fails in exactly the same
circumstances, and it would add a third code path for no additional coverage.

### Plan

1. **Correct `docs/GAPS.md` first.** Remove the cross-user `ps` claim, record
   that `nfsstat -m` is a second disclosure surface, and record that `MNT` is
   issued once per mount and that refusing later calls does not break
   reconnection. This is independent of every option below and is worth doing
   whether or not the rest proceeds.
2. **Make the wire layer transport-agnostic.** `rpc::read_message` and
   `rpc::write_message` are already generic over `Read` and `Write`. Only
   `server.rs`'s accept loop and its two socket-option calls are TCP-specific.
   A small trait covering non-blocking mode and read timeout, implemented for
   both stream types, is enough. No public API changes.
3. **Add the argument encoder behind a unit test that pins the bytes.** Keep a
   recorded buffer from the shipped client as a fixture and assert equality for
   the attributes it covers. A change in the encoding that breaks the fixture is
   then visible without a mount.
4. **Implement the primary path.** Socket in a directory created at mode 0700,
   socket at mode 0600, path short enough for `sun_path`, root handle in the
   arguments, MOUNT program not served.
5. **Verify the mount rather than trusting the return code.** After mounting,
   check the root through the mountpoint and compare against what the
   filesystem reports. A mount that returns success but is wrong is the one
   failure the loud-failure property above does not cover.
6. **Implement the fallback and the split.** Two independent values,
   `cookieverf` taking its own value, `MNT` refused after `mount_nfs` exits,
   and constant-time comparison in `FileHandle3::resolve`.
7. **Decide what the fallback says.** A mount that lands on the fallback has
   weaker properties than one that does not. Surfacing which path was taken,
   rather than silently accepting either, is what lets a caller who cares
   refuse the weaker one.

The public API does not change. `Backend::Nfs` keeps its shape, no value type
gains a field, and no new dependency is needed. This is a patch or minor
release under the 1.0 freeze, with a `CHANGELOG.md` entry, and it needs a
`docs/GAPS.md` rewrite of the section it closes.
