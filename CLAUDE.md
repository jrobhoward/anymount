# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`anymount` mounts a read-only filesystem from user space on Linux, macOS and
Windows. An implementor writes one trait, `ReadOnlyFs`; the crate mounts it with
whatever mechanism the host OS provides — FUSE on Linux, NFS on macOS, the
Cloud Files API (cfapi) on Windows. One backend per platform, not a shared
mechanism across platforms — see `docs/PLAN.md`'s "Revised decision" for why
FUSE and WebDAV were both set aside on macOS in favor of NFS.

**macOS has no backend built yet.** The decision is NFS (`docs/PLAN.md`), and
the mechanism is spiked and proven there, but `backend/nfs.rs` does not exist
in the tree — mounting on macOS currently compiles fine and returns
`FsError::Unsupported` at mount time. There is no `fuse` module, feature, or
`Backend` variant scoped to macOS; `fuse` is Linux-only. Do not reach for
macFUSE as a stopgap without revisiting that decision first.

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
  `fuse.rs` (Linux only), `cfapi.rs` (Windows). No `nfs.rs` yet — see "What
  this is". `backend/mod.rs` resolves `Backend::Auto` and owns the
  `MountHandle` enum.

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
the real verdict.

**Verify mounts with real tools, not process output.** A backend that prints
"mounted" has proved nothing. Check `mount`, then `ls -lR`, `cat`, `stat`, and a
`sha256sum` compared against a digest computed independently of the mount. The
`mount-smoke-test` CI job does exactly this and is the template.

## Definition of Done

Before considering any task or phase complete:

- `cargo test` passes with zero failures
- `cargo clippy --all-targets -- -Dwarnings` is clean
- `cargo fmt --all -- --check` is clean
- `cargo deny check licenses bans sources` passes
- The two cross-compile checks pass
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
