# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`anymount` mounts a read-only filesystem from user space on Linux, macOS and
Windows. An implementor writes one trait, `ReadOnlyFs`; the crate mounts it with
whatever mechanism the host OS provides — FUSE on Linux, NFS on macOS, the
Cloud Files API (cfapi) on Windows. One backend per platform, not a shared
mechanism across platforms — see `docs/PLAN.md`'s "Revised decision" for why
FUSE and WebDAV were both set aside on macOS in favor of NFS.

**macOS mounts via a from-scratch NFSv3 server** (`backend/nfs/`), using the
OS's built-in `mount_nfs` client — no macFUSE, no kernel extension, no root.
See `docs/PLAN.md`'s Phase 0.6 for how that mechanism was chosen and proven,
and the "Platform constraints" section below for what to know before editing
it. There is no `fuse` module, feature, or `Backend` variant scoped to macOS;
`fuse` is Linux-only. Do not reach for macFUSE as a stopgap without
revisiting the NFS decision first.

**Read-only is the scope, not a stage.** Write operations report `EROFS`.
`docs/GAPS.md` lists every limitation and what changing it costs.

**ProjFS was evaluated in the Windows spike and cut entirely — not deferred,
not an opt-in feature.** There is no `projfs` module, feature, or `Backend`
variant in the tree. See `docs/PLAN.md` (Phase 0) and `docs/GAPS.md` for why;
do not reintroduce it without new evidence that cfapi is insufficient.

The first consumer is `ciphercask` (a separate repo), which will mount an
encrypted backup archive for restore browsing. That drives several design
choices — see *Why it looks like this* below.

See `docs/PLAN.md` for phases and open questions, `docs/GAPS.md` for known
limitations, and `README.md` for what the crate does and how to use it.

## Commands

```bash
# Build
cargo build --all-targets

# Test
cargo test
cargo test --test trait_shape                    # integration tests only
cargo test some____test____name                  # single test

# Lint (must be clean before any phase is considered done)
cargo clippy --all-targets -- -Dwarnings
cargo fmt --all -- --check

# Cross-compile checks for the other OS backend (no linker needed for `check`).
# Windows checks fully. macOS has no backend in the tree yet (see "What this
# is"), so this only proves the platform-independent parts still compile
# there; it needs no default-features flag since `fuse` is Linux-only and a
# no-op off it.
cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -Dwarnings
cargo check --target aarch64-apple-darwin

# Supply chain — run before adding or updating any dependency
cargo deny check licenses bans sources

# MSRV — pin to the exact floor `rust-version` in Cargo.toml declares. Needs
# `rustup toolchain install 1.88.0` once; `cargo clippy --all-targets` alone
# uses whatever toolchain is active and will not catch API usage newer than
# the floor.
cargo +1.88.0 check --all-targets

# Examples. `probe` needs no mountpoint and no privileges; run it first on a
# new machine to see which backends are actually available.
cargo run --example probe
mkdir -p /tmp/anymount-demo && cargo run --example memfs -- /tmp/anymount-demo
```

## Architecture

Single crate, edition 2024, `rust-version = 1.88.0`.

- `fs.rs` — the `ReadOnlyFs` trait: `lookup`, `getattr`, `readdir`, `open`,
  `read_at`, `release`, plus default-implemented `listxattr`, `getxattr`,
  `statfs`. The operation set is the FUSE *lowlevel* intersection, because
  cfapi maps onto it but not the reverse.
- `types.rs` — `Ino`, `FileHandle`, `FileAttr`, `DirEntry`, `FileKind`,
  `StatFs`. `FileKind` has only `File` and `Directory`; there is no symlink
  variant.
- `error.rs` — `FsError`, mapped to `errno` on Unix and to `HRESULT`/`NTSTATUS`
  on Windows. `FsError::context` attaches an explanation while keeping the
  underlying errno.
- `mount.rs` — `MountBuilder` and `Mount`. `Mount` unmounts on drop;
  `Mount::unmount` surfaces the errors that drop swallows.
- `backend/` — one module per OS mechanism, `cfg`-gated *and* feature-gated:
  `fuse.rs` (Linux only), `nfs/` (macOS only — a submodule tree: `mod.rs`,
  `xdr.rs`, `rpc.rs`, `mount_proto.rs`, `nfs_proto.rs`, `handle.rs`,
  `server.rs`), `cfapi.rs` (Windows). `readdir_cookie.rs` holds the `.`/`..`
  cookie arithmetic shared by `fuse.rs` and `nfs/nfs_proto.rs`, compiled and
  tested unconditionally rather than gated to either. `backend/mod.rs`
  resolves `Backend::Auto` and owns the `MountHandle` enum.

### Why it looks like this

Three decisions are driven by the first consumer and should not be revisited
without revisiting that:

- **Sync, not async.** ciphercask's `Cask` trait is synchronous top to bottom,
  and its LAN backend already hides tokio behind its own runtime and `block_on`.
  An async trait could not be fed by it. Concurrency comes from serving FUSE
  requests on several threads (`Config::n_threads`), not from async.
- **No symlinks.** ciphercask skips them at backup time, and cfapi does not
  model them the way FUSE does.
- **`read_at` takes an offset even though some archives cannot seek.** cfapi
  calls `read_at` sequentially during hydration, because it materialises whole
  files. Only FUSE issues random reads. An implementor whose archive can only
  decode from byte 0 should materialise on open and serve reads from a cache;
  the trait does not change when true streaming becomes possible.

## Platform constraints worth knowing before editing a backend

**FUSE `auto_unmount` requires a non-`Owner` ACL.** `fusermount3` refuses to arm
it on an owner-private mount, so `auto_unmount` defaults off and `mount()`
rejects the combination with an explanation rather than passing fuser's message
through.

**Linux does not link libfuse.** `fuser` is built with
`default-features = false`, so mounting goes through the `fusermount3` binary.
That keeps LGPL code out of the link and allows unprivileged mounts. Do not add
`fuser`'s default features to the Linux target — that pulls in the libfuse
link path, which this crate deliberately avoids everywhere it mounts via FUSE.

**NFS authorizes with a secret embedded in the file handle, not `AUTH_SYS`.**
`AUTH_SYS` trusts client-supplied uid/gid with no verification — confirmed
worthless in the Phase 0.6 spike, where an unprivileged `mount_nfs` invocation
claimed `uid=0 gid=0`. Instead, `FileHandle3` (`backend/nfs/handle.rs`)
embeds a per-mount random 128-bit secret in every handle this server hands
out, and the same secret is required as a literal path segment in `MNT`
(`/export/<hex secret>`). Do not add real credential checking on top of this
without revisiting `docs/PLAN.md`'s Phase 0.6 — it was a deliberate,
tested-against-guessing choice, not an oversight.

**NFS mounts `soft` with a short `timeo`/`retrans`, not classic NFS `hard`
semantics.** A crashed server under `hard` semantics hangs every read
indefinitely until a human dismisses macOS's own "Server connections
interrupted" dialog — confirmed in the Phase 0.6 spike. `soft,timeo=20,
retrans=2` (`backend/nfs/mod.rs`'s `mount_nfs` invocation) turns that into a
bounded `Operation timed out` in a few seconds instead, since this crate's own
server should answer over loopback with near-zero latency — a real slowdown
almost certainly means the server crashed, not a transient hiccup worth
waiting out. Do not remove these options to "fix" a slow mount; the fix
belongs in the server, not in loosening the client's patience.

**NFS's RPC framing supports only single-fragment messages.** `rpc.rs`'s
`read_message` closes the connection on a multi-fragment ONC RPC message
rather than reassembling one — every request the spike observed from
`mount_nfs` fit in one fragment. See `docs/GAPS.md` if a client that needs
reassembly ever shows up.

## Licensing is a design constraint

The crate is MIT OR Apache-2.0 with no copyleft anywhere in the dependency
graph. This is not incidental — the obvious bindings for these platform APIs are
all copyleft, so the crate routes around them:

| Avoided | Licence | Used instead |
|---|---|---|
| `winfsp`, `winfsp-sys` | GPL-3.0 | nothing; WinFsp is out of scope |
| `windows-projfs` | GPL-2.0 | Microsoft's `windows` crate |
| `dokan`, `dokan-sys` | wrap LGPL Dokany | cfapi |
| libfuse (linked) | LGPL | `fusermount3` on Linux (the only platform that links FUSE at all — see "What this is") |

`deny.toml` bans those crates by name, so an accidental `cargo add` fails loudly
rather than quietly relicensing the crate. The licence allow-list is
permissive-only and excludes copyleft by omission; adding an MPL/LGPL/GPL
dependency fails CI by design. The fix is a PR that edits the allow-list and
says why, never a silent `exceptions` entry.

Adding WinFsp later means a separate `anymount-winfsp` crate, so GPL never
enters a default dependency graph.

## Conventions

**Test file layout:** tests live in separate `*_tests.rs` files, registered at
the bottom of the source file with:
```rust
#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
```
Integration tests live in `tests/`.

**Test naming:** `subject____condition____result` — exactly four underscores
between segments. Because consecutive underscores trip `non_snake_case`, every
`*_tests.rs` file carries `#![allow(non_snake_case)]` at the top, alongside
`#![allow(clippy::unwrap_used)]` and `#![allow(clippy::expect_used)]`.

**No `.unwrap()` / `.expect()` in production code** — use `?`. `clippy.toml`
allows them in tests only. Workspace lints also warn on `cognitive_complexity`.

**Public docs:** every new public item needs a doc comment. Doc examples that
use a gated API must be `cfg`-gated too; `cargo test` runs them.

**Errors:** `thiserror`, in `error.rs`. `FsError` is `#[non_exhaustive]`.

**Unsafe:** the crate is `#![forbid(unsafe_op_in_unsafe_fn)]`. FFI calls need an
explicit `// SAFETY:` comment saying why the call is sound — what the arguments
point at, what the callee does with them, and what is kept alive.

**Platform code:** keep OS-specific FFI behind `backend/`, `cfg`-gated;
platform-specific tests are `cfg`-gated too. **CI is where Windows and macOS
code actually executes.** The cross-compile commands above are the local
pre-push check: they prove the other backends type-check, not that they work.
Run them before calling a change done, review FFI carefully, and expect CI to be
the real verdict. For code that is not backend-specific FFI but still needs a
Unix/non-Unix split — `types.rs`'s `current_uid`/`current_gid`, `error.rs`'s
`to_errno` — use a pair of `#[cfg(unix)]` / `#[cfg(not(unix))]` functions of
the same name and signature, not an inline `cfg!` branch.

**Verify mounts with real tools, not process output.** A backend that prints
"mounted" has proved nothing. Check `mount`, then `ls -lR`, `cat`, `stat`, and a
`sha256sum` compared against a digest computed independently of the mount. The
`mount-smoke-test` CI job does exactly this and is the template.

**Property tests:** reach for `proptest` (dev-dependency) when a pure function
has a round-trip or arithmetic invariant worth checking across many inputs, not
just a couple of hand-picked examples — `backend/fuse_tests.rs`'s `cookie`
module tests (readdir cookie encode/decode, and a simulated multi-call
pagination) are the template. Don't reach for it for ordinary example-based
behavior; a `subject____condition____result` unit test is still the default.

## Code Style

**Prefer `use` imports over fully-qualified paths** in function bodies —
`&FsError`, not `&crate::error::FsError`, when `FsError` is already imported at
the top of the file. The one deliberate exception is `backend/fuse.rs`, which
writes `fuser::FileHandle`/`fuser::FileType` fully-qualified throughout: `fuser`
has its own `FileHandle`, which collides with `crate::types::FileHandle` if
both are in scope unqualified. Don't add a `use fuser::FileHandle` there to
"clean it up" — that reintroduces the collision.

**Never set `RUSTFLAGS=-Dwarnings` in the CI environment** (or any shared
`env:` block). It applies to *dependency* compilation too — `fuser`, `windows`,
`proptest`, and their transitive trees — so a new stable rustc that adds one
warning anywhere in that graph reds every job with no anymount changes. Pass
`-Dwarnings` to `cargo clippy` explicitly instead (as every job in
`.github/workflows/ci.yml` already does), which scopes the deny to this crate
only.

## Definition of Done

Before considering any task or phase complete:

- `cargo test` passes with zero failures
- `cargo clippy --all-targets -- -Dwarnings` is clean
- `cargo fmt --all -- --check` is clean
- `cargo deny check licenses bans sources` passes
- The two cross-compile checks pass
- `cargo +1.88.0 check --all-targets` (MSRV) passes
- No `.unwrap()` / `.expect()` in production code
- New public items have doc comments
- If behaviour changed on a platform, it was verified with real tools there, or
  the fact that it was not is stated plainly

## Docs are part of "done"

Each file has one job; keep changes in the right one rather than restating
across them:

| File | Holds | Scope |
|---|---|---|
| `README.md` | What the crate does, how to use it, and the caveats that change how it should be used | Link out rather than expand |
| `docs/PLAN.md` | Phases, decisions and their rationale, open questions | The plan of record; update when a phase closes or a decision changes |
| `docs/GAPS.md` | Every known limitation, why it exists, and what changing it costs | One section per gap. Add to it rather than quietly narrowing scope |
| `CLAUDE.md` | Conventions and constraints a contributor needs before editing | Rules, not narrative |

## Writing style for `README.md`, `docs/*.md` and rustdoc

- **No second person, no first person.** Not "your filesystem", "you can", "we
  chose". Describe the crate and what it does: "mounts a read-only filesystem",
  "the backend calls `read_at` sequentially". Imperatives are fine in
  instructions. The dual-licence boilerplate in the README is standard legal
  text and stays as it is.
- **Bold is for bullet lead-ins only** — the first word or phrase of a list item.
  No bold mid-sentence, none in table cells, none opening a paragraph. Italics
  are for genuine contrast, used sparingly.
- **No decorative icons.** Write "yes" and "no" in tables, not ✅ and ❌.
- **Do not sell.** Avoid "the whole point", "load-bearing", "genuinely",
  "crucially", "deliberately", "notably". State the fact and stop.
- **Plain words, short sentences.** Prefer "use" over "utilize", "about" over
  "approximately", "does nothing" over "is inert".
- **Understate the caveats.** "Nobody has run it on macOS yet" beats "a critical
  unverified gap".
