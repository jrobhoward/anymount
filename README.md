# anymount

Mount a read-only filesystem from user space on **Linux, macOS and Windows**,
implementing one trait.

```rust
use anymount::MountBuilder;

let mount = MountBuilder::new("/mnt/restore")
    .fs_name("mybackup")
    .mount(my_fs)?;

println!("mounted at {}", mount.mountpoint().display());
mount.unmount()?;
```

| OS | Mechanism | Install burden |
|----|-----------|----------------|
| Linux | FUSE via `fusermount3` | `apt install fuse3`; **mounts unprivileged** |
| macOS | FUSE via macFUSE | macFUSE; 5.2+ on macOS 15.4+ needs no kernel extension |
| Windows | ProjFS *or* Cloud Files (cfapi) | ProjFS: one admin feature-enable. cfapi: none |

Both Windows backends project into a directory, not a drive letter.

## Status

**Phase 0 — spikes.** The FUSE backend works end to end on Linux. The Windows
and macOS paths are being brought up in that order. See [`docs/PLAN.md`](docs/PLAN.md).

| Backend | State |
|---------|-------|
| FUSE (Linux) | working — mounts, reads, random access, clean unmount |
| FUSE (macOS) | untested; same code path as Linux |
| ProjFS | `probe()` implemented; `mount()` is a stub |
| cfapi | `probe()` implemented; `mount()` is a stub |

## Why not just use an existing crate?

Nothing in Rust spans all three platforms behind one API. `fuser` and `fuse3`
are Unix-only; `winfsp`, `dokan` and `windows-projfs` are Windows-only *and*
copyleft. The nearest equivalent in any language is Go's `cgofuse`.

## Licensing

MIT OR Apache-2.0, with no copyleft anywhere in the dependency graph. That is a
design constraint, enforced in CI by `cargo deny check licenses bans`.

That rules out the obvious bindings, so `anymount` takes different routes:

- **Windows** uses Microsoft's own `windows` crate (MIT OR Apache-2.0). The
  third-party `windows-projfs` is GPL-2.0 and `winfsp`/`winfsp-sys` are GPL-3.0;
  `deny.toml` bans all three by name so an accidental `cargo add` fails loudly.
- **WinFsp is not used at all.** It is GPLv3 with a commercial licence, and a
  read-only mount needs nothing it uniquely provides (drive letters, write
  interception, memory-mapped files). Excluding it lets this crate ship with no
  licensing disclaimer.
- **Linux** builds `fuser` with `default-features = false`, mounting through the
  `fusermount3` binary rather than linking LGPL libfuse — which also enables
  unprivileged mounts.
- **macOS** reaches libfuse through macFUSE's own mount helper at runtime, so it
  is never a cargo dependency.

## Scope

Read-only; write operations report `EROFS`. Known limitations are
catalogued in [`docs/GAPS.md`](docs/GAPS.md).

## Try it

```sh
mkdir -p /tmp/anymount-demo
cargo run --example probe                          # what can this machine mount?
cargo run --example memfs -- /tmp/anymount-demo    # mount a small in-memory tree
```

Then, from another shell:

```sh
ls -lR /tmp/anymount-demo
cat /tmp/anymount-demo/hello.txt
sha256sum /tmp/anymount-demo/numbers.txt   # matches `seq 1 100 | sha256sum`
```

## Development

```sh
cargo test
cargo clippy --all-targets -- -Dwarnings
cargo deny check licenses bans sources

# Type check the other platforms' backends without their toolchains
cargo check --target x86_64-pc-windows-msvc
cargo check --target aarch64-apple-darwin
```

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)),
at your option.
