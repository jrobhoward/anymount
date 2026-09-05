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
| macOS | NFS via the built-in `mount_nfs` | none to install; chosen over both FUSE (needs Apple Silicon's Reduced Security boot policy) and WebDAV (Finder downloads every file in a viewed folder over it); see `docs/PLAN.md` |
| Windows | Cloud Files (cfapi) | none |

The Windows backend projects into a directory, not a drive letter. ProjFS was
evaluated and cut entirely, not kept as a fallback — see
[`docs/GAPS.md`](docs/GAPS.md).

## Status

**Phase 0 — spikes**, with the FUSE and NFS backends now built and verified
end to end. The Windows spike confirmed cfapi meets v1's needs unpackaged, but
`mount()` there is still a stub. macOS went through three mechanisms before
landing on one: FUSE hit real Apple Silicon boot-security friction (see
`docs/GAPS.md`); WebDAV avoided that but was found to make Finder download
every file in a viewed folder; a from-scratch, unprivileged NFS server avoided
both problems and is what ships. See [`docs/PLAN.md`](docs/PLAN.md) for the
full history and why each earlier choice was set aside rather than just which
one won.

| Backend | State |
|---------|-------|
| FUSE (Linux) | working — mounts, reads, random access, clean unmount |
| NFS (macOS) | working — a from-scratch NFSv3 server (`backend/nfs/`), mounted with the built-in `mount_nfs` client; verified unprivileged mounting, `ls`/`cat`/`stat`/`find`, `..` navigation, checksum-verified reads, paginated `READDIRPLUS3` listings, clean unmount, and crashed-server recovery (`soft,timeo=20,retrans=2`) |
| cfapi | `probe()` implemented and confirmed working unpackaged; `mount()` is a stub |

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
- **macOS** mounts through the built-in `mount_nfs` client, so nothing beyond
  Rust's standard library is needed to reach it — no macFUSE, no runtime
  dylib resolution, no cargo dependency of any kind. (An earlier plan used
  FUSE via macFUSE here, the same as Linux; see `docs/PLAN.md` for why that
  changed.)

## Scope

Read-only; write operations report `EROFS`. Known limitations are
catalogued in [`docs/GAPS.md`](docs/GAPS.md).

## Try it

```sh
mkdir -p /tmp/anymount-demo
cargo run --example probe                          # what can this machine mount?
cargo run --example memfs -- /tmp/anymount-demo    # mount a small in-memory tree
                                                    # (FUSE on Linux, NFS on macOS)
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
