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

# Rustdoc, with broken intra-doc links treated as errors. Set on this command
# only, never in a shared `env:` block — see "Never set RUSTFLAGS" below.
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# Cross-compile checks for the other OS backends (no linker needed for
# `check`). Both need `--all-targets`: without it the `*_tests.rs` files are
# not compiled, and a `cfg`-gated test referring to something that has been
# renamed sails straight through.  `fuse` is Linux-only and a no-op off it, so
# no default-features flag is needed.
cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -Dwarnings
cargo check --target aarch64-apple-darwin --all-targets

# Supply chain — run before adding or updating any dependency. `advisories`
# also runs weekly in CI, since the database changes with no commit here.
cargo deny check licenses bans sources advisories

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
- `error.rs` — `FsError`, mapped to `errno` (available on every platform, not
  only Unix, so the public surface has one shape everywhere) and to
  `HRESULT`/`NTSTATUS` on Windows. `FsError::context` attaches an explanation
  while keeping the underlying errno. `From<FsError> for io::Error` returns an
  `FsError::Io` whole rather than rebuilding it, so its raw OS error survives.
- `mount.rs` — `MountBuilder` and `Mount`. `Mount` unmounts on drop;
  `Mount::unmount` surfaces the errors that drop swallows.
- `backend/` — one module per OS mechanism, `cfg`-gated *and* feature-gated:
  `fuse.rs` (Linux only), `nfs/` (a submodule tree: `mod.rs`, `xdr.rs`,
  `rpc.rs`, `mount_proto.rs`, `nfs_proto.rs`, `handle.rs`, `server.rs`),
  `cfapi.rs` (Windows). Only `nfs::mount` and `NfsHandle` are macOS-gated; the
  wire layer around them builds and tests on any Unix, for the same reason
  `readdir.rs` is unconditional — see "The NFS wire layer" below.
  `backend/mod.rs` resolves
  `Backend::Auto`; it and its sibling modules hold the seams every backend
  shares, so a backend supplies only a `mount` function, a `Mounted` impl and
  a `Caps`:
  - `Mounted` — the trait a backend's live handle implements. `unmount`
    consumes the handle, so `Mount` calls it from both `Mount::unmount` and
    its own `Drop` and it runs exactly once. Unmount-on-drop is therefore a
    guarantee the crate makes uniformly; a backend needs no `Drop` and no
    idempotence flag of its own.
  - `preflight.rs` — `Caps` (what a backend can honor) and `check`, run
    before any platform code. Covers `allow_other`, `auto_unmount`, `threads`,
    and whether the mountpoint must be empty. See "Unsupported builder
    options" below.
  - `readdir.rs` — the `.`/`..` cookie arithmetic *and* `emit`, the driver
    that walks one paginated, resumable listing. `fuse.rs` and
    `nfs/nfs_proto.rs` differ only in their `Sink` closure; `cfapi.rs` passes
    `Dots::Omit` and reuses the pagination alone. Compiled and tested
    unconditionally rather than gated to a backend.
  - `trace.rs` — `backend_warn!`/`backend_info!`, no-ops without the
    `tracing` feature. Use these where an error is deliberately discarded
    (a failed `release`, a failed unmount during `drop`); never `let _ =`.

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

**Unsupported builder options are a hard error, not a no-op.** `allow_other`,
`auto_unmount` and `threads` are FUSE-only — the first two are FUSE mount
options with no NFS or cfapi counterpart, and FUSE is the only backend that
owns a worker pool. Rather than silently ignoring them off Linux, each backend
declares a `Caps` and `preflight` (`backend/preflight.rs`) rejects the request
at `mount()` naming the backend. `preflight` also checks the mountpoint exists
and is a directory, so all three platforms report the same actionable error
instead of passing through whatever their helper binary prints. A new backend
adds a `Caps`, not a fourth policy.

**cfapi requires an empty mountpoint, and clears it on unmount.** A sync root
projects placeholders *into* the directory rather than covering it the way a
Unix mount does, so mounting over existing files would destroy them.
`Caps::empty_mountpoint` is how that is enforced, and
`remove_leftover_placeholders` deletes only entries still carrying
`FILE_ATTRIBUTE_REPARSE_POINT`. Do not widen that check to "delete everything
found here" — that was the pre-1.0 behaviour and it was a data-loss bug.

**cfapi's placeholder descriptors must outlive the `CfExecute` call that reads
them.** `CF_PLACEHOLDER_CREATE_INFO` holds raw pointers with no lifetime, so
the compiler cannot check this. `Placeholders::with_descriptors` builds the
array and borrows its backing store for the duration of the call; a function
that *returns* the array hands back dangling pointers instead. Do not
reintroduce one.

**`ReadOnlyFs::readdir` may return a partial page.** An empty result is the
only end-of-directory signal, and `backend/readdir.rs`'s `emit` loops until it
gets one. A backend must never call `fs.readdir` directly and treat the result
as the whole tail: NFS turns one `emit` call into one `dirlist3` and cfapi into
one `TRANSFER_PLACEHOLDERS`, so a short page there reads as a complete
directory, with no error to show for it. FUSE hides this — its kernel client
reissues `readdir` from the last cookie regardless — so a mount smoke test on
Linux will not catch it. Test paging at the `emit` or procedure layer instead.

**The NFS wire layer builds on any Unix, not only macOS.** `xdr.rs`, `rpc.rs`,
`handle.rs`, `mount_proto.rs`, `nfs_proto.rs` and `server.rs` contain no macOS
API, so they compile and test wherever `cargo test` runs — which is what makes
a change to a shared seam like `readdir::emit` checkable without a Mac. They
carry a scoped `dead_code` allow off macOS because nothing calls them there.
`FileHandle3::new_random` stays macOS-only on purpose; tests use
`from_secret`/`for_test` rather than a portable randomness fallback that could
later be mistaken for a production path.

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

**Public docs:** every new public item needs a doc comment — `missing_docs` is
on, so this is enforced rather than asked for. Doc examples that use a gated
API must be `cfg`-gated too; `cargo test` runs them. `README.md`'s example is
compiled as a doctest via `#[cfg(doctest)]` in `lib.rs`, so it cannot drift.
Rustdoc must not link to a repository-relative path like `docs/GAPS.md` — a
docs.rs reader cannot follow one; use the absolute GitHub URL there and keep
relative links in the Markdown files.

**Errors:** `thiserror`, in `error.rs`. `FsError` is `#[non_exhaustive]`.

**The public API is frozen at 1.0.** `ReadOnlyFs`'s method set, the value types
in `types.rs`, `MountBuilder` and `Mount` do not change shape without a major
version. `Backend` and `FsError` are `#[non_exhaustive]` so a variant can be
added; the value types deliberately are not, so implementors can keep building
them with struct literals — that was weighed before 1.0 and declined, and is
not planned for revisiting. Adding a field to `FileAttr` or a variant to
`FileKind` is a 2.0, not a minor release. Additive changes (a new default trait
method, a new builder option, a new trait impl) are minor releases and need a
`CHANGELOG.md` entry.

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
just a couple of hand-picked examples — `backend/readdir_tests.rs` is the
template: cookie encode/decode, plus a multi-call pagination property that
drives the real `readdir::emit` rather than a simulation of it. Prefer
restructuring production code into a testable pure function over
re-implementing its logic in the test file. Don't reach for it for ordinary example-based
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
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` is clean (broken intra-doc
  links fail CI)
- `cargo fmt --all -- --check` is clean
- `cargo deny check licenses bans sources advisories` passes
- The two cross-compile checks pass
- `cargo +1.88.0 check --all-targets` (MSRV) passes
- No `.unwrap()` / `.expect()` in production code
- New public items have doc comments (`missing_docs` catches this)
- `CHANGELOG.md` has an entry for anything a user would notice
- If behaviour changed on a platform, it was verified with real tools there, or
  the fact that it was not is stated plainly
- Anything touching `backend/mod.rs`, `backend/readdir.rs` or a `Mounted` impl
  is checked on every target with `--all-targets`, not just the host

## Docs are part of "done"

Each file has one job; keep changes in the right one rather than restating
across them:

| File | Holds | Scope |
|---|---|---|
| `README.md` | What the crate does, how to use it, and the caveats that change how it should be used | Link out rather than expand |
| `docs/PLAN.md` | Phases, decisions and their rationale, open questions | The plan of record; update when a phase closes or a decision changes |
| `docs/GAPS.md` | Every known limitation, why it exists, and what changing it costs | One section per gap. Add to it rather than quietly narrowing scope |
| `CHANGELOG.md` | What changed in each release, and enough of why to act on it | Keep a Changelog format. One entry per released version |
| `SECURITY.md` | How to report a vulnerability, and what is in scope | Reporting process and scope, not a list of known issues — those go in `docs/GAPS.md` |
| `CLAUDE.md` | Conventions and constraints a contributor needs before editing | Rules, not narrative |

`docs/macos_nfs_plan.md` and `docs/v1-readiness.md` are closed working
documents, kept for their reasoning and excluded from the published package.
Neither is updated; anything still true belongs in the files above.

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
