# Known gaps

What `anymount` does not do, why, and what it would take to change.
Kept honest so downstream users hit no surprises.

## Read-only

Write operations report `EROFS`. The first consumer restores from immutable
backup snapshots, and ProjFS [cannot intercept writes at
all](https://github.com/microsoft/ProjFS-Managed-API/issues/30), so a
cross-platform write story would be uneven from the start.

*To change:* add write ops to `ReadOnlyFs` (or a `ReadWriteFs` supertrait) and
accept that ProjFS cannot implement them. Windows write support realistically
means WinFsp, which reintroduces GPLv3 — see below.

## No symlinks or hardlinks

`FileKind` has only `File` and `Directory`. ciphercask skips symlinks at backup
time, and neither ProjFS nor cfapi models them the way FUSE does.

*To change:* add `FileKind::Symlink` plus a `readlink` op; ProjFS has partial
support via symlink placeholders, but those require NTFS and error on ReFS.

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

This costs less than it sounds, because **ProjFS and cfapi call `read_at`
sequentially during hydration** — they materialise whole files anyway. Only the
FUSE path issues random reads.

*To change:* the archive format must record plaintext chunk offsets. The trait
does not change when it does.

## Windows: a directory, not a drive letter

Both ProjFS and cfapi project into a directory under a virtualisation root.
Neither can assign `X:`.

*To change:* WinFsp is the only Windows option that mounts a real volume — and
it is GPLv3 with a paid commercial license. It would have to live in a separate,
opt-in `anymount-winfsp` crate so it never enters a default dependency graph.

## Windows: ProjFS hydration consumes disk

Reading a file through ProjFS materialises it on local disk, with no automatic
eviction. Browsing a very large archive can fill the volume.

*Mitigated by:* using cfapi instead, whose `STREAMING_ALLOWED` policy avoids
persisting fetched data and whose `AUTO_DEHYDRATION_ALLOWED` lets Windows
Storage Sense reclaim space.

## Windows: ProjFS needs an admin feature-enable

`Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart`,
once per machine. No reboot. cfapi has no such step.

Because that feature is off by default, `ProjectedFSLib.dll` is frequently
absent, and a load-time import of a `Prj*` symbol would prevent the *entire
binary* from starting rather than yielding a catchable error. `anymount` never
references those symbols statically and probes with `LoadLibraryW`; any code
added to the ProjFS backend must resolve entry points dynamically to preserve
that.

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
