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
  or proprietary.** ProjFS and cfapi through Microsoft's own `windows` crate are
  MIT/Apache. FSKit is unusable from Rust (Swift-only entry point, Xcode-built
  `.appex`, block-device filesystems only).

## Decisions

| Decision | Choice |
|---|---|
| Name | `anymount` |
| Licence | MIT OR Apache-2.0, enforced by `cargo-deny` |
| Structure | **Single crate.** See below |
| Async | **Sync-only** |
| v1 scope | **Read-only.** Write ops return `EROFS`; gaps in `GAPS.md` |
| Windows | cfapi preferred, ProjFS fallback; **WinFsp excluded entirely** |
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
ProjFS and cfapi stubs are known-good before touching a Windows box. macOS can
only be checked with `--no-default-features`, because `fuser`'s build script
calls pkg-config for libfuse and that needs a macOS sysroot; the `macos-latest`
CI runner covers the real thing.

**Windows — next.** Two questions:
1. Does the crate build against the `windows` crate, and does `probe()` find
   ProjFS and cfapi?
2. **Can Win32 `CfRegisterSyncRoot` register a sync root from an unpackaged
   binary?** Its docs state no package-identity requirement (only `WRITE_DATA`/
   `WRITE_DAC`), whereas the WinRT `StorageProviderSyncRootManager` path used by
   the `cloud-filter` crate *is* identity-gated. Public sources conflate the two;
   nobody has confirmed the Win32 path unpackaged.
   - **Pass** → cfapi is the Windows default; ProjFS becomes secondary.
   - **Fail** → ProjFS is the CLI default; cfapi stays for the GUI, which ships
     an installer and can carry a sparse package.

**macOS — last.** Same FUSE code path as Linux. Confirm macFUSE resolves the
libfuse path, and that macFUSE 5.2+ on macOS 15.4+ mounts without a kext.

### Phase 1 — harden the trait

Inode/handle lifetime rules, `forget` handling, xattr plumbing, `statfs`.
Property tests over the readdir cookie arithmetic.

### Phase 2 — Windows backends

Both against the `windows` crate directly. cfapi: `PopulationType::Partial` for
on-demand enumeration, `STREAMING_ALLOWED` to avoid persisting data,
`AUTO_DEHYDRATION_ALLOWED` for reclamation. ProjFS: `PrjStartVirtualizing` plus
enumeration and `PrjGetFileDataCallback`.

**Hard constraint discovered in Phase 0: ProjFS entry points must be resolved
dynamically.** `ProjectedFSLib.dll` only exists once the `Client-ProjFS`
optional feature is enabled, and that feature is **off by default**. A
load-time import of any `Prj*` symbol therefore stops the whole process from
starting on a machine without it — the loader fails before `main`, so there is
no chance to report a friendly error, and `probe()` could never return `false`.

`src/backend/projfs.rs` consequently makes **no static reference to any `Prj*`
symbol**, and probes with `LoadLibraryW` instead. Phase 2 must preserve that:
resolve every ProjFS function through `GetProcAddress` or delay-loading. cfapi
needs no such care — `CldApi.dll` ships with every Windows 10 1709+ install and
is not optional.

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
| `fuse` | yes | FUSE backend. Dependency is `cfg(unix)`-scoped |
| `cfapi` | yes | Cloud Files backend. Dependency is `cfg(windows)`-scoped |
| `projfs` | yes | ProjFS backend. Dependency is `cfg(windows)`-scoped |
| `tracing` | no | Per-operation spans |

Cargo cannot express per-OS defaults, so all three ship in `default` and compile
to nothing off-platform; because the dependencies live under
`[target.'cfg(...)'.dependencies]`, a Linux build never fetches `windows`.

Deliberately absent: any `winfsp` or `async` feature.

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
6. Windows: confirm ProjFS returns an actionable error when `Client-ProjFS` is
   disabled, rather than an opaque `HRESULT`.
