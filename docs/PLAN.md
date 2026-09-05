# anymount — plan

## Context

`ciphercask` is an encrypted backup tool whose only restore paths today are
whole-file (`restore-file`) and whole-directory (`restore-dir`) extraction to
disk. Its `design.md:505` already lists *"FUSE Filesystem (under consideration):
Browse backups as virtual filesystem"*. The goal is to browse and read a backup
— local or over LAN — as an ordinary mounted directory on all three platforms.

No Rust crate spans those platforms behind one API; the nearest equivalent in
any language is Go's `cgofuse`. So `anymount` is a new general-purpose crate,
with `ciphercask` as its first consumer (gaining a `ciphercask mount`
subcommand).

The hard constraint is MIT OR Apache-2.0 with **no copyleft in the dependency
graph**, matching the `deny.toml` allow-list already used in `bgrt`.

### Findings that shaped this design

- **`Cask` is fully synchronous** (`ciphercask-cask-core/src/lib.rs:44`), and
  `LanCask` hides QUIC/tokio behind its own `Runtime` + `block_on`
  (`ciphercask-lan-client/src/lib.rs:148`). An async trait could not be fed by
  it → **sync-only**.
- **Backups are regular files only.** `tasks/backup/mod.rs:875` skips
  directories and symlinks; `FileInventoryData.file_absolute_path` is a flat
  `String` → the tree is **synthesised**, and no `readlink` op is needed.
- **Random-access reads are impossible today.** `BackupFileInventoryData` has
  `chunk_ids: Vec<ChunkId>` — order but no plaintext offsets — and FastCDC
  chunks vary in size. `get_chunk_stored_size()` returns the *compressed*
  size → v1 uses **materialise-on-open**.
- **WinFsp, macFUSE's framework/kext, Dokany and `windows-projfs` are copyleft
  or proprietary.** cfapi through Microsoft's own `windows` crate is MIT/Apache.
  FSKit is unusable from Rust directly (Swift-only entry point, Xcode-built
  `.appex`). *Correction from the initial write-up:* this originally also
  claimed FSKit was block-device-only; that turned out to be unconfirmed and
  probably wrong — macFUSE's own FSKit backend demonstrably mounts synthetic
  content — see `docs/GAPS.md`'s FSKit section. The Swift/appex packaging
  requirement is what actually rules FSKit out here, not a block-device
  limitation.

## Decisions

| Decision | Choice |
|---|---|
| Name | `anymount` |
| Licence | MIT OR Apache-2.0, enforced by `cargo-deny` |
| Structure | **Single crate.** See below |
| Async | **Sync-only** |
| v1 scope | **Read-only.** Write ops return `EROFS`; gaps in `GAPS.md` |
| Windows | **cfapi only.** ProjFS evaluated and cut entirely — not planned; **WinFsp excluded entirely** |
| macOS | **NFS**, a from-scratch unprivileged NFSv3 server mounted with the built-in `mount_nfs` client. FUSE (macFUSE) and WebDAV (`mount_webdav`) were both evaluated and set aside — not cut outright, but not the default; see Phase 0 |
| Linux | **FUSE**, via `fusermount3`. WebDAV considered and rejected — Linux's real-POSIX-mount WebDAV clients (`davfs2`, GVFS) are themselves FUSE underneath, so it would add a translation layer over FUSE, not replace it |
| Edition / MSRV | edition 2024, `rust-version = "1.88.0"` |

### Why one crate, not a `-core` split

A `-core` crate was considered and rejected. `ciphercask-cask-core` exists to
break a real cycle — `cask-fjall`, `cask-rocksdb` and `cask-sqlite` are separate
crates implementing `Cask`. `anymount` has no such structure: backends are
cfg-gated modules *inside* the crate, and ciphercask both implements
`ReadOnlyFs` and mounts it in one binary.

The only genuine future buyer is an isolated `anymount-winfsp`, and WinFsp is
excluded. Splitting later is mechanical and non-breaking if the facade
re-exports.

## Phases

### Phase 0 — three spikes (current)

Prove build *and* runtime dependencies on each OS before designing further.
`examples/probe.rs` reports availability without mounting; `examples/memfs.rs`
mounts a small in-memory tree.

**Linux — done.** Mounts unprivileged (`ro,user_id=1000`) via `fusermount3`,
`ls -lR` recurses, `stat` reports correct size/mode/type, random access is
correct (`dd skip=10 count=12` returns the right byte range), a SHA-256 taken
through the mount matches one computed directly, and unmount is clean.

One real constraint surfaced and is now handled: FUSE's `auto_unmount` requires
a non-`Owner` ACL, so it cannot combine with an owner-private mount.
`auto_unmount` therefore defaults **off** and returns an actionable error if
requested without `allow_other`.

Cross-compilation status, checked from Linux: the Windows target (`cargo check
--target x86_64-pc-windows-msvc`) type checks clean including clippy, so the
cfapi stub was known-good before touching a Windows box. macOS can only be
checked with `--no-default-features`, because `fuser`'s build script calls
pkg-config for libfuse and that needs a macOS sysroot; the `macos-latest` CI
runner covers the real thing.

**Windows — done.** Both questions resolved on a Windows 11 (build 26100)
unpackaged dev machine:

1. **Builds clean.** `cargo build --all-targets`, `cargo clippy --all-targets
   -- -Dwarnings`, `cargo fmt --all -- --check`, `cargo test`, and `cargo deny
   check licenses bans sources` all pass against `windows = "0.62"` with no
   changes needed. `cargo run --example probe` reports cfapi **available**,
   platform build 26100.8875, integration `0x628` — comfortably above the
   `0x310` floor `cfapi.rs` documents for unrestricted placeholder-management
   policies. (ProjFS was probed too, before it was cut — see below — and
   correctly reported unavailable: `Client-ProjFS` is off by default on a
   fresh machine, and `LoadLibraryW("ProjectedFSLib.dll")` failed as expected
   rather than crashing the process, confirming the dynamic-resolution
   strategy worked while it was in the tree.)
2. **`CfRegisterSyncRoot` works unpackaged.** A throwaway spike (built, run
   three times, then deleted — not part of the crate) called
   `CfRegisterSyncRoot` directly on a plain `cargo run` binary with no MSIX, no
   sparse package, and no app identity, registering a real sync root on a temp
   directory and unregistering it cleanly each time. The Win32 docs' claim of
   no package-identity requirement holds in practice, resolving the ambiguity
   with the WinRT `StorageProviderSyncRootManager` path (which *is*
   identity-gated and was correctly avoided).

   **Result: cfapi meets v1's Windows needs on its own, and ProjFS is cut
   entirely — not deferred, not an opt-in feature.** Checked for a genuine
   ProjFS capability advantage first and found none: ProjFS cannot intercept
   writes at all (cfapi at least has a callback path there, moot for v1's
   read-only scope either way — see `docs/GAPS.md`), both backends hydrate
   placeholder files through the same NTFS reparse-point/minifilter mechanism
   so `mmap` behaves the same on either, and both fetch callbacks
   (`PrjGetFileDataCallback` / `CF_CALLBACK_TYPE_FETCH_DATA`) receive an offset
   and length, so neither is uniquely capable of ranged reads. Every real
   difference (admin feature-enable, disk-filling hydration with no
   auto-eviction) favors cfapi, so there is no case left for carrying a second
   Windows backend. `src/backend/projfs.rs`, the `projfs` feature, the
   `Backend::ProjFs` variant, and the ProjFS-only `windows` crate features
   (`Win32_Storage_ProjectedFileSystem`, `Win32_System_LibraryLoader`) are all
   removed. If an environment ever turns up where cfapi genuinely doesn't
   work, that is a reason to re-add ProjFS from scratch with real evidence in
   hand, not a reason to have kept a stub around on spec.

**macOS — decided: WebDAV, not FUSE.** The FUSE path hit a real wall (points
1–4 below) and was paused by choice rather than worked around by lowering
boot security. That prompted a step back to ask what mechanism macOS itself
actually offers for this, the same question the Windows spike answered with
cfapi — see point 5: a hand-rolled, loopback-only WebDAV server mounted with
macOS's built-in `mount_webdav` client, with no macFUSE, no kext, no FSKit,
no Reduced Security, and no notarization, worked on the first real attempt.
Points 1–4 stay as the record of why FUSE was set aside on macOS, not deleted
— macFUSE isn't wrong or broken in general, it just needs a boot-security
tradeoff this project chose not to require. See "Decided" below point 5 for
the full per-platform rationale (Windows and Linux were checked too, not just
assumed to keep their existing mechanisms), and Phase 0.5 for what turning
the WebDAV spike into a real backend still needs.

1. **Builds clean.** `cargo build --all-targets`, `cargo test`, `cargo clippy
   --all-targets -- -Dwarnings`, `cargo fmt --all -- --check`, and `cargo deny
   check licenses bans sources` all pass with no source changes, once
   `pkg-config` can see macFUSE's `fuse.pc`. On this machine that file lands at
   `/usr/local/lib/pkgconfig/fuse.pc`, which is already on pkg-config's default
   search path (`pkg-config --variable pc_path pkg-config` includes it) — no
   `PKG_CONFIG_PATH` override needed. Without macFUSE installed, the build
   fails in `fuser`'s build script with "Package fuse was not found in the
   pkg-config search path."
2. **Kernel extension cannot be approved through ordinary System Settings on
   Apple Silicon.** `cargo run --example memfs -- /tmp/anymount-demo` fails
   with `Io(Custom { kind: Other, error: "Unspecified Error" })`.
   `systemextensionsctl list` shows 0 loaded extensions throughout. The first
   mount attempt after a fresh install did produce a real signal in the
   system log — `kernelmanagerd`/`syspolicyd` explicitly resolving and then
   rejecting the load (`Kernel Extension BLOCKED: ... not approved to load.
   Please approve using System Settings`) — but there is no System Settings
   surface to act on that rejection on this machine: there is no "Driver
   Extensions" row under Login Items & Extensions, and nothing appears under
   Privacy & Security either. That's expected, not a bug in this crate's
   understanding: on Apple Silicon, third-party kernel extensions need the
   machine rebooted into **Recovery Mode**, **Startup Security Utility**
   opened, the boot security policy lowered from "Full Security" to
   **"Reduced Security,"** and **"Allow user management of kernel extensions
   from identified developers"** checked — only then does an approval toggle
   exist in ordinary System Settings at all. This is a boot-security-policy
   change, not a click-through, so it was deliberately not done as a side
   effect of getting a dev environment working; see point 4.
3. **The "no kernel extension" path does not work at all on this machine —
   platform bug, not a `fuser` limitation.** The README and this plan
   originally assumed macFUSE 5.2+'s FSKit backend (`-o backend=fskit`) would
   apply here. Investigation went two levels deep and both dead-end:
   - `fuser` 0.18 mounts macOS exclusively through the legacy
     `fuse_mount_compat25` entry point, which does not forward
     `backend=fskit` to macFUSE's FSKit machinery.
   - Tracing macFUSE's own public source shows this isn't a gap `fuser` could
     close by itself: the open C library has no knowledge of `backend=fskit`
     at all and always shells out to a closed-source helper, `mount_macfuse`,
     for both the kext and FSKit paths. That helper's source isn't published.
   - More fundamentally: macFUSE's official, signed FSKit module cannot be
     enabled on this machine at all (macOS 26.6.2, build 25G83). The System
     Settings toggle for it is disabled outright, and `fskitd` logs `Received
     error '(null)', errno 2, retrieving team ID` on every attempt — it can't
     resolve a Developer Team ID for the module, so it never reaches an
     allow/deny decision. This matches the shape of an open upstream bug,
     [`andrewgazelka/loaf#1`](https://github.com/andrewgazelka/loaf/issues/1)
     ("FSKit third-party extensions broken on macOS 26"), reported against
     earlier builds with a different but related symptom (explicit
     entitlement denial rather than a team-ID lookup failure). A from-scratch
     FSKit filesystem (e.g. `KhaosT/FSKitSample`) would very likely hit the
     same `fskitd` wall once built, independent of `fuser` or any Rust
     bridging. See `docs/GAPS.md` for the full trace, including what was
     checked in macFUSE's public repos.

   Kext approval is therefore the only working path on this machine today,
   and that conclusion doesn't depend on this crate's design — it's a current
   macOS platform bug blocking third-party FSKit modules generally.
4. **Paused rather than lowering boot security.** Getting a working FUSE
   mount on this machine means accepting the Reduced Security tradeoff in
   point 2 — a standing change to the machine's boot-time security posture,
   not scoped to this one crate. That's a call worth making deliberately, not
   as a byproduct of a spike, so this was stopped here on request rather than
   pushed through. Options for whoever picks this back up: lower this
   machine's boot security policy, spike on a different machine/VM that
   already runs Reduced Security, or wait and retry the FSKit path once
   `andrewgazelka/loaf#1` (or an equivalent report) shows Apple has fixed
   `fskitd`'s handling of third-party modules.
5. **WebDAV, mounted with macOS's built-in `mount_webdav`, works today —
   no FUSE dependency at all.** Prompted by the FUSE dead end, the question
   became: what does macOS itself provide for this, the way cfapi answered it
   on Windows? `mount_webdav` ships in `/sbin` on every Mac. A throwaway
   ~180-line, zero-dependency Rust server (`std::net::TcpListener`, no crate,
   not part of this repo) implementing just enough WebDAV — `OPTIONS`,
   `PROPFIND`, `GET`, `HEAD` — to serve a fixed two-file tree, bound to
   `127.0.0.1` only, mounted cleanly on the first attempt:
   `mount_webdav http://127.0.0.1:<port>/ /tmp/webdav-demo` exited 0, and
   `mount` showed `(webdav, nodev, noexec, nosuid, read-only, mounted by
   jhoward)` — unprivileged and read-only with no extra flags needed.
   Verified with real tools, same bar as the Linux spike: `ls -la` listed
   both files with real sizes, `cat` returned correct content for both,
   `stat` succeeded, a `sha256sum` taken through the mount matched one
   computed independently, Finder (`osascript ... reveal`) showed it as a
   mounted disk, and `umount` was clean. None of macFUSE, a kext, FSKit, a
   Reduced Security boot policy, or a notarized/Developer-ID-signed app were
   involved anywhere in this path — the server is just a loopback network
   service, not a macOS extension of any kind.

   Real gaps before this is more than a spike: no auth (anything on the
   machine can currently reach the loopback port while the mount is live —
   WebDAV over HTTP can carry a per-mount token or Basic Auth, which should
   be designed in from the start given this crate's content is decrypted
   backup data, not bolted on later), the hand-rolled HTTP parsing needs
   replacing with a real crate, only a fixed two-file tree and a subset of
   PROPFIND (`Depth: 1` on the root, no arbitrary nesting) were exercised, and
   file metadata (timestamps, permission bits) through the WebDAV client
   looked synthetic rather than mapped from real `FileAttr` values.

**Revised decision: macOS uses NFS, not WebDAV, not FUSE. Linux keeps FUSE.
Windows keeps cfapi.** One mechanism per platform, not one mechanism
everywhere — each platform gets whichever native mechanism actually fits it,
the same logic that chose cfapi over ProjFS on Windows.

macOS went through two choices before landing here, both recorded in full
below because the reasoning matters, not just the outcome:

1. FUSE was set aside first — its only working path on this machine needs
   Apple Silicon's Reduced Security boot policy (points 1–4 below), and both
   kext-free alternatives (macFUSE's own FSKit backend, a from-scratch FSKit
   filesystem) are blocked by a current macOS platform bug (point 3, and
   `docs/GAPS.md`).
2. WebDAV, mounted with the built-in `mount_webdav`, was chosen next and
   spiked working end to end (point 5) — until a follow-up spike found that
   Finder navigating into a folder downloads the full content of *every*
   file in it, not just what the user opens or copies (Phase 0.5, second and
   third bullets). For a restore browser whose primary interaction is
   exactly Finder-based folder browsing, that's a materially worse cost
   profile than FUSE's lazy per-request reads, not a rough edge — and no
   mitigation was found (`.metadata_never_index` doesn't help; neither does
   Finder's list view, tried when the same question came up for NFS below).
3. **NFS was spiked as a third option specifically to answer: is the
   eager-download behavior general to network filesystems on macOS, or
   specific to `webdavfs`?** It's specific to `webdavfs`. A minimal,
   unprivileged NFSv3 server (hand-rolled RPC/XDR over TCP, no root anywhere
   — `mount_nfs -o vers=3,tcp,port=<N>,mountport=<N>,noresvport`, confirmed
   the mount syscall itself doesn't require privilege, only macOS's own
   system-wide `nfsd` does) served the same five-file test folder Finder was
   tested against for WebDAV. Finder rendered the folder and issued **zero**
   `READ` RPCs — confirmed twice (automated reveal, and a follow-up manual
   `ls -la`), with a deliberate `dd` read immediately afterward proving the
   request logging itself was working, not silently broken. Content
   correctness was verified the same way as every other spike: byte-exact
   reads at arbitrary offsets against a deterministic pattern. NFS gets
   everything WebDAV was chosen for (no macFUSE, no kext, no FSKit, no
   Reduced Security, no root) without the cost that put WebDAV back in
   question.

This is why the decision keeps changing rather than being reversed once:
each choice was made on the evidence available at the time, and revised
specifically when a follow-up spike changed that evidence — not by default,
and not without recording why the earlier choice looked right until it
didn't.

The alternative — WebDAV uniformly, or at least investigated for the other
two platforms — was checked and rejected on the evidence, not by default:

- **Windows** can programmatically mount WebDAV (`WNetAddConnection2` /
  `DavAddConnection` against a UNC path like `\\127.0.0.1@<port>\path`, no
  GUI wizard needed), but the WebClient redirector service is disabled or
  manual-start on most Windows editions, and starting it as a normal user
  needs an undocumented ETW-trigger workaround, not a supported API call.
  That's real added fragility for no gain over cfapi, which this crate
  already has working, needs no service to start, and was purpose-built for
  exactly this (on-demand virtualization, not general file sharing).
- **Linux**'s real-POSIX-mount WebDAV clients are themselves FUSE-based —
  `davfs2` is a FUSE filesystem by its own description, and GVFS's
  `gio mount dav://...` only becomes visible to ordinary POSIX processes
  (`cat`, `ls`) through GVFS's own FUSE daemon under
  `/run/user/$UID/gvfs/`. Routing Linux through WebDAV would mean
  `anymount`'s server → WebDAV → `davfs2` → FUSE: strictly more moving parts
  than today's direct FUSE backend, for no benefit, since that backend
  already mounts cleanly and unprivileged.
- **macOS** is the one platform where WebDAV is a genuine, different,
  non-FUSE kernel-level path (`mount_webdav`), and the one platform where
  FUSE has real, current friction (Apple Silicon's Reduced Security
  requirement, and both FSKit alternatives to it currently broken — see
  `docs/GAPS.md`). WebDAV earns its place here specifically because the
  problem it solves only exists here.

### Phase 0.5 — macOS WebDAV: what the spike didn't cover (historical — WebDAV is no longer the pick)

Kept in full because the reasoning matters: this is the record of *why*
WebDAV was set aside in favor of NFS (see "Revised decision" above,
point 3), not just a note that it was. The Finder-browsing finding here is
what triggered the NFS spike in Phase 0.6.

The spike (previous section, point 5) proved the mechanism works; it did not
build anything close to a real backend. Before `backend/webdav.rs` could
exist alongside `backend/fuse.rs` and `backend/cfapi.rs`, this would have
needed resolving — some of it design work, some of it further spiking:

- **Auth.** The spike had none — anything on the machine could reach the
  loopback port while mounted. Needs a per-mount random token or Basic Auth
  designed in from the start, not bolted on later, given the content behind
  this crate is decrypted backup data.
- **Random-access reads over HTTP — spiked, and the answer is "both at
  once."** A second spike extended the server with a 32 MiB file of
  deterministic content (`byte[N] == N % 256`, so any offset's expected byte
  is checkable) and full `Range` support (206 Partial Content, correct
  `Content-Range`), then read from three far-apart offsets through the mount
  without ever reading the file sequentially. Two behaviors happen together,
  not one or the other:
  - `mount_webdav` genuinely issues scoped `Range: bytes=<offset>-<offset>`
    GETs for arbitrary offsets — confirmed with correct data at offset
    1,000,000 and 16,000,000 (`16000000 % 256 == 0`, matched exactly). This
    is real random access, not an assumption.
  - It *also* opportunistically starts an unranged, whole-file `GET` in the
    background the moment a file is first touched — a readahead/local-cache
    strategy. In the clean test, the first read's whole-file GET was aborted
    partway through (`Broken pipe` on the server side, after the client had
    what it needed from the front of the stream); the second read's
    whole-file GET ran to completion in the background *while* a separate,
    immediate `Range` GET satisfied the actual read; and the third read (near
    the end of the file) hit the server **not at all** — served from the
    local cache the completed background download had already populated.

  Consequences for `backend/webdav.rs`: `read_at(offset, len)` needs to serve
  correct data at arbitrary offsets (matches `ReadOnlyFs`'s existing shape,
  no trait change needed), and the server must treat a client dropping a
  connection mid-transfer as routine — not an error to propagate — since an
  aborted background full-download is expected behavior here, unlike FUSE.
  No `anymount`-side materialise-on-open cache is needed the way cfapi
  requires one; `mount_webdav`'s own local caching already does that job.

  **The eager full-fetch on open is unconditional and confirmed
  unsuppressible from the server — but it isn't the cost it first looked
  like.** Read Apple's actual `webdavfs` source
  ([`apple-oss-distributions/webdavfs`](https://github.com/apple-oss-distributions/webdavfs),
  `mount.tproj/webdav_network.c`): `stream_get_transaction()` always starts a
  sequential download from byte 0 on `open()`, driven by the response status
  code of its own requests (`200` = start over, `206` = resume appending at
  the cache file's EOF, `304` = cached copy still valid) — not by anything a
  server can advertise. There's a real cache-size constant
  (`webdavCacheMaximumSize`, physical RAM ÷ 4), but it only gates whether the
  local cache file bypasses the unified buffer cache, not whether the fetch
  happens. Confirmed empirically too: `Cache-Control: no-store` on every
  response made no difference. No `Accept-Ranges`, `Content-Length`, `ETag`,
  or `Last-Modified` handling influences fetch strategy anywhere in the
  source. This is unconditional, client-side behavior with no server-side
  opt-out.

  That said, it costs nothing for this crate's actual shape of use.
  **Browsing a directory never touches file content at all** — `PROPFIND`
  (what lists a folder) never opens or fetches anything, so listing a
  multi-GB archive's tree is free regardless of this behavior. **Copying a
  file out — the other half of "browse and copy back to the original
  filesystem," and the primary way this gets used — needs the whole file's
  content anyway**, so a full sequential fetch on open is exactly the right
  work, not wasted work. And it isn't a new cost this backend choice
  introduces: it matches the materialise-on-open pattern already accepted
  for cfapi, and more fundamentally already required by ciphercask's own
  archive format (FastCDC chunks with no recorded plaintext offsets — see
  "No random-access streaming from chunked archives" in `docs/GAPS.md`) —
  `ReadOnlyFs`'s own implementation likely has to materialise-on-open
  regardless of which backend serves it.

  What the eager fetch *does* cost, pinned down by three follow-up spikes,
  and it is a real, significant cost for this crate's primary workflow —
  Finder browsing is the expected way ciphercask's users will navigate a
  restore, not an edge case:
  - A clean mount, left alone for 8 seconds with no `ls` and no Finder
    involvement, produced **zero** GETs to a 32 MiB test file. Spotlight's
    metadata-marker probes (`.metadata_never_index`,
    `.metadata_never_index_unless_rootfs`, `.Spotlight-V100`, and friends)
    are lightweight existence checks that never touch file content by
    themselves.
  - Revealing that same mount in Finder (`osascript ... reveal`, no explicit
    read at all) produced a real, unranged, full `GET` for the test file
    every time — Finder's own icon/QuickLook thumbnail generation, not
    `mdworker` content indexing.
  - **Extended to a five-file directory (two 8 MiB, one 4 MiB, one 2 MiB, one
    64 KiB) to check whether this is scoped to one file or the whole
    folder: every single file was fetched in full, unranged, just from
    Finder displaying the folder** — some files were fetched twice (likely
    one pass for the icon, a second for a QuickLook/preview-pane pass). This
    is not "Finder sometimes opens a file it's curious about" — navigating
    into a directory downloads the complete content of everything visible in
    it, proportional to total folder size, not to anything the user actually
    asked to see or copy.
- **Suppressing it via `.metadata_never_index` — spiked, does not work.**
  Reporting that marker as present (a `200`/`207` response instead of `404`)
  was tried as a targeted fix, on the theory that it was Spotlight indexing
  driving the downloads. It isn't: with the marker present, Finder still
  fetched every file in the test folder in full on reveal — no improvement.
  `.metadata_never_index` is specifically a Spotlight/`mdworker` opt-out;
  Finder's icon/QuickLook thumbnail generation is a different subsystem it
  doesn't reach. No server-side WebDAV-level mitigation for this was found.
  Options not yet tried: whether a mount option (`-o nobrowse`, hiding the
  volume from Finder's browse UI — though this likely just stops it
  appearing in the sidebar, not what happens once the user navigates in) or
  a Finder view mode (list view instead of icon/gallery view — a user
  preference, not something this crate controls) avoids it. Absent a fix,
  this is a real, unresolved cost specifically for Finder-GUI browsing
  (icon or gallery view); plain directory listing (`PROPFIND`) and `ls`/`cp`
  from a terminal are unaffected, and list view is unconfirmed either way.
  **This needs weighing seriously before committing further to WebDAV as the
  macOS backend** — for a restore browser whose whole point is Finder-based
  navigation of a potentially large backup archive, "every folder view
  downloads everything in it" is a materially different cost profile than
  FUSE's genuinely lazy, read-what's-asked-for model, not a minor rough edge.
- **Arbitrary trees.** Only a flat two-file root was served. Nested
  directories, `PROPFIND` at `Depth: 0` (single resource) as well as
  `Depth: 1`, and correct XML-escaping of names all need exercising.
  `Depth: infinity` should be checked too — some WebDAV clients send it, and
  the server needs a defined, bounded response rather than surprise recursion.
- **Attribute mapping.** File timestamps and permission bits looked
  synthetic through the client; needs real mapping from `FileAttr`
  (`getlastmodified`, `creationdate`, `resourcetype`, `getcontentlength`).
- **A real HTTP/WebDAV crate,** in place of the spike's hand-rolled
  `std::net::TcpListener` parsing — chosen against this crate's MIT/Apache,
  no-copyleft constraint (`cargo deny check licenses` gates it, same as every
  other dependency).
- **Process lifecycle.** What happens if the server task dies while
  `mount_webdav` still has the volume mounted — does macOS hang reaching a
  dead loopback port, or fail visibly? `Mount`'s existing "unmounts on drop"
  contract (`mount.rs`) needs to cover shutting the server down cleanly too,
  not just unmounting.
- **Port allocation.** The spike bound `127.0.0.1:0` (OS-assigned ephemeral
  port) and passed the resolved port to `mount_webdav` — this pattern is
  fine and should carry forward, including for multiple concurrent mounts
  from separate processes.
- **Confirm read-only is enforced by the protocol, not just by convention.**
  The spike's `Allow` header never listed `PUT`/`DELETE`/`MKCOL`; verify a
  client attempting one of those gets a clean rejection (`405`) rather than
  hanging or being silently accepted.

### Phase 0.6 — macOS NFS: what the spike didn't cover

The NFS spike (see "Revised decision," point 3) proved the mechanism and
confirmed it avoids WebDAV's Finder-browsing cost; it's a smaller, more
targeted spike than the WebDAV one and covers less ground. Before
`backend/nfs.rs` exists, this needs resolving:

- **Auth — spiked, and `AUTH_SYS` is confirmed worthless, but file handles
  can be turned into real capability tokens without it.** NFSv3's default
  security flavor, `AUTH_SYS`, trusts client-supplied UID/GID with no
  cryptographic verification. Confirmed directly: parsing (not just
  skipping) the credential on every RPC call showed the real, unprivileged
  `mount_nfs` invocation used throughout this spike claims **`uid=0 gid=0`**
  — root — despite never running with elevated privilege at the OS level.
  `AUTH_SYS` proves nothing and was treated as decorative from that point on.

  Instead, the spike embedded a **per-run random 128-bit secret directly in
  the NFS file handle** (`fh_bytes`/`resolve`: handle = 16-byte secret ||
  4-byte index, checked on every request) and required that same secret as
  a literal path segment in `MNT` (`mount_nfs ... 127.0.0.1:/export/<hex
  secret>`) — the NFS analogue of a per-mount token in a WebDAV URL. Tested
  end to end:
  - Mounting with the correct secret works exactly as before (regression:
    `ls -la`, `dd` at an arbitrary offset, byte-exact).
  - Mounting with a wrong secret is rejected — `mount_nfs` itself reports
    **`Permission denied` (exit 13)**, and the server logs the rejected path.
  - **Bypassing `MNT` entirely and sending a raw, hand-crafted `GETATTR`
    RPC directly at the NFS program** — simulating a local attacker who
    knows the protocol but not the secret — was rejected (`NFS3ERR_NOENT`)
    both for the *old* 4-byte sequential-index handle format this spike
    started with, and for a naive all-zero 20-byte guess. A control run
    using the real secret in the same crafted-RPC path succeeded, proving
    the rejections were real (the secret mismatch), not an artifact of the
    attack tool being malformed.
  - **Checked whether the secret leaks the obvious way — process listing —
    and it doesn't.** `mount_nfs`'s `argv` (which contains the secret as
    part of the mount URL) was already unavailable via `ps aux` (shown as
    `(mount_nfs)`) by the time the process was even observable, apparently
    cleared deliberately and quickly. This is a real, favorable difference
    from `webdavfs_agent`'s WebDAV mount, which is a long-lived daemon whose
    invocation (including the mounted URL) stays visible for the mount's
    whole lifetime.

  Net: `sec=krb5` is unnecessary for this crate's threat model. A per-mount
  random secret carried in the export path and checked in every returned
  file handle gives real, tested access control independent of `AUTH_SYS` —
  demonstrably not exploitable by handle-guessing (both the old sequential
  scheme and a naive all-zero guess were rejected) or by reading the secret
  back out of `ps` (`mount_nfs`'s argv is gone by the time it's observable).
  The residual risk is the generic one any loopback service on a
  multi-user machine has — another local process, present at the moment a
  legitimate mount happens, could in principle capture the secret off the
  wire before the TCP connection's contents are otherwise protected; this
  spike didn't attempt to measure that specifically, and it applies
  identically to the WebDAV path this replaced. Not a new weakness
  introduced by choosing NFS.
- **Only `NULL`, `MNT`/`UMNT`, `GETATTR`, `LOOKUP`, `ACCESS`, `READ`,
  `READDIR`/`READDIRPLUS`, `FSSTAT`, `FSINFO`, and `PATHCONF` are
  implemented** — enough for `mount_nfs`, `ls -la`, `cat`, `dd`, and Finder
  browsing to work, verified with real tools the same as every other spike
  (byte-exact reads at arbitrary offsets, correct sizes, clean mount/unmount
  table entries). Not implemented at all: `SETATTR`, `WRITE`, `CREATE`,
  `MKDIR`, `REMOVE`, `RMDIR`, `RENAME`, `LINK`, `SYMLINK`, `MKNOD`, `COMMIT`
  — fine for a read-only crate, but the server needs to actively reject
  these (a clean NFS3ERR, not silence or a hang) rather than have them fall
  through to the spike's generic unhandled-proc fallback, which returns a
  malformed response today.
- **Arbitrary trees — spiked, works.** Rebuilt the spike's flat five-file
  root as a small arena-tree (`NodeId` = index into a `Vec<Node>`, each node
  a `Dir(Vec<(name, NodeId)>)` or `File { len, seed }`, two levels deep:
  `/fileRoot.bin`, `/dirA/fileA1.bin`, `/dirA/dirA_sub/fileA1sub.bin`,
  `/dirB/fileB1.bin`, `/dirB/fileB2.bin`) and re-tested the same way as
  every other spike, real tools only:
  - `find` recursed the whole tree correctly through both nesting levels.
  - `ls -la` at each level, and navigating back up via a literal `../..`
    path, all resolved correctly — `LOOKUP` now handles `.` and `..`
    explicitly (`.` → the directory itself, `..` → its stored parent id),
    and `READDIR`/`READDIRPLUS` list them first, matching real NFS server
    convention (WebDAV's `PROPFIND` has no equivalent — this was new
    surface area, not a port of the WebDAV spike's approach).
  - A `dd` read at an arbitrary offset into the most deeply nested file
    (`/dirA/dirA_sub/fileA1sub.bin`) returned correct bytes, and a
    `sha256sum` of the largest nested file (`/dirB/fileB1.bin`, 2 MiB)
    matched one computed independently.
  - The secret-in-handle scheme from the auth spike generalizes for free:
    `fh_bytes`/`resolve` now carry a `NodeId` instead of a flat-array index,
    so every node in the tree — file or directory — gets the same
    16-byte-secret-prefixed handle, no separate handling needed.

  `Depth: infinity`-style unbounded recursion isn't a concept NFS's
  `LOOKUP`-per-component model has, so unlike the WebDAV spike's open
  question there, nothing further to check on that front.
- **`READDIR`/`READDIRPLUS` cookie-based paging — spiked, and the first
  version was quietly wrong before it was fixed.** A 3000-entry directory
  (`dirBig`, tiny 16-byte files) was added to the tree specifically to force
  real client-side paging — a real client's `count`/`maxcount` budget per
  call is far smaller than what 3000 entries encode to. The *original*
  version of this spike (same one from the "arbitrary trees" round) ignored
  `cookie` and the size budget entirely and dumped every entry into one
  reply regardless of what was asked; tested against `dirBig`, `ls` still
  returned all 3000 names correctly in a single, oversized `READDIRPLUS`
  reply — because NFS-over-TCP has no hard per-message size ceiling the way
  NFS-over-UDP's ~64 KiB datagram limit would enforce, so `mount_nfs`
  tolerated it. That's a real trap: it means the shortcut *looked* correct
  under this exact test and would have shipped broken — no per-entry size
  ceiling, no resumability, wrong for any client stricter about `maxcount`
  and wrong the moment a directory got too large for one in-memory reply to
  be reasonable at all.

  Rewritten to honor the protocol for real: `cookie` in a request resumes
  from that position (cookie *N* means "after the entry with cookie *N*"),
  and each entry is encoded into a scratch buffer first so the *actual* XDR
  size — not a guessed constant — is checked against the caller's `count`
  (`READDIR`) or `maxcount` (`READDIRPLUS`) budget before being added, with
  `eof` set correctly based on whether the full listing was covered. Tested
  the same way as every other spike: `ls dirBig` still returns all 3000
  entries (verified unique, first/middle/last spot-checked present, and
  content of a sampled entry byte-correct) — but now via **29 separate
  `READDIRPLUS` calls**, `cookie_in` advancing exactly by each call's
  returned count (0 → 16 → 231 → 446 → …), `maxcount_budget=32768`
  genuinely constraining each reply to ~215 entries, and the final call
  correctly reporting `eof=true` after all 3000 (root's `.`/`..` push the
  true count to 3002) were accounted for.
- **Real inode-backed file handles — spiked, works, and surfaced a genuine
  trait gap.** The spike was rebuilt from the ground up on `anymount` as an
  actual `path` dependency (`Cargo.toml`: `anymount = { path = "...",
  default-features = false }`, so no `fuser`/`windows` pulled in) — every
  NFS operation now goes through a real `Fixture: ReadOnlyFs` (`fixture.rs`
  in the spike), not a private arena. File handles are `16-byte secret ||
  8-byte Ino` (24 bytes total, `Ino` being the crate's actual type), and
  `write_fattr3` maps directly from a real `FileAttr` — which resolves the
  "attribute mapping is synthetic" gap from the previous round as a
  side effect: real `dr-xr-xr-x`/`-r--r--r--` (`0o555`/`0o444`, from
  `FileAttr::dir()`/`file()`'s actual defaults) and real epoch timestamps
  showed up in `ls -la` output, not hand-picked constants. Every previous
  verification was re-run against this real trait and still passes: `find`
  and `ls -la` through the nested tree, `..` navigation, a `dd` at an
  arbitrary offset into the most deeply nested file (byte-exact), a
  `sha256sum` of a 2 MiB file matching the *exact same* digest as the
  previous round's private-arena version, cookie-based paging over the
  3000-entry directory (still 19 real `READDIRPLUS` calls, cookies
  advancing correctly, `eof` correct), and the secret-in-handle auth
  bypass rejections (both the old handle format and a fresh 24-byte
  all-zero guess correctly rejected with `NFS3ERR_NOENT`).

  **The genuine finding: FUSE's kernel client never sends `.` or `..` to
  `lookup()`, but NFS's wire protocol does — and the trait has no way to
  answer `..` at all.** Confirmed by reading `backend/fuse.rs`'s existing
  `readdir` synthesis: it reports the *current* directory's own `ino` for
  both the `.` and `..` entries it hands the kernel
  (`reply.add(ino, 1, ..., ".")` and `reply.add(ino, 2, ..., "..")`,
  `fuse.rs:171,178`) — which would be wrong for `..` on any directory but
  the root, except the Linux/macOS kernel VFS resolves `..` from its own
  dentry cache and never actually trusts that reported value, so the bug
  is invisible. NFS clients have no such local cache to fall back on; they
  issue real `LOOKUP` calls for `.` and `..` over the wire and need correct
  answers. **`examples/memfs.rs`'s `lookup()` does not handle either name
  today** — checked directly, it looks the name up in its children map and
  returns `NotFound` for both — so it would break under a real NFS backend
  as-is, not hypothetically.

  The workaround used here, and the one to document as guidance rather than
  fix at the trait level: nothing forbids an implementor's own `lookup()`
  from answering `.` (return `getattr(self)`) and `..` (return the parent's
  attributes) itself — the spike's `Fixture` does exactly this. The NFS
  backend layer then needs zero special-casing in its own `LOOKUP` handler,
  forwarding every name verbatim; and for `READDIR`/`READDIRPLUS`, where the
  trait's contract already requires the *backend* (not the implementor) to
  synthesize `.`/`..`, the backend obtains `..`'s target `Ino` by calling
  `fs.lookup(dir, "..")` — the same mechanism `LOOKUP` uses, so no separate
  parent-tracking is needed anywhere in `backend/nfs.rs` either. This only
  works if implementors adopt the convention; it isn't enforceable by the
  trait as written. Worth an explicit doc note on `ReadOnlyFs::lookup`
  once `backend/nfs.rs` is real, and worth deciding whether
  `examples/memfs.rs` should be fixed to match given it's the crate's own
  reference implementation.

  Also confirmed, not just assumed: **NFSv3's `READ` has no client-visible
  open/close**, unlike `ReadOnlyFs::open`/`release`'s stateful handle model
  — the spike calls `fs.open(ino)` → `fs.read_at(...)` → `fs.release(...)`
  on *every single* `READ` RPC (visible in the request log: `READ ino=8
  offset=0`, then `READ ino=8 offset=32768` immediately after, each a fresh
  open/release pair) and it works, but a real `backend/nfs.rs` almost
  certainly wants its own handle cache keyed by `Ino` — open lazily on
  first access, keep it open across a burst of sequential reads, close on
  idle — rather than paying an open/release round-trip per RPC.
- **A real RPC/XDR crate, or careful hand-rolled code with actual test
  coverage,** in place of the spike's `unsafe`-free but unvalidated manual
  byte parsing (`Reader`/`Writer` in the spike do no bounds-checking against
  malformed input — a hand-crafted or corrupted RPC message would panic on
  a slice index, not fail cleanly). Chosen against this crate's MIT/Apache,
  no-copyleft constraint like every other dependency.
- **TCP only, NFSv3 only, single-fragment RPC messages only.** The spike
  never negotiates NFSv4, doesn't handle UDP, and assumes every RPC call
  fits in one record-marking fragment — true for the small requests this
  spike's procedures produce, not guaranteed in general.
- **Process lifecycle — spiked. A crashed server hangs the mount
  indefinitely by default, and macOS's recovery path needs a human unless
  the mount is configured to avoid it.** Killed the spike server (`kill -9`)
  while mounted, then read a file that had never been touched (so the read
  had to actually reach the server, not serve from local cache):
  - **Default mount options ("hard" NFS semantics): the read blocked for
    over 30 seconds with no resolution in sight.** Eventually macOS surfaced
    its own generic **"Server connections interrupted" alert** (`[ignore]` /
    `[disconnect all]`) — the same system-level dialog used for *any*
    unresponsive network mount (NFS/SMB/AFP), not something this crate
    triggers or can suppress from the server side. Only after a human
    clicked **"disconnect all"** did the blocked read unblock, failing with
    `Input/output error`, and the mount cleanly disappeared from `mount`
    with no stale entry. Without a human present — exactly the situation for
    a backend library embedded in another headless or background tool — this
    would hang forever.
  - **Mounting with `-o soft,timeo=20,retrans=2` instead of the default
    fixes it: bounded, silent, no human needed.** Same test, same kill: the
    read failed cleanly after **6 seconds** with `Operation timed out`, no
    dialog appeared, and `umount` afterward exited 0 with nothing left
    behind. `nfsstat -m` confirmed the options actually took effect
    (`NFS parameters: vers=3,tcp,...,soft,...,timeo=20,retrans=2`).

  **Consequence for `backend/nfs.rs`: mount with `soft` and a modest
  `timeo`/`retrans` by default, not classic NFS `hard` semantics.** This
  crate's own server should normally answer over loopback with near-zero
  latency, so a real slowdown is almost certainly the server having crashed,
  not a transient hiccup worth waiting out — and turning that into a bounded
  I/O error the calling application can see and handle beats an indefinite
  hang that only a human clicking a system dialog can clear. `Mount`'s
  "unmounts on drop" contract itself is unaffected by any of this in the
  *normal* shutdown path — `unmount()` should tear down the client-side
  mount before stopping the server task, at which point there's no in-flight
  request left to hang on; this finding is specifically about what happens
  if the server dies *unexpectedly* while still mounted (a panic, a crash),
  which no amount of clean-shutdown ordering can prevent outright.

**Done.** `backend/nfs/` is built for real: `xdr.rs`/`rpc.rs` (bounds-checked
XDR primitives and ONC RPC envelope), `mount_proto.rs` (MOUNT, RFC 1813
Appendix I), `nfs_proto.rs` (NFS v3, RFC 1813 §3 — `GETATTR`/`LOOKUP`/
`ACCESS`/`READ`/`READDIR(PLUS)`/`FSSTAT`/`FSINFO`/`PATHCONF`, plus a clean
`NFS3ERR_ROFS`/`NFS3ERR_NOTSUPP` rejection table for every mutating
procedure), `handle.rs` (the secret-in-handle scheme from the spike, unit-
and property-tested), and `server.rs`/`mod.rs` (accept loop, per-connection
dispatch, `mount_nfs` invocation, unmount ordering). The `.`/`..` convention
the spike found is now documented on `ReadOnlyFs::lookup` (`src/fs.rs`) and
fixed in `examples/memfs.rs`. The `readdir` cookie arithmetic that used to be
private to `backend/fuse.rs` moved to `backend/readdir_cookie.rs` (renamed to
`backend/readdir.rs` in Phase 1.5, which also moved the walk around it there),
unconditionally compiled and tested on every platform — doing so surfaced that one of its own
property tests asserted the wrong invariant (`trait_offset(for_entry(x)) ==
x`, when the real resume-after-a-cookie semantics require `x + 1`); the
`readdir`/`for_entry`/`trait_offset` implementation itself was already
correct, confirmed by hand-tracing the existing multi-call pagination
simulation, so only the test's assertion needed fixing.

Verified end to end on this machine, real tools throughout: `mount_nfs`
mounts unprivileged (`probe`/`memfs`), `mount` reports type `nfs`, `ls -lR`/
`cat`/`stat`/`find` all work, `cd subdir && ls ..` exercises `..` navigation,
a `shasum -a 256` through the mount matches one computed independently,
`nfsstat -m` confirms `soft,timeo=20,retrans=2` took effect, unmounting
(both `Mount::unmount()` and a manual `umount`) leaves no stale `mount`
entry, and `kill -9`-ing the server mid-mount produces a clean `Operation
timed out` in ~7 seconds rather than a hang or a system dialog. Paginated
`READDIRPLUS3` listings (a client budget too small to fit even one entry,
one large enough for the whole directory, and everything in between) are
covered by a property test rather than a single manual large-directory run.
Deferred to `docs/GAPS.md`: a per-inode handle cache (v1 pays an open/release
round trip per `READ3`), `onc-rpc` as an alternative to the hand-rolled RPC
envelope, and the unmeasured constant-time posture of the handle secret
comparison.

### Phase 1 — harden the trait

**Done.**

- **Inode/handle lifetime rules — documented, not new code.** `ReadOnlyFs`'s
  doc comment (`src/fs.rs`) now states the contract explicitly: an `Ino` must
  answer `getattr` correctly for the life of the mount (no eviction
  contract), `open` may be called more than once for the same `ino` with each
  call returning an independent `FileHandle`, handles need not be released in
  the order they were opened, and a backend serving several worker threads
  may call `read_at` on the same handle concurrently — implementors do their
  own locking, matching the existing `Send + Sync` requirement.
- **`forget` handling — added as an optional hook, not a required one.**
  `ReadOnlyFs::forget(&self, ino: Ino, nlookup: u64)` defaults to doing
  nothing. Nothing in the trait requires lookup-count bookkeeping — a
  filesystem that answers every `getattr`/`lookup` fresh has no cache to
  evict — so the default is correct for `examples/memfs.rs` and for
  ciphercask's likely shape unless it caches a built tree. The hook exists for
  implementors that do cache: `backend/fuse.rs` now forwards `fuser`'s
  `forget` callback (`batch_forget`'s default already forwards each node to
  `forget`, so no separate override was needed). cfapi has no equivalent
  notification and never calls it — documented on the trait method, not
  worked around.
- **xattr plumbing — was dead code, now wired.** `ReadOnlyFs::listxattr` and
  `getxattr` have existed since Phase 0 with sensible defaults, but
  `backend/fuse.rs`'s `FuseAdapter` never implemented `fuser`'s `listxattr`/
  `getxattr` callbacks, so `fuser`'s own defaults (`ENOSYS`, logged as "Not
  Implemented") answered instead — any implementor's overrides were
  unreachable from the FUSE path. Both are now wired, honoring FUSE's
  size-query convention (`size == 0` → reply with the length only; a value
  too large for the given `size` → `ERANGE`, never silent truncation).
- **`statfs`** was already wired in Phase 0; nothing further needed.
- **Property tests over the readdir cookie arithmetic.** The cookie math that
  used to be inline in `FuseAdapter::readdir` (`.` = cookie 1, `..` = cookie
  2, trait entry `i` = cookie `i + 3`) is now `backend/fuse::cookie`, three
  pure functions with no `fuser` types involved, so it can be tested without
  a live session. `src/backend/fuse_tests.rs` adds `proptest`
  (new dev-dependency; `cargo deny check licenses bans sources` passes with
  it — MIT/Apache-2.0 throughout its graph, only new warning is the
  pre-existing "duplicate `syn` version" class, already `warn` not `deny`)
  covering two things: the cookie functions round-trip and never collide with
  `.`/`..`'s reserved cookies, and — the part worth having a property test
  for — a full directory listing reassembled from many buffer-limited FUSE
  calls (`one_call`/`full_listing` in the test file simulate `ReplyDirectory`'s
  "buffer full" contract with an arbitrary per-call capacity) always equals
  exactly one `.`, one `..`, then every trait entry in order, for any
  combination of entry count (0–500) and buffer capacity (1–20), 512 cases per
  property.

**`fuse` is now Linux-only — the macOS FUSE fallback is removed from the
tree, not merely made optional.** While verifying the above, `cargo build
--all-targets`/`cargo test` failed to link on this machine because its
macFUSE install is gone (only a stale 2011-era i386/x86_64 OSXFUSE.framework
remains, no `fuse.pc`, incompatible with this arm64 toolchain anyway).
Reproducing the same link failure on a clean `git stash` confirmed this was
pre-existing and unrelated to the Phase 1 changes above — but it exposed that
the crate had never actually committed to "macOS's backend is NFS" (the
"Revised decision" earlier in this file): `fuse` was still default-on and
target-scoped to `any(linux, macos)`, so any macOS box without a working
macFUSE install couldn't build or test the crate at all with default
features, and `probe`/`any_backend_available` still advertised FUSE as
available on macOS by feature flag alone, regardless of whether `fuser` was
actually linkable there.

Fixed by making the code match the decision that was already on record:
`fuse` (`Cargo.toml`) is now gated to `target_os = "linux"` only, in both the
feature-to-dependency mapping and every `cfg` that gates `backend/fuse.rs`'s
inclusion (`backend/mod.rs`, `src/lib.rs`, `examples/probe.rs`). The
`[target.'cfg(target_os = "macos")'.dependencies]` table no longer names
`fuser` at all — only `libc`, still needed for `types.rs`'s
`cfg(unix)` uid/gid helpers. macOS currently compiles with **no** mount
backend; `auto_mount` returns `FsError::Unsupported` there until
`backend/nfs.rs` is built for real (still just the Phase 0.6 spike, not in
the tree). This is not a regression — macOS mounting didn't work by default
on an unprepared machine before this change either, it just failed at
`cargo build` time instead of failing cleanly at `mount()` time with an
actionable error.

Consequences worth recording:
- `cargo check --target aarch64-apple-darwin` no longer needs
  `--no-default-features` — default features are now a true no-op for that
  target, so the flag was dropped from `CLAUDE.md`, `Cargo.toml`'s cross-check
  guidance, and `.github/workflows/ci.yml`'s `cross-check` job.
- CI's `build` job no longer installs macFUSE on the `macos-latest` runner —
  there is nothing left in the tree that would use it. That runner still
  builds and tests the platform-independent parts of the crate; it proves
  nothing about FUSE or NFS on macOS specifically, which is now honestly
  reflected in the workflow's comments rather than papered over by a
  `continue-on-error` cask install.
- `cargo test`, `cargo build --all-targets`, and `cargo clippy --all-targets
  -- -Dwarnings` all now pass locally on this machine with **default**
  features — no `--no-default-features`/`--features cfapi` workaround
  needed — closing out the verification gap from earlier in this phase.
  `cargo deny check licenses bans sources` and both cross-compile checks
  (`x86_64-pc-windows-msvc` clippy, `aarch64-apple-darwin` check) still pass.
- If a macOS FUSE fallback is ever wanted again before `backend/nfs.rs`
  exists, it needs its own opt-in feature and its own manifest identity for
  the `fuser` dependency (Cargo has no per-target default-feature list, so
  the only way to keep it off by default without also disabling Linux's is a
  second, differently-named optional dependency) — not a reversion to the
  shared `any(linux, macos)` gating this phase removed.

### Phase 1.5 — consolidate the backend seams

**Done.**

The FUSE and NFS backends were built far apart and had never been reconciled.
Both worked, but they answered the same questions differently, and each
difference would have become a three-way inconsistency once cfapi landed. This
phase removed the divergences by giving the shared behaviour one home, and
brought the FUSE backend to the standard the NFS work set.

What diverged, and what it is now:

| Concern | Before | Now |
|---|---|---|
| Teardown | FUSE relied on `fuser::Mount`'s `Drop`, which unmounts but leaves the serving thread detached; NFS had its own `Drop` plus an idempotence flag | `backend::Mounted`, whose `unmount` consumes the handle. `Mount` calls it from `unmount()` and from its own `Drop`, so it runs exactly once and FUSE now joins its thread |
| Dispatch | a `MountHandle` enum with one cfg'd variant, one cfg'd `unmount` arm and one `auto_mount` block per backend | `Box<dyn Mounted>`. The enum is gone; a backend contributes one arm to `mount` and one to `auto_mount` |
| `..` in `readdir` | FUSE reported the directory's own inode; NFS resolved the parent with `lookup(dir, "..")` and failed the whole call if that failed | `backend::readdir::emit`, shared. `lookup(dir, "..")` best-effort, falling back to the directory itself — `fs.rs` documents answering `..` as a should, not a must |
| Listing pagination | the cookie arithmetic was shared, but the walk around it was written twice | one `emit`, parameterised by a `Sink` closure. Only "does this entry fit?" is backend-specific |
| Unsupported builder options | FUSE rejected `auto_unmount` without `allow_other`; NFS silently ignored both | `backend::preflight`: each backend declares a `Caps`, and a request it cannot honor is an error naming the backend |
| Mountpoint validation | none; each platform surfaced its helper binary's own message | `preflight::check` — exists, and is a directory — before any platform code runs |
| Read allocation | FUSE allocated whatever `size` asked for; NFS capped at `RTMAX` | FUSE caps at `MAX_READ` too |
| `tracing` feature | declared in `Cargo.toml`, referenced by no code, so `fs.rs`'s "errors are logged, not propagated" was not true | `backend/trace.rs`, used where an error is deliberately discarded |

Two things worth recording beyond the table:

- **The `readdir` property tests were testing a copy of the code.**
  `backend/fuse_tests.rs` held a hand-written `one_call`/`full_listing`
  simulation of `FuseAdapter::readdir`'s three-stage structure, so 512 cases
  per property exercised a mirror of the algorithm rather than the algorithm.
  Extracting `emit` made the production code drivable from a test; the
  properties now live in `backend/readdir_tests.rs` and call it directly, with
  a second pair covering `Dots::Omit`, the shape cfapi will use. What stayed in
  `fuse_tests.rs` became real unit tests over the FUSE-specific pure functions
  (`to_fuser_attr`, `to_fuser_kind`, `xattr_reply`, `read_buffer_len`, the
  `FsError` → `Errno` mapping). Lib tests went from 17 to 41.
- **`cargo check --target aarch64-apple-darwin` was too weak a cross-check.**
  Without `--all-targets` it does not compile the `*_tests.rs` files, so a
  macOS-gated test referring to a renamed item passes locally and fails on the
  runner. This was hit during this phase (`nfs_proto_tests.rs` still named
  `readdir_cookie`). `CLAUDE.md` and CI's `cross-check` job now pass
  `--all-targets`; CI also lints `--features tracing` and
  `--no-default-features`, neither of which any job compiled before.

Verified on Linux with real tools, not process output: `mount` reports
`ro,nosuid,nodev,user_id=1000`, `ls -lR`, `cat`, `find`, `stat`, `dd skip=10
count=12`, and a `sha256sum` through the mount matching one computed
independently; `stat -c %i <mnt>/subdir/..` now reports the root's inode where
it previously reported `subdir`'s own; a `Mount` dropped without calling
`unmount()` tears down and leaves no stale entry in `mount`; and mounting at a
nonexistent path or at a regular file fails with the new message instead of a
raw `fusermount3` string. macOS and Windows are cross-compile-checked only —
CI's `nfs-mount-smoke-test` remains the real verdict for the NFS changes.

What this leaves for Phase 2: the cfapi backend supplies a `mount` function, a
`Mounted` impl, and a `Caps`. Enumeration reuses `readdir::emit` with
`Dots::Omit` and a sink that always accepts, since cfapi's
`FETCH_PLACEHOLDERS` callback carries no size budget (Phase 2 spike item 3).

### Phase 2 — Windows backend

**Current state:** `src/backend/cfapi.rs`'s `mount()` is a hard stub — it
checks `probe()` and returns `FsError::Unsupported`, nothing else. Registration
was confirmed working in Phase 0. All four items below are now spiked and
answered with real evidence, the same way NFS's open questions were closed
before committing to `backend/nfs.rs`'s design.

1. **Fetch-callback correctness — spiked, and it works.** A throwaway spike
   (built, run, then deleted — not part of the crate, same convention as every
   other Phase 0/0.5/0.6 spike) registered a sync root, created a placeholder
   file backed by 5 MiB of deterministic content via `CfCreatePlaceholders`,
   then opened and read it through a normal `std::fs::File`.
   `CF_CALLBACK_TYPE_FETCH_DATA` fired with `RequiredFileOffset=0`,
   `RequiredLength=5242880` (the whole file), `CfExecute` with
   `CF_OPERATION_TYPE_TRANSFER_DATA` delivered the bytes, and a SHA-256 of the
   read-back content matched one computed independently. One real gotcha hit
   along the way: `CfCreatePlaceholders` fails with
   `ERROR_CLOUD_FILE_INVALID_REQUEST` (`0x8007017C`) unless `FileIdentity` is
   set to a non-null, non-zero-length buffer — the docs call it "mandatory for
   files," and the failure mode gives no hint that this specific field is the
   problem.
2. **The sequential-read assumption — spiked, and the real answer is stronger
   than what was assumed: cfapi does not do ranged reads at all, it hydrates
   the whole file unconditionally on first touch.** Three variants were tried
   against a never-before-touched placeholder, seeking straight to a 4 MiB
   offset and reading only 4 KiB, without ever touching byte 0 first:
   buffered `std::fs::File` with `CF_HYDRATION_POLICY_PARTIAL`, the same with
   `CF_HYDRATION_POLICY_PROGRESSIVE`, and unbuffered `CreateFileW` with
   `FILE_FLAG_NO_BUFFERING` and a sector-aligned offset/length (removing the
   NTFS cache manager's own read-ahead as a possible explanation). All three
   produced the identical single `FETCH_DATA` call: offset 0, length equal to
   the whole file. Neither hydration policy nor bypassing buffered I/O changed
   this. **Consequence: this is not "cfapi happens to hydrate sequentially,"
   it is "cfapi has no partial/ranged fetch path a caller can reach through
   ordinary file I/O" — `docs/GAPS.md`'s materialise-on-open recommendation
   was already the right call, and this closes the question rather than
   just confirming a milder version of it.**
3. **Directory enumeration correctness and scale — spiked, works, and surfaced
   a Rust-specific test-tooling gotcha.** A placeholder directory
   (`CF_PLACEHOLDER_CREATE_INFO` with `FILE_ATTRIBUTE_DIRECTORY`, no
   `CF_PLACEHOLDER_CREATE_FLAG_DISABLE_ON_DEMAND_POPULATION`, confirmed
   afterward to carry `FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_OFFLINE |
   FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`) was populated with 3000 synthetic
   children by returning `CF_OPERATION_TYPE_TRANSFER_PLACEHOLDERS` from a
   `CF_CALLBACK_TYPE_FETCH_PLACEHOLDERS` handler. Unlike NFS's `READDIR`,
   `CF_CALLBACK_PARAMETERS`'s `FetchPlaceholders` variant carries no
   count/size budget field at all (only `Flags` and a wildcard `Pattern`), and
   a single `CfExecute` call handed back all 3000 entries in one shot with no
   truncation or error — **cfapi's callback shape genuinely sidesteps the
   size-budget failure mode that NFS's first `READDIR` attempt fell into (see
   Phase 0.6)**, at least at this scale. Verified with real tools: `cmd`'s
   `dir` and PowerShell's `Get-ChildItem`, run against the mount from a
   separate process while the sync-provider process was deliberately kept
   alive and sleeping, both listed all 3000 children correctly. **The gotcha:
   `std::fs::read_dir` on the same path, from the same process, immediately
   returned zero entries without ever invoking `FETCH_PLACEHOLDERS` at all** —
   confirmed by checking the callback invocation count directly, not just the
   returned list. Whatever directory-query mechanism Rust's standard library
   uses on Windows does not trigger cfapi's on-demand population the way
   `FindFirstFileW`-based tools (`dir`, `Get-ChildItem`, and by extension
   Explorer) do. Root cause not tracked down further — not needed to answer
   the question this spike was for — but this matters directly for
   `anymount`'s own test suite: **a future integration test that verifies
   cfapi directory listing must not rely on `std::fs::read_dir`** to check
   the result; shelling out to `dir`, or a raw `FindFirstFileW` binding, is
   needed instead, the same "verify with real tools" bar the top of this file
   already asks for, but now with a concrete trap identified.
4. **Crash/lifecycle behavior — spiked, and cfapi recovers well on its own, no
   `soft`/`timeo`-equivalent configuration needed.** Two throwaway binaries (a
   `server` registering a sync root and a `client` opening and blocking-reading
   the placeholder it created — built, run, then deleted, same convention as
   every other spike here) reproduced a crash mid-hydration directly: the
   server's `FETCH_DATA` callback deliberately slept 20 seconds without ever
   calling `CfExecute`, and partway through that sleep — with the client's
   `File::open`+`read_to_end` genuinely blocked waiting on it — the server
   process was killed with `taskkill /F` (a hard kill, not a clean shutdown).
   **The client's blocked read unblocked on its own after 12.08 seconds**,
   failing with a specific, actionable error:
   `ERROR_CLOUD_FILE_PROVIDER_TERMINATED` ("The cloud file provider exited
   unexpectedly," os error 404) — not a generic timeout or an indefinite hang.
   12 seconds is well under the `CF_CALLBACK_TYPE` docs' fixed 60-second
   per-callback timeout, suggesting cfapi/NTFS detects the dead process
   directly (likely via the closed connection handle) rather than waiting out
   the callback timeout — plausibly a bounded grace period in case a
   replacement provider process reconnects to the same sync root, though that
   reconnect path itself wasn't tested. Also checked: after the crash, a
   **fresh process re-registering the same sync root path succeeded cleanly**
   (`CfRegisterSyncRoot` and `CfConnectSyncRoot` both succeeded, a new
   placeholder was created normally) — a crashed provider does not leave the
   sync root permanently wedged. **Consequence: `backend/cfapi.rs` needs no
   NFS-style `soft`/`timeo` configuration or equivalent — cfapi's own
   12-second recovery window is already bounded and requires no human
   intervention, unlike NFS's default "hard" mount (Phase 0.6), and restart
   after a crash is a clean re-register, not a stale-state cleanup problem.**
   This closes Phase 2's spike list; all four items now have real answers.

All four have real answers, and Phase 1.5 has since removed most of the
non-cfapi work this phase would otherwise carry: `CfApiHandle` implements
`backend::Mounted` (teardown, and unmount-on-drop, come from `Mount`), a
`Caps` covers option validation and the mountpoint check, and directory
enumeration reuses `backend::readdir::emit` with `Dots::Omit`. What remains is
cfapi-specific: build `CfApiHandle`/`mount()` for real against the
`windows` crate directly — `PopulationType::Partial` for on-demand
enumeration, `STREAMING_ALLOWED` to avoid persisting fetched data,
`AUTO_DEHYDRATION_ALLOWED` for reclamation — and verify with the same bar
every other backend has met: `dir`/`ls`, `type`/`cat`, a checksum through the
mount matching one computed independently, and confirmation that
`CfUnregisterSyncRoot` leaves no orphaned sync root behind.

ProjFS is not part of this crate — see the Windows spike result above for why
it was cut rather than kept as a fallback.

### Phase 3 — ciphercask integration (separate repo)

A `CaskFs` implementing `ReadOnlyFs`:

- Build the tree at mount from `get_backups()` + `get_files_metadata_bulk()`,
  reusing the approach proven in `tasks/treemap/mod.rs:136`
  (`build_tree(&[(FileId, FileInventoryData)]) -> DirNode`).
- Layout `/<backup_id>/<original/absolute/path>`; LAN casks also expose
  `/<client_id>/<backup_id>/...` via `restore_file_for_client`.
- **Materialise-on-open**: `open()` calls `Cask::restore_file()` into
  `{cache_dir}/ciphercask/mount/{cask}/{backup_id}/`, following the convention
  at `tasks/add_remote/mod.rs:46`. This preserves the whole-file BLAKE3 check in
  `backend_helpers.rs:69` that a streaming reader would lose.

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `fuse` | yes | FUSE backend (Linux only). Dependency is `cfg(target_os = "linux")`-scoped |
| `nfs` | yes | NFS backend (macOS only). Hand-rolled RPC/XDR over `std::net`, no new dependency — `libc::arc4random_buf`/`libc::unmount` (already a macOS target dependency) cover the secret and clean teardown |
| `cfapi` | yes | Cloud Files backend (Windows only). Dependency is `cfg(windows)`-scoped |
| `tracing` | no | Lifecycle and discarded-error logging via `backend/trace.rs`. Not yet per-operation spans |

Cargo cannot express per-OS defaults, so `fuse`, `nfs` and `cfapi` all ship in
`default` and compile to nothing off-platform; because the `fuse`/`cfapi`
dependencies live under `[target.'cfg(...)'.dependencies]`, a Linux build
never fetches `windows`, and `nfs` needs no dependency of its own at all.

`fuse` narrowing from `cfg(unix)` (Linux and macOS both) to `cfg(target_os =
"linux")` only is **done** (Phase 1) — see that phase's notes for what broke
locally before this landed and why. `nfs` for macOS is now built for real
(Phase 0.6, closed below) — `backend/nfs/` in the tree, not the spike. (An
earlier version of this plan named the macOS feature `webdav`; superseded by
the NFS decision above.)

Deliberately absent: any `winfsp`, `projfs`, or `async` feature.

## Deferred

- Materialise-on-open cache helper, if a second consumer needs it.
- Streaming random access — needs ciphercask to record plaintext chunk lengths
  (format_version 4). Designed so the trait does not change when it lands.
- `anymount-winfsp` as an isolated crate, if drive letters or writes are needed.

## Verification

```sh
cargo test
cargo clippy --all-targets -- -Dwarnings
cargo fmt --all -- --check
cargo deny check licenses bans sources

cargo check --target x86_64-pc-windows-msvc
cargo check --target aarch64-apple-darwin
```

Per platform, verify with real tools rather than trusting process output:

1. `cargo run --example probe` — backends available on this machine.
2. `cargo run --example memfs -- <mountpoint>`.
3. `ls -lR`, `cat`, `find`, `stat`, and a `sha256sum` compared against a digest
   computed independently.
4. Confirm clean unmount leaves no stale entry in `mount`.
5. Linux: confirm the mount is unprivileged.
