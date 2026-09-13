//! Mount a read-only filesystem from user space, on Linux, macOS and Windows.
//!
//! One trait, [`ReadOnlyFs`], is implemented; `anymount` mounts it with
//! whatever mechanism the host OS provides:
//!
//! | OS | Mechanism | Notes |
//! |----|-----------|-------|
//! | Linux | FUSE via `fusermount3` | unprivileged; libfuse never linked |
//! | macOS | NFSv3, mounted by the OS's own NFS client | unprivileged; no macFUSE, no kernel extension |
//! | Windows | Cloud Files (cfapi) | projects into a directory, not a drive letter |
//!
//! # Example
//!
//! ```no_run
//! use anymount::MountBuilder;
//! # fn example<F: anymount::ReadOnlyFs>(my_fs: F) -> anymount::Result<()> {
//! let mount = MountBuilder::new("/mnt/myfs")
//!     .fs_name("myfs")
//!     .mount(my_fs)?;
//!
//! println!("mounted at {}", mount.mountpoint().display());
//! mount.unmount()?;
//! # Ok(())
//! # }
//! ```
//!
//! [`Mount`] unmounts when dropped, so the explicit
//! [`unmount`](Mount::unmount) above is needed only to see the errors that
//! dropping discards.
//!
//! # Scope
//!
//! Read-only, by design. Write operations report `EROFS`. Known limitations
//! are catalogued in [`docs/GAPS.md`][gaps].
//!
//! The Windows mountpoint must be an empty directory: cfapi projects its
//! entries into that directory rather than covering it, and clears them again
//! on unmount.
//!
//! # Choosing a macOS NFS transport
//!
//! The NFS backend has two transports, and the `nfs-local-socket` and
//! `nfs-tcp` features decide which are compiled in. Both are on by default,
//! which tries the local socket first and falls back to loopback TCP. The
//! choice is a trade between how private a mount is and how much of the
//! mechanism macOS documents.
//!
//! | Features | Mount is private to the mounting user | Documented interfaces only |
//! |---|---|---|
//! | `nfs-local-socket`, `nfs-tcp` | when the local socket is used | no |
//! | `nfs-local-socket` alone | yes | no |
//! | `nfs-tcp` alone | no | yes |
//!
//! `nfs-local-socket` serves an `AF_UNIX` socket and passes the root file
//! handle to `mount(2)` directly. Nothing listens on the network, the MOUNT
//! protocol never runs, and no credential reaches the system mount table;
//! access is decided by the socket's file permissions. The argument buffer it
//! builds has a layout macOS does not document, so a future release can break
//! it. Nothing is compiled against a private header — the constants are
//! transcribed, and `mount(2)` and `unmount(2)` both have man pages — so the
//! risk is a failed mount, not a failed build.
//!
//! `nfs-tcp` runs `mount_nfs` with documented options and relies on nothing
//! undocumented. A mount that uses it is reachable by any local account: the
//! export path reaches the system mount table and `nfsstat -m`. Two
//! independent values limit what that grants, and
//! [`docs/GAPS.md`][gaps] describes what remains.
//!
//! Cargo features unify, so building without `nfs-tcp` does not guarantee its
//! absence — another crate in the graph can enable it.
//! [`MountBuilder::nfs_require_local_socket`] refuses the fallback at mount
//! time instead, and [`Mount::nfs_uses_local_socket`] reports which transport
//! a live mount got.
//!
//! # Licensing
//!
//! `anymount` is MIT OR Apache-2.0 with no copyleft anywhere in its dependency
//! graph. That is a design constraint rather than an accident: WinFsp
//! (GPL-3.0), Dokany (LGPL) and `windows-projfs` (GPL-2.0) are all excluded —
//! ProjFS itself was evaluated and dropped for having no capability advantage
//! over cfapi, see [`docs/GAPS.md`][gaps] — and Linux's FUSE backend never
//! links libfuse, mounting through the `fusermount3` binary instead.
//! `cargo deny check licenses` enforces this in CI.
//!
//! [gaps]: https://github.com/jrobhoward/anymount/blob/main/docs/GAPS.md

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]
// `doc_cfg` renders the platform and feature gates on docs.rs, which builds
// for several targets. Nightly-only, so it is kept behind the `docsrs` cfg
// that docs.rs sets and nothing else does.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod backend;
mod error;
mod fs;
mod mount;
mod types;

pub use error::{FsError, Result};
pub use fs::ReadOnlyFs;
pub use mount::{Backend, Mount, MountBuilder};
pub use types::{DirEntry, FileAttr, FileHandle, FileKind, Ino, ROOT_INO, StatFs};

/// Compiles `README.md`'s example as a doctest, so the front page cannot
/// drift from the API it demonstrates. `cfg(doctest)` means the module exists
/// only while doctests are being collected: it is absent from a normal build
/// and from the rendered documentation.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme_example {}

/// Backend availability probes, for diagnostics.
pub mod probe {
    /// Is a usable backend compiled in for this platform?
    ///
    /// False means [`mount`](crate::MountBuilder::mount) will report
    /// [`FsError::Unsupported`](crate::FsError::Unsupported) whatever it is
    /// asked for — either the platform has no backend, or the feature
    /// supplying it was turned off. It says nothing about whether a mount
    /// would succeed: that also needs `fusermount3` on Linux and a new enough
    /// Windows for cfapi.
    pub fn any_backend_available() -> bool {
        cfg!(all(target_os = "linux", feature = "fuse"))
            || cfg!(all(target_os = "macos", feature = "nfs"))
            || cfg!(all(windows, feature = "cfapi"))
    }

    #[cfg(all(windows, feature = "cfapi"))]
    #[cfg_attr(docsrs, doc(cfg(all(windows, feature = "cfapi"))))]
    pub use crate::backend::cfapi::{PlatformInfo, probe as cfapi};
}
