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

This costs less than it sounds, because **cfapi calls `read_at` sequentially
during hydration** — it materialises whole files anyway. Only the FUSE path
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

## macOS: no native FSKit backend

FSKit is unusable from a Rust library: the entry point must be Swift, it must be
packaged as an Xcode-built `.appex` with entitlements and notarisation, and
`FSUnaryFileSystem` only supports filesystems that mount on a `/dev` node — not
synthetic ones.

*Mitigated by:* macFUSE 5.2+ ships its own FSKit backend, so on macOS 15.4+ the
mount is kernel-extension-free anyway, reached through the ordinary FUSE path.

## No async

`ReadOnlyFs` is synchronous. The first consumer's storage trait is synchronous
top to bottom, so an async trait could not be fed by it. Concurrency comes from
serving FUSE requests on multiple threads (`Config::n_threads`).

*To change:* add an `async` feature with a parallel trait and a bridging
adapter. Worth doing only when an async consumer exists.

## No `statfs` numbers by default

The default `statfs` reports zeroed counters, so `df` shows an empty
filesystem. Implementors that know their archive size should override it.
