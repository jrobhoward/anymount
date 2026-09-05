# Security policy

## Reporting

Report a suspected vulnerability through GitHub's private advisory form:
<https://github.com/jrobhoward/anymount/security/advisories/new>. Please do not
open a public issue for one.

Include what a report needs to be actionable: the platform and backend, the
version, and the smallest reproduction available. A first response should be
expected within a week.

## What is in scope

`anymount` is a library, so most of its attack surface belongs to whatever
embeds it. Three areas are the crate's own:

- The macOS NFS server. It binds to `127.0.0.1` on an ephemeral port, so any
  local process can connect to it. Authorization is a 128-bit per-mount secret
  embedded in every file handle and required as a path segment in `MNT`; there
  is no credential checking beyond that, by design (`docs/PLAN.md`, Phase 0.6).
  A way to read or enumerate a mount's contents without holding that secret is
  a vulnerability. So is a message that panics a server thread, hangs it, or
  makes it allocate proportionally to what a client claims rather than to what
  it sent.
- The FFI in `backend/cfapi.rs`. Memory unsafety reachable from a callback the
  platform invokes is a vulnerability.
- Path handling around the mountpoint. cfapi's unmount deletes the
  placeholders it created; anything that makes it delete something else is a
  vulnerability.

## What is not

- The non-constant-time comparison of the NFS handle secret. Known, documented
  in `docs/GAPS.md`, and bounded by the loopback binding. A measured timing
  attack against it would be a report worth making.
- Anything reachable only by an implementation of `ReadOnlyFs` itself. That
  code is trusted by construction: it is the filesystem being served.
- Denial of service by a local process that could equally kill the mounting
  process outright.

## Supported versions

The latest published 1.x release.
