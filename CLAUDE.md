# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`anymount` mounts a read-only filesystem from user space on Linux, macOS and
Windows. An implementor writes one trait, `ReadOnlyFs`; the crate mounts it
with whatever mechanism the host OS provides — FUSE on Linux, a from-scratch
NFSv3 server on macOS (`backend/nfs/`, mounted with the OS's built-in
`mount_nfs`, no macFUSE/kernel extension/root), the Cloud Files API (cfapi)
on Windows. One backend per platform, not a shared mechanism across
platforms. There is no `fuse` module/feature/`Backend` variant on macOS and
no `projfs` module/feature/`Backend` variant anywhere — both were evaluated
and cut, not deferred. See `docs/ARCHITECTURE.md` for why.

**Read-only is the scope, not a stage.** Write operations report `EROFS`.
`docs/GAPS.md` lists every limitation and what changing it costs.

**1.0 is feature-complete.** The API is frozen (see "The public API is
frozen at 1.0" below). New capabilities are out of scope for 1.x and would
likely need a 2.0 given the frozen, non-`#[non_exhaustive]` value types.

See `docs/ARCHITECTURE.md` for the module map and design rationale,
`docs/GAPS.md` for known limitations, and `README.md` for what the crate does
and how to use it.

## Commands

```bash
# Build
cargo build --all-targets

# Test
cargo test
cargo test --test trait_shape                    # integration tests only
cargo test some____test____name                  # single test

# Lint (must be clean before any change is considered done)
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

See `docs/ARCHITECTURE.md` for the module map, the mermaid diagram, and why
each backend was chosen over its alternatives. Summary a contributor needs
day to day:

- Single crate, edition 2024, `rust-version = 1.88.0`.
- `backend/mod.rs`, `backend/preflight.rs`, `backend/readdir.rs` and the
  `Mounted` trait are the shared seams every backend goes through. A new
  backend supplies a `mount` function, a `Mounted` impl, and a `Caps` — not a
  fourth policy bolted onto one of those seams.
- The NFS wire layer (`xdr.rs`, `rpc.rs`, `handle.rs`, `mount_proto.rs`,
  `nfs_proto.rs`, `server.rs`) builds and tests on any Unix; only `mod.rs`'s
  `mount` and `NfsHandle` are macOS-gated. It carries a scoped `dead_code`
  allow off macOS.

## Platform constraints worth knowing before editing a backend

**Unsupported builder options are a hard error, not a no-op.** `allow_other`,
`auto_unmount` and `threads` are FUSE-only. Each backend declares a `Caps`,
and `preflight` (`backend/preflight.rs`) rejects an unsupported request at
`mount()` naming the backend, rather than silently ignoring it. A new backend
adds a `Caps`, not a fourth policy.

**cfapi requires an empty mountpoint, and clears it on unmount.** A sync root
projects placeholders *into* the directory rather than covering it, so
mounting over existing files would destroy them. `Caps::empty_mountpoint`
enforces the precondition; `remove_leftover_placeholders` deletes only
entries still carrying `FILE_ATTRIBUTE_REPARSE_POINT`. Do not widen that
check to "delete everything found here" — that was a pre-1.0 data-loss bug.

**cfapi's placeholder descriptors must outlive the `CfExecute` call that reads
them.** `CF_PLACEHOLDER_CREATE_INFO` holds raw pointers with no lifetime, so
the compiler cannot check this. `Placeholders::with_descriptors` builds the
array and borrows its backing store for the duration of the call; a function
that *returns* the array hands back dangling pointers instead. Do not
reintroduce one.

**`ReadOnlyFs::readdir` may return a partial page.** An empty result is the
only end-of-directory signal, and `backend/readdir.rs`'s `emit` loops until it
gets one. A backend must never call `fs.readdir` directly and treat the result
as the whole tail: a short page there reads as a complete directory, with no
error to show for it. FUSE hides this — its kernel client reissues `readdir`
from the last cookie regardless — so a mount smoke test on Linux will not
catch it. Test paging at the `emit` or procedure layer instead.

**FUSE `auto_unmount` requires a non-`Owner` ACL.** `fusermount3` refuses to
arm it on an owner-private mount, so `auto_unmount` defaults off and
`mount()` rejects the combination with an explanation.

**Linux does not link libfuse.** `fuser` is built with
`default-features = false`, so mounting goes through the `fusermount3`
binary. That keeps LGPL code out of the link and allows unprivileged mounts.
Do not add `fuser`'s default features to the Linux target.

**NFS authorizes with a secret embedded in the file handle, not `AUTH_SYS`.**
`AUTH_SYS` trusts client-supplied uid/gid with no verification. `FileHandle3`
(`backend/nfs/handle.rs`) embeds a per-mount random 128-bit secret in every
handle this server hands out, and the same secret is required as a literal
path segment in `MNT` (`/export/<hex secret>`). Do not add real credential
checking on top of this without revisiting `docs/ARCHITECTURE.md`'s platform
constraints — it is a deliberate choice, not an oversight.

**NFS mounts `soft` with a short `timeo`/`retrans`, not classic NFS `hard`
semantics.** `soft,timeo=20,retrans=2` (`backend/nfs/mod.rs`'s `mount_nfs`
invocation) turns a crashed server into a bounded `Operation timed out`
instead of a hang behind macOS's own "Server connections interrupted"
dialog. Do not remove these options to "fix" a slow mount; the fix belongs in
the server, not in loosening the client's patience.

**NFS's RPC framing supports only single-fragment messages.** `rpc.rs`'s
`read_message` closes the connection on a multi-fragment ONC RPC message
rather than reassembling one. See `docs/GAPS.md` if a client that needs
reassembly ever shows up.

## Licensing is a design constraint

The crate is MIT OR Apache-2.0 with no copyleft anywhere in the dependency
graph. The obvious bindings for these platform APIs are all copyleft, so the
crate routes around them:

| Avoided | Licence | Used instead |
|---|---|---|
| `winfsp`, `winfsp-sys` | GPL-3.0 | nothing; WinFsp is out of scope |
| `windows-projfs` | GPL-2.0 | Microsoft's `windows` crate |
| `dokan`, `dokan-sys` | wrap LGPL Dokany | cfapi |
| libfuse (linked) | LGPL | `fusermount3` on Linux (the only platform that links FUSE at all) |

`deny.toml` bans those crates by name, so an accidental `cargo add` fails
loudly rather than quietly relicensing the crate. The licence allow-list is
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

**The public API is frozen at 1.0.** `ReadOnlyFs`'s method set, the value
types in `types.rs`, `MountBuilder` and `Mount` do not change shape without a
major version. `Backend` and `FsError` are `#[non_exhaustive]` so a variant
can be added; the value types deliberately are not, so implementors can keep
building them with struct literals. Adding a field to `FileAttr` or a variant
to `FileKind` is a 2.0, not a minor release. Additive changes (a new
default trait method, a new builder option, a new trait impl) are minor
releases and need a `CHANGELOG.md` entry.

**Unsafe:** the crate is `#![forbid(unsafe_op_in_unsafe_fn)]`. FFI calls need
an explicit `// SAFETY:` comment saying why the call is sound — what the
arguments point at, what the callee does with them, and what is kept alive.

**Platform code:** keep OS-specific FFI behind `backend/`, `cfg`-gated;
platform-specific tests are `cfg`-gated too. **CI is where Windows and macOS
code actually executes.** The cross-compile commands above are the local
pre-push check: they prove the other backends type-check, not that they
work. Run them before calling a change done, review FFI carefully, and expect
CI to be the real verdict. For code that is not backend-specific FFI but
still needs a Unix/non-Unix split — `types.rs`'s `current_uid`/`current_gid`,
`error.rs`'s `to_errno` — use a pair of `#[cfg(unix)]` / `#[cfg(not(unix))]`
functions of the same name and signature, not an inline `cfg!` branch.

**Verify mounts with real tools, not process output.** A backend that prints
"mounted" has proved nothing. Check `mount`, then `ls -lR`, `cat`, `stat`, and
a `sha256sum` compared against a digest computed independently of the mount.
The `mount-smoke-test` CI job does exactly this and is the template.

**Property tests:** reach for `proptest` (dev-dependency) when a pure
function has a round-trip or arithmetic invariant worth checking across many
inputs, not just a couple of hand-picked examples — `backend/readdir_tests.rs`
is the template. Prefer restructuring production code into a testable pure
function over re-implementing its logic in the test file. Don't reach for it
for ordinary example-based behavior; a `subject____condition____result` unit
test is still the default.

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

Before considering any change complete:

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
| `docs/ARCHITECTURE.md` | Module map, diagram, and why each backend was chosen over its alternatives | Update when a design decision changes; not a development log |
| `docs/GAPS.md` | Every known limitation, why it exists, and what changing it costs | One section per gap. Add to it rather than quietly narrowing scope |
| `CHANGELOG.md` | What changed in each release, and enough of why to act on it | Keep a Changelog format. One entry per released version |
| `SECURITY.md` | How to report a vulnerability, and what is in scope | Reporting process and scope, not a list of known issues — those go in `docs/GAPS.md` |
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
- **No development-phase framing.** Don't attribute a fact to "the Phase N
  spike" or narrate how a decision was reached over time. State the current
  fact and, if the reasoning matters, the reasoning — not the history of
  getting there. This is a maintained 1.0 crate, not a project log.
