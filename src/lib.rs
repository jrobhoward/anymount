//! Mount a read-only filesystem from user space, on Linux, macOS and Windows.
//!
//! You implement one trait, [`ReadOnlyFs`], and `anymount` mounts it with
//! whatever mechanism the host OS provides:
//!
//! | OS | Mechanism | Notes |
//! |----|-----------|-------|
//! | Linux | FUSE via `fusermount3` | unprivileged; libfuse never linked |
//! | macOS | none yet | decided: NFS, not FUSE — not built as `backend/nfs.rs` yet; see `docs/PLAN.md` |
//! | Windows | Cloud Files (cfapi) | projects into a directory, not a drive letter |
//!
//! # Licensing
//!
//! `anymount` is MIT OR Apache-2.0 and has **no copyleft anywhere in its
//! dependency graph**. That is a deliberate design constraint, not an accident:
//! WinFsp (GPLv3), Dokany (LGPL) and `windows-projfs` (GPL-2.0) are all
//! excluded — ProjFS itself was evaluated in Phase 0 and dropped for having no
//! capability advantage over cfapi, see `docs/GAPS.md` — and Linux's FUSE
//! backend never links libfuse, mounting through the `fusermount3` binary
//! instead.
//! `cargo deny check licenses` enforces this in CI.
//!
//! # Example
//!
//! ```no_run
//! use anymount::MountBuilder;
//! # fn example<F: anymount::ReadOnlyFs>(my_fs: F) -> anymount::Result<()> {
//! let mount = MountBuilder::new("/mnt/restore")
//!     .fs_name("mybackup")
//!     .mount(my_fs)?;
//!
//! println!("mounted at {}", mount.mountpoint().display());
//! mount.unmount()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Scope
//!
//! Read-only, by design. Write operations report `EROFS`. Known limitations are
//! catalogued in `docs/GAPS.md`.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

mod backend;
mod error;
mod fs;
mod mount;
mod types;

pub use error::{FsError, Result};
pub use fs::ReadOnlyFs;
pub use mount::{Backend, Mount, MountBuilder};
pub use types::{DirEntry, FileAttr, FileHandle, FileKind, Ino, ROOT_INO, StatFs};

/// Backend availability probes, for diagnostics and the Phase 0 spikes.
pub mod probe {
    /// Is a usable backend compiled in for this platform?
    pub fn any_backend_available() -> bool {
        cfg!(all(target_os = "linux", feature = "fuse")) || cfg!(all(windows, feature = "cfapi"))
    }

    #[cfg(all(windows, feature = "cfapi"))]
    pub use crate::backend::cfapi::{PlatformInfo, probe as cfapi};
}
