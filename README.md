# anymount

[![crates.io](https://img.shields.io/crates/v/anymount.svg)](https://crates.io/crates/anymount)
[![docs.rs](https://docs.rs/anymount/badge.svg)](https://docs.rs/anymount)
[![CI](https://github.com/jrobhoward/anymount/actions/workflows/ci.yml/badge.svg)](https://github.com/jrobhoward/anymount/actions/workflows/ci.yml)

Mount a read-only filesystem from user space on Linux, macOS and Windows by
implementing one trait.

```sh
cargo add anymount
```

| OS | Mechanism | Install burden |
|----|-----------|----------------|
| Linux | FUSE via `fusermount3` | `apt install fuse3`; mounts unprivileged |
| macOS | NFSv3 via the built-in `mount_nfs` | none; no macFUSE, no kernel extension, no root |
| Windows | Cloud Files (cfapi) | none |

Requires Rust 1.88 or newer.

## Implementing the trait

`ReadOnlyFs` has six required methods. Inodes are `u64` and stable for the
life of the mount; the root is always `ROOT_INO`.

```rust
use std::ffi::{OsStr, OsString};
use anymount::{
    DirEntry, FileAttr, FileHandle, FileKind, FsError, Ino, MountBuilder,
    ReadOnlyFs, Result, ROOT_INO,
};

/// A filesystem with one file in it: `/greeting`.
struct Greeting;

const TEXT: &[u8] = b"hello from anymount\n";
const FILE: Ino = Ino(2);

impl ReadOnlyFs for Greeting {
    fn lookup(&self, parent: Ino, name: &OsStr) -> Result<FileAttr> {
        // NFS clients ask for `.` and `..` by name; FUSE resolves them itself.
        match name.to_str() {
            Some(".") | Some("..") => self.getattr(parent),
            Some("greeting") if parent == ROOT_INO => self.getattr(FILE),
            _ => Err(FsError::NotFound),
        }
    }

    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        match ino {
            ROOT_INO => Ok(FileAttr::dir(ROOT_INO)),
            FILE => Ok(FileAttr::file(FILE, TEXT.len() as u64)),
            _ => Err(FsError::NotFound),
        }
    }

    fn readdir(&self, ino: Ino, offset: u64) -> Result<Vec<DirEntry>> {
        if ino != ROOT_INO {
            return Err(FsError::NotADirectory);
        }
        // `.` and `..` are synthesised by the backend, never returned here.
        // An empty result is what ends the listing, so honour the offset.
        Ok(std::iter::once(DirEntry {
            ino: FILE,
            name: OsString::from("greeting"),
            kind: FileKind::File,
        })
        .skip(offset as usize)
        .collect())
    }

    fn open(&self, ino: Ino) -> Result<FileHandle> {
        if ino == FILE { Ok(FileHandle(1)) } else { Err(FsError::IsADirectory) }
    }

    fn read_at(&self, _fh: FileHandle, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let start = (offset as usize).min(TEXT.len());
        let n = buf.len().min(TEXT.len() - start);
        buf[..n].copy_from_slice(&TEXT[start..start + n]);
        Ok(n)
    }

    fn release(&self, _fh: FileHandle) -> Result<()> {
        Ok(())
    }
}

fn mount_it() -> Result<()> {
    let mount = MountBuilder::new("/tmp/greeting")
        .fs_name("greeting")
        .mount(Greeting)?;

    println!("mounted at {}", mount.mountpoint().display());
    mount.unmount()
}
```

That example is compiled by `cargo test`, so it cannot drift from the API.

`listxattr`, `getxattr`, `statfs` and `forget` have defaults that do the
harmless thing, so an implementation only overrides what it has answers for.

The mount is torn down when the `Mount` is dropped. Calling `unmount()`
explicitly does the same thing and returns the errors that dropping discards.
Either one succeeds on a mount the OS has already taken down, so ejecting the
volume first costs nothing.

## Caveats worth knowing before use

1.0 is feature-complete. None of the limitations below were required by the
crate's original use case, so none are planned for a 1.x release; a future
version that adds one is likely a 2.0, since the value types below are not
`#[non_exhaustive]`.

- Read-only. Write operations report `EROFS`, and that is the scope rather
  than a stage. Every limitation is catalogued in [`docs/GAPS.md`](docs/GAPS.md).
- On macOS, a mount that falls back to loopback TCP is reachable by any local
  account. The NFS backend prefers an `AF_UNIX` socket, where access is decided
  by file permissions and nothing is published to the system mount table, but
  that path needs an interface macOS does not document and falls back when it
  fails. `Mount::nfs_uses_local_socket` reports which path a mount took, and
  `MountBuilder::nfs_require_local_socket` refuses the fallback rather than
  mounting on the weaker terms.
  See [`docs/GAPS.md`](docs/GAPS.md).
- No symlinks or hardlinks: `FileKind` has only `File` and `Directory`.
- No extended attributes beyond `listxattr`/`getxattr`'s harmless defaults,
  and no Windows alternate data streams.
- The Windows mountpoint must be an empty directory. cfapi projects its
  entries into that directory rather than covering it, and clears them again
  on unmount, so mounting over existing files would destroy them; `mount()`
  refuses rather than risk it.
- Windows gets a directory, not a drive letter. cfapi projects into a
  virtualisation root and cannot assign `X:`.
- `read_at` takes an offset, but only FUSE issues random reads. cfapi fetches
  a whole file on first touch. An archive that can only decode from byte 0
  should materialise on open and serve reads from a cache.
- The trait is synchronous. Concurrency comes from serving requests on several
  threads, not from async.
- An unmount started by the OS is not reported. Ejecting the volume in Finder,
  or running `umount` or `fusermount3 -u`, takes the mount down at any time;
  the server behind it keeps running until the `Mount` is dropped, and there is
  no query or callback for the mount having gone. A process killed by a signal
  leaves the mount in place instead, since `Drop` does not run.
  See [`docs/GAPS.md`](docs/GAPS.md).

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `fuse` | yes | FUSE backend. Linux only; compiles to nothing elsewhere |
| `nfs` | yes | NFS backend. macOS only; needs no dependency of its own |
| `nfs-local-socket` | yes | NFS over an `AF_UNIX` socket. Implies `nfs` |
| `nfs-tcp` | yes | NFS over loopback TCP, run through `mount_nfs`. Implies `nfs` |
| `cfapi` | yes | Cloud Files backend. Windows only |
| `tracing` | no | Logs mounts, unmounts and the errors a backend has to discard |

All three backends default on because cargo cannot express a per-OS default;
the platform dependencies are `cfg`-scoped, so a Linux build never fetches the
`windows` crate.

### Choosing a macOS NFS transport

The two transport features are a trade between how private a mount is and how
much of the mechanism macOS documents. Both are on by default, which tries the
local socket first and falls back to loopback TCP.

| Features | Mount is private to the mounting user | Documented interfaces only | If the private encoding breaks |
|---|---|---|---|
| `nfs-local-socket`, `nfs-tcp` | when the local socket is used | no | falls back to TCP |
| `nfs-local-socket` alone | yes | no | mount fails |
| `nfs-tcp` alone | no | yes | not applicable |

`nfs-local-socket` passes the root file handle to `mount(2)` in an argument
buffer whose layout macOS does not document — `<nfs/nfs.h>` declares the
attribute numbers behind `__APPLE_API_PRIVATE` and the layout nowhere, and the
socket netid the buffer names is absent from `mount_nfs(8)`. Nothing is
compiled against that header: the constants are transcribed, and the only libc
calls are `mount(2)` and `unmount(2)`, both of which have man pages. The risk
is that a future macOS changes the encoding, not that the crate fails to build.

`nfs-tcp` runs `/sbin/mount_nfs` with options its man page documents, and uses
nothing undocumented. It costs reachability: the export path reaches the system
mount table and `nfsstat -m`, where any local account can read it.

Turning `nfs-tcp` off does not guarantee its absence, because cargo features
unify — another crate in the dependency graph enabling it brings the fallback
back for everyone. `MountBuilder::nfs_require_local_socket` is the guarantee,
and it cannot be overridden from outside. `Mount::nfs_uses_local_socket`
reports which transport a live mount got.

`nfs` with neither transport is a compile error rather than a backend that
cannot mount.

## Status

All three backends mount, read and unmount, and each is exercised against a
real mount in CI on its own platform — `ls`, `cat`, `find`, and a checksum
compared against one computed outside the mount.

## Why three backends, not one mechanism everywhere

Nothing in Rust spans all three platforms behind one API — the nearest
equivalent in any language is Go's `cgofuse`, which does not cover Windows.
So each platform gets the mechanism that fits it best rather than a lowest
common denominator: FUSE on Linux, a from-scratch NFSv3 server on macOS
(FUSE there needs a kernel extension; WebDAV made Finder download a whole
file on every folder view), and the Cloud Files API on Windows (ProjFS was
evaluated and set aside — see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)).
`winfsp`, `dokan` and `windows-projfs` are also Windows-only and copyleft;
see Licensing below.

## Licensing

MIT OR Apache-2.0, with no copyleft anywhere in the dependency graph. That is a
design constraint, enforced in CI by `cargo deny check licenses bans`, and it
rules out the obvious bindings for these platform APIs — so the crate routes
around them. Windows goes through Microsoft's own `windows` crate rather than
GPL `windows-projfs` or `winfsp`; Linux builds `fuser` without default
features, mounting through the `fusermount3` binary instead of linking LGPL
libfuse, which also enables unprivileged mounts; macOS needs nothing beyond the
standard library to reach `mount_nfs`. `deny.toml` bans the copyleft crates by
name, so an accidental `cargo add` fails loudly rather than quietly relicensing
the crate.

## Opening the mount in a file manager

`anymount` never relocates a mount: `Mount::mountpoint()` is always exactly the
path given to `MountBuilder::new`, on all three backends. There is no
`/Volumes`-style OS-injected location to look up, so opening a native window at
that path is a one-line job left to the caller:

```rust,ignore
let mount = MountBuilder::new("/mnt/restore").mount(my_fs)?;
opener::open(mount.mountpoint())?;
```

`examples/memfs.rs` demonstrates this behind an `--open` flag; `opener` is a
dev-dependency of the example, not of the library.

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
cargo fmt --all -- --check
cargo deny check licenses bans sources advisories

# Type check the other platforms' backends without their toolchains
cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -Dwarnings
cargo check --target aarch64-apple-darwin --all-targets

# The declared MSRV floor, which a drifting stable toolchain will not catch
cargo +1.88.0 check --all-targets
```

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) covers the module layout and
design rationale; [`CLAUDE.md`](CLAUDE.md) records the conventions and
platform constraints a contributor needs before editing a backend.

## License

MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)),
at your option.
