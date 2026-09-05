# 1.0 readiness review

> Closed. Every finding below was acted on for 1.0.0 — see `CHANGELOG.md` for
> what changed and `docs/PLAN.md`'s Phase 2.5 for the decisions behind it.
> Kept as the record of what the review found, not as outstanding work.
>
> One correction, established while fixing it: item 4 claims a paging
> `readdir` makes the directory "appear short" under FUSE. It does not. FUSE's
> kernel client reissues `readdir` from the last cookie regardless of what the
> reply claimed, so it re-drove the listing itself and was correct by
> accident. The truncation was real on NFS and cfapi only, where one pass
> through `emit` produces one complete reply. The fix and the reasoning behind
> it stand; the blast radius was smaller than stated, and in a way that
> explains why the Linux smoke test never caught it.

A review of `anymount` at commit `8ee56b5` against three questions: is it ready
for 1.0, does it follow Rust idiom, and does the documentation carry its
weight. Findings are ordered by what blocks a 1.0 tag.

Current state of the gates: `cargo test` (48 tests), `cargo clippy
--all-targets -- -Dwarnings`, `cargo fmt --all -- --check`, `cargo deny check
licenses bans sources advisories`, both cross-compile checks, and `cargo
+1.88.0 check --all-targets` are all clean. Nothing below was found by a gate;
all of it was found by reading.

## Verdict

Not ready for 1.0 yet. The trait, the error type, the mount lifecycle and the
Linux and macOS backends are in good shape — the seam design (`Mounted`,
`preflight::Caps`, `readdir::emit`) is genuinely well factored and the safety
commentary around FFI is above average. Two memory-safety and data-loss defects
in the Windows backend, one unwritten trait contract that all three backends
depend on, and a set of API-freeze decisions that have not been made yet stand
between the current tree and a version number that promises stability.

The gap between "works on the machine it was demonstrated on" and "1.0" here is
mostly Windows. cfapi is the only backend with no runtime coverage in CI, and
it is the only backend with a confirmed defect.

## P0 — blocks 1.0

### 1. Use-after-free in the cfapi directory listing

`list_placeholders` (`src/backend/cfapi.rs:426`) builds a local `prepared`
vector of `([u8; 8], Vec<u16>, FileAttr)` tuples, then returns
`CF_PLACEHOLDER_CREATE_INFO` values whose `RelativeFileName` and `FileIdentity`
are raw pointers into those tuples:

```rust
Ok(prepared
    .iter()
    .map(|(id, name, attr)| to_create_info(name, id, attr))
    .collect())
```

`prepared` is dropped when the function returns. Every returned
`CF_PLACEHOLDER_CREATE_INFO` therefore carries a dangling pointer, and
`handle_fetch_placeholders` (`src/backend/cfapi.rs:414`) hands the whole array
straight to `CfExecute`. Every directory enumeration on Windows reads freed
memory. The in-file comment above the `Ok(...)` reasons only about `prepared`
not reallocating, which is true and not the problem.

The type system cannot catch this because `CF_PLACEHOLDER_CREATE_INFO` holds
raw pointers with no lifetime. `to_create_info____carries_the_name_pointer_and_identity_through`
(`src/backend/cfapi_tests.rs:82`) keeps its `name` alive across the assertion,
so it cannot catch it either.

*Fix:* return the owned backing store alongside the descriptors and keep it
alive across the `CfExecute` call — for example have `list_placeholders`
return `(Vec<(…)>, Vec<CF_PLACEHOLDER_CREATE_INFO>)`, or restructure so the
descriptors are built inside `transfer_placeholders` where the backing store is
still in scope. A borrowing wrapper struct that ties the descriptor array to
the store with a lifetime would make the constraint checkable rather than
commented.

*Test:* `list_placeholders` needs no live sync root — it takes `&F` and returns
plain structs. A unit test that builds a listing, drops nothing, and compares
`RelativeFileName` against the expected names would fail today under Miri or a
sanitizer, and would likely fail in practice under a debug allocator.

### 2. Unmount deletes the contents of the mountpoint

`remove_leftover_placeholders` (`src/backend/cfapi.rs:167`, called from
`unmount` at `:151`) iterates `read_dir(mountpoint)` and calls `remove_file` or
`remove_dir_all` on every entry, unconditionally. Nothing distinguishes a
placeholder this backend created from a file that was already there.

`preflight::check_mountpoint` (`src/backend/preflight.rs:51`) requires only
that the path exists and is a directory. cfapi projects *into* the mountpoint
rather than covering it the way a Unix mount does, so a caller who mounts over
a directory that already holds files loses them on unmount. `MountBuilder::new`
documents the Windows mountpoint as "the virtualisation root" without saying it
must be empty or that its contents are destroyed.

*Fix:* two changes, both worth making.

- Add an `empty_mountpoint: bool` to `preflight::Caps`, set on cfapi only, and
  reject a non-empty mountpoint at `mount()` with an explanation. This matches
  the existing pattern exactly — a backend adds a `Caps`, not a policy.
- Delete only entries this backend owns. Check
  `FILE_ATTRIBUTE_REPARSE_POINT` and the cloud reparse tag before removing, or
  track the names created during the mount's life. Belt and braces: the
  emptiness check makes the common case safe, the tag check makes the
  destructive path narrow.

Document the behaviour in `README.md` and `docs/GAPS.md` either way: "the
Windows mountpoint must be empty and is cleared on unmount" is a caveat that
changes how the crate is used.

### 3. cfapi has no runtime coverage anywhere

`.github/workflows/ci.yml` runs a real mount smoke test on Linux
(`mount-smoke-test`) and macOS (`nfs-mount-smoke-test`). The `windows-latest`
leg of `build` compiles and runs unit tests but never mounts. Both defects
above live in code no automated run has ever executed.

Meanwhile `README.md:29` says "All three backends are built and verified end to
end" and `README.md:41` describes cfapi as verified against `dir`/`type`,
subdirectory population, and checksummed multi-megabyte reads. That was a
manual verification and is not repeatable.

*Fix:* add a `cfapi-mount-smoke-test` job on `windows-latest` mirroring the
other two — build `memfs`, mount, `dir`, `type`, compare a `CertUtil -hashfile`
digest against one computed outside the mount, recurse into `subdir`, unmount,
confirm the mountpoint is empty and unregistered. Until that job exists and is
green, no version number should claim Windows is verified.

## P1 — should be settled before 1.0

### 4. `readdir`'s real contract is unwritten, and all three backends depend on it

`readdir::emit` (`src/backend/readdir.rs:133`) calls `fs.readdir(dir, offset)`
exactly once and iterates the result. If an implementation returns a bounded
page rather than every remaining entry, `emit` returns `Ok(true)` — exhausted —
and the listing silently truncates. Under FUSE the directory appears short;
under NFS `dirlist3.eof` is set early; under cfapi the missing entries never
become placeholders at all.

`src/backend/cfapi.rs:40` asserts the contract exists — "`ReadOnlyFs::readdir`
is documented to return every remaining entry from a given offset, not a capped
page" — but `src/fs.rs:71` says only "List directory `ino`, skipping the first
`offset` entries." Nothing tells an implementor that returning a page is wrong,
and `emit` does not defend against it.

There is a related scaling problem behind it. FUSE calls `readdir` with a
buffer of a few kilobytes, so listing a 100k-entry directory issues many calls,
each of which asks the implementor to materialise the entire remaining tail —
quadratic work and allocation for large directories.

*Fix:* resolve both at once. Loosen the contract to "return at least one entry
and at most all remaining entries; an empty result means the listing is
exhausted", state that in `ReadOnlyFs::readdir`'s docs, and make `emit` loop
until the sink reports `Full` or `readdir` returns empty. That makes bounded
pages legal and correct on every backend, removes the quadratic behaviour for
implementors that want it, and keeps every existing implementation working
unchanged. Extend `backend/readdir_tests.rs`'s pagination property to drive a
fixture that pages, which is the test that would have caught this.

### 5. Thirty-three public items have no documentation

`src/lib.rs:43` enables `forbid(unsafe_op_in_unsafe_fn)` and
`warn(missing_debug_implementations)` but not `missing_docs`. Turning it on
reports 33 warnings: every `FsError` variant, both `FsError::Context` fields,
both `FileKind` variants, and every public field of `FileAttr`, `DirEntry` and
`StatFs`.

CLAUDE.md already requires a doc comment on every new public item. The lint is
what makes that requirement hold.

*Fix:* add `#![warn(missing_docs)]` to `src/lib.rs` and write the 33 comments.
Several are load-bearing rather than decorative — `FileAttr::nlink` and
`StatFs::frsize` versus `bsize` both have non-obvious expected values, and
`FsError::Unsupported` versus `FsError::ReadOnly` is a distinction an
implementor has to get right.

### 6. docs.rs will publish only the Linux surface

There is no `[package.metadata.docs.rs]` in `Cargo.toml`. docs.rs builds on
`x86_64-unknown-linux-gnu`, so `probe::cfapi`, `probe::PlatformInfo` and every
`cfg`-gated remark about the NFS and cfapi backends will be absent from the
published documentation of a crate whose entire selling point is that it spans
three platforms.

*Fix:*

```toml
[package.metadata.docs.rs]
all-features = true
targets = [
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
]
rustdoc-args = ["--cfg", "docsrs"]
```

and gate the platform-specific public items with
`#[cfg_attr(docsrs, doc(cfg(...)))]` so a reader can see which platform an item
belongs to. This needs `#![cfg_attr(docsrs, feature(doc_cfg))]` in `src/lib.rs`,
which is nightly-only and must stay behind the `docsrs` cfg so stable builds
are unaffected.

While in `Cargo.toml`: the `description` field still reads "FUSE on Linux,
Cloud Files on Windows" and omits macOS entirely. That string is what appears on
crates.io. `keywords` omits `nfs`.

### 7. Semver freeze decisions have not been made

A 1.0 freezes these shapes. Three deserve an explicit decision rather than a
default.

- `FileKind` (`src/types.rs:22`) is a plain two-variant enum, and
  `docs/GAPS.md` names "add `FileKind::Symlink` plus a `readlink` op" as the
  documented path to symlink support. Adding a variant post-1.0 is breaking.
  Recommendation: mark it `#[non_exhaustive]`. The cost to implementors is a
  wildcard arm; the alternative is a 2.0 for a change already planned.
- `FileAttr`, `DirEntry` and `StatFs` have all-public fields and no
  `#[non_exhaustive]`. Marking them would forbid downstream struct-literal and
  functional-update construction, which is exactly how implementors build a
  `FileAttr` today — so this is a real trade, not a free win. Recommendation:
  leave them exhaustive, and say so in their docs, so the freeze is a stated
  promise rather than an accident. `StatFs` is the one worth reconsidering: it
  implements `Default`, it is only ever consumed by the crate, and NFS's
  `FSSTAT3` has fields the struct does not carry yet.
- `FsError::to_errno` (`src/error.rs:109`) is `pub` under `#[cfg(unix)]`, while
  its siblings `to_nfsstat3` and `to_ntstatus` are `pub(crate)`. A downstream
  crate that calls `to_errno` fails to compile on Windows. Either make it
  available on all platforms (returning the same POSIX numbers, which are
  meaningful as a portable error taxonomy) or make it `pub(crate)` and keep the
  public surface identical everywhere.

An `assert_send`/`assert_sync` compile-time test over `Mount`, `MountBuilder`
and `FsError` would lock in auto traits that hold today by accident of the
current backends' field types. `Mount` holding a `Box<dyn Mounted>` is `Send +
Sync` only because `Mounted: Send + Sync`; that is enforced, but nothing
enforces that `Mount` itself stays so if a field is added.

## P2 — worth doing, not blocking

### 8. NFS server hardening

The server is reachable by any local process that finds the loopback port; the
handle secret is what stops it from being useful, not the network boundary.
Three robustness items:

- `server::run` (`src/backend/nfs/server.rs:31`) pushes a `JoinHandle` per
  accepted connection into `workers` and never reaps them until shutdown.
  A client that connects repeatedly grows the vector and the thread count
  without bound. Cap concurrent connections, and drain finished handles
  periodically.
- `nfs_proto::read` (`src/backend/nfs/nfs_proto.rs:292`) computes `offset + n as
  u64` on a client-supplied offset. A near-`u64::MAX` offset overflows: a panic
  in debug builds, a wrong `eof` flag in release. `fsstat`
  (`src/backend/nfs/nfs_proto.rs:446`) multiplies implementor-supplied `blocks`
  by `frsize` with the same exposure. Use `saturating_add`/`saturating_mul`.
- `build_dirlist` (`src/backend/nfs/nfs_proto.rs:358`) returns zero entries and
  `eof: false` when the client's `maxcount` cannot fit even one entry, which
  invites a client loop that never advances. RFC 1813 has `NFS3ERR_TOOSMALL`
  for exactly this. There is already a test asserting the current behaviour
  (`readdirplus____budget_smaller_than_one_entry____returns_zero_entries_and_a_resumable_cookie`),
  so this is a deliberate choice worth revisiting rather than an oversight.

The non-constant-time secret comparison is already catalogued in
`docs/GAPS.md`; no change needed, the gap entry is the right treatment.

### 9. The NFS wire layer is only testable on macOS

`backend/nfs/` is gated to `target_os = "macos"`, so `xdr.rs`, `rpc.rs`,
`nfs_proto.rs` and `mount_proto.rs` — which are pure byte-slice manipulation
with no platform API in them — compile and test only on the macOS CI leg. A
Linux developer changing `readdir::emit` cannot run the NFS tests that cover
its NFS-side consumer.

`backend/readdir.rs` already sets the precedent: compiled and tested
unconditionally because nothing in it touches a platform API. The same applies
to the four files above; only `mod.rs` (which shells out to `mount_nfs` and
calls `libc::unmount`) and `server.rs` genuinely need the gate.

`rpc.rs` also has no tests at all, despite being the layer that parses
untrusted bytes off a socket. `read_message`'s fragment handling,
`read_call_header`'s three outcomes, and the oversize and truncation rejections
are all straightforward to test once the module compiles everywhere.

### 10. CI additions

- `cargo deny check advisories` is configured in `deny.toml` but the CI job runs
  only `licenses bans sources`. Advisories are the check that goes stale on its
  own without any commit, so it also wants a `schedule:` trigger rather than
  only push and pull-request.
- `cargo doc --no-deps` runs without `-Dwarnings`, so a broken intra-doc link
  ships silently. Set `RUSTDOCFLAGS: -D warnings` on that step alone — not in
  the workflow-level `env:` block, for the reason the existing comment gives.
- The MSRV job runs `cargo check --all-targets` but not `cargo test`. Cheap to
  add and catches test-only API drift below the floor.
- `cargo publish --dry-run` on the release path would catch a packaging problem
  before the tag rather than after.

### 11. Convention and style drift

- `src/backend/fuse.rs:314` registers `mod fuse_tests` mid-file, with
  `to_fuser_attr` defined after it. CLAUDE.md says the registration goes at the
  bottom.
- `src/lib.rs:3` opens with "You implement one trait", and `src/lib.rs:14` uses
  bold mid-sentence. Both violate the writing-style rules the project sets for
  its own rustdoc. `README.md:19` puts bold inside a table cell; `docs/GAPS.md`
  uses bold mid-sentence in several places (lines 41, 101, 115, 135, 183, 195).
- Rustdoc refers to `docs/GAPS.md` and `docs/PLAN.md` as bare repo-relative
  paths (`src/lib.rs:18`, `src/fs.rs:42`, and others). A docs.rs reader cannot
  follow those. Use absolute GitHub URLs in rustdoc, keep relative paths in the
  Markdown files.
- `src/lib.rs:57` describes the `probe` module as "for diagnostics and the
  Phase 0 spikes". Phase numbering is internal planning vocabulary and does not
  belong in published API documentation. The same applies to
  `src/backend/readdir.rs:55`'s reference to "Phase 2".
- `error.rs:120` writes `Self::NoXattr => 93, // ENOATTR` on macOS. `libc`
  exports `ENOATTR` on that target; the named constant beats the literal.

### 12. Documentation gaps

`docs/PLAN.md` and `docs/GAPS.md` are unusually thorough and need no
restructuring — GAPS in particular is the strongest document in the repo and
should not be touched beyond the additions noted above. Three things are
missing around them:

- No `CHANGELOG.md`. A 1.0 needs one, and it needs a `0.1.0` entry written
  retroactively.
- `docs/macos_nfs_plan.md` (722 lines) is a fourth documentation file that
  CLAUDE.md's docs table does not account for, and its own header admits parts
  are stale ("`backend/nfs.rs` itself was never built"). It ships in the
  published package. Either fold what is still true into `docs/PLAN.md`'s Phase
  0.6, or add it to a `Cargo.toml` `exclude` list so it stays in the repo
  without shipping.
- No `SECURITY.md`. The crate stands up a network-reachable RPC server on macOS
  and parses untrusted bytes; a stated reporting channel is proportionate.

## README

Rewrite rather than patch. The current text is accurate but is organised around
the project's own history — three of its seven sections are about decisions that
were made and reversed. A reader arriving from crates.io wants to know what the
crate does, how to use it, and what it will not do, in that order.

What is missing outright:

- No installation line. No `cargo add anymount`, no version badge, no docs.rs
  badge, no MSRV statement.
- No feature-flag table. `tracing` is undocumented outside `docs/PLAN.md`; a
  reader has no way to learn it exists.
- No `ReadOnlyFs` implementation. The one code block starts from `my_fs`, which
  is never defined, and would not compile if it were ever doctested — a bare
  `?` outside a function. The whole premise of the crate is "implement one
  trait", and the README never shows the trait. A trimmed `memfs` — one file,
  one directory — is roughly thirty lines and would carry the page.
- No statement that `Mount` unmounts on drop, which is the single most
  important lifecycle fact about the API.
- No mention that the Windows mountpoint must be empty (see P0 item 2).

What should shrink: the "Status" section's history of three macOS mechanisms
belongs in `docs/PLAN.md`, which already tells it better. One sentence and a
link is enough. The licensing section is four bullets where two would do — the
detail is already in `CLAUDE.md` and `deny.toml`.

What should stay: the platform table, the "Why not just use an existing crate?"
paragraph, and "Opening the mount in a file manager", which answers a real
question a caller will have and explains a deliberate non-feature.

Consider `#![doc = include_str!("../README.md")]` once the example compiles, so
`cargo test` keeps the README honest.

## Rust idiom

Broadly good, and better than average in the places that matter for this kind
of crate.

What is done well: the `Mounted` trait taking `self: Box<Self>` so teardown
runs exactly once from either `unmount` or `Drop` is the right shape and is
correctly reasoned about in its own docs. `preflight::Caps` turns three
per-backend policies into one table. `readdir::emit` isolates cookie arithmetic
that would otherwise be duplicated and subtly wrong twice. The `trace` macros
expanding to `let _ = format_args!(...)` keep the no-feature build free of
unused-variable noise without a dependency. `Reader` returning `Option` on
every read rather than panicking is the correct posture for a network parser,
and the proptest that feeds it arbitrary truncated prefixes is the right test
for it. Every FFI call carries a specific `// SAFETY:` comment naming what is
kept alive — the discipline is real, not ceremonial. The `deny.toml` rationale
comments are exemplary.

Where idiom is thin:

- The public value types offer no conversions. `Ino` and `FileHandle` are
  public tuple newtypes with no `From<u64>`, no `Display`. Callers write
  `Ino(x)` and `ino.0` everywhere, which is fine, but `Display` in particular
  is missing at every log site — `backend_warn!("... ino {}", ino.0)` appears
  in both backends.
- `FsError` has no `From<FsError> for std::io::Error`. Downstream code
  bridging into `io::Result` has to write the match itself.
- The concurrency knob is hardcoded. `WORKER_THREADS = 4`
  (`src/backend/fuse.rs:43`) is not reachable from `MountBuilder`, though
  `docs/GAPS.md` cites `Config::n_threads` as the crate's answer to
  concurrency. A `.threads(n)` builder method with the current value as the
  default would cost little; if the answer is deliberately no, the reasoning
  belongs in the `MountBuilder` docs.
- `Backend` is `#[non_exhaustive]` and correctly so, but `backend::unavailable`
  (`src/backend/mod.rs:138`) matches it exhaustively with no wildcard arm. That
  is fine inside the crate and will simply fail to compile when a variant is
  added, which is the desired outcome — no change needed, noted only because it
  is the one place the `non_exhaustive` promise is load-bearing internally.

## Suggested order of work

1. Fix the cfapi use-after-free, and the mountpoint deletion, together. Add the
   Windows mount smoke test in the same change so both are proven fixed by CI
   rather than by inspection. (P0 items 1–3.)
2. Settle the `readdir` contract and make `emit` loop. (P1 item 4.)
3. Turn on `missing_docs`, write the 33 comments, add the docs.rs metadata, fix
   the `Cargo.toml` description. (P1 items 5–6.)
4. Make the semver freeze decisions and apply them. `FileKind` is the one that
   is likely to bite. (P1 item 7.)
5. Move the NFS wire layer off the macOS gate, add `rpc.rs` tests, apply the
   saturating arithmetic and the connection cap. (P2 items 8–9.)
6. CI additions, convention drift, `CHANGELOG.md`. (P2 items 10–12.)
7. Rewrite the README last, once the caveats it has to state are settled.

Steps 1 through 4 are what stand between this tree and a defensible 1.0.
Steps 5 through 7 are what make the 1.0 worth reading.
