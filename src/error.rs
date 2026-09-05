//! Error type shared by every backend.

use std::io;

/// Failure of a filesystem operation.
///
/// Variants are chosen to map cleanly onto both `errno` (FUSE) and
/// `HRESULT`/`NTSTATUS` (cfapi), so an implementor never has to know which
/// backend is mounted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FsError {
    /// The named inode or directory entry does not exist. `ENOENT`.
    #[error("no such file or directory")]
    NotFound,

    /// The caller may not perform this operation. `EACCES`.
    #[error("permission denied")]
    PermissionDenied,

    /// A directory operation was asked of something that is not one.
    /// `ENOTDIR`.
    #[error("not a directory")]
    NotADirectory,

    /// A file operation was asked of a directory — reading one, for instance.
    /// `EISDIR`.
    #[error("is a directory")]
    IsADirectory,

    /// The request itself is malformed: an unusable handle, a nonsensical
    /// offset. `EINVAL`.
    #[error("invalid argument")]
    InvalidArgument,

    /// The named extended attribute is not set on this inode. `ENODATA` on
    /// Linux, `ENOATTR` on macOS. The default
    /// [`getxattr`](crate::ReadOnlyFs::getxattr) returns this.
    #[error("no such extended attribute")]
    NoXattr,

    /// A write was attempted. `EROFS`. Backends answer mutating operations
    /// with this on the crate's behalf; an implementation should not need to
    /// return it.
    #[error("read-only filesystem")]
    ReadOnly,

    /// The operation is not implemented, with a static explanation of what
    /// was asked for. `ENOSYS`. Also how a request for a backend that is not
    /// compiled in, or not available on this platform, is reported.
    #[error("operation not supported: {0}")]
    Unsupported(&'static str),

    /// An underlying I/O failure, kept whole so its `errno` survives.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),

    /// A failure with no better variant, carrying its own message. `EIO`.
    #[error("{0}")]
    Other(String),

    /// An error carrying extra explanation, mapping to the inner errno.
    ///
    /// Built by [`context`](FsError::context) rather than directly.
    #[error("{msg}")]
    Context {
        /// The error whose `errno` this reports, so the kernel still sees the
        /// right code.
        errno_as: Box<FsError>,
        /// The explanation shown to a human.
        msg: String,
    },
}

impl FsError {
    /// Wrap this error with human-readable context.
    ///
    /// The errno mapping is preserved, so the kernel still sees the right code
    /// while the caller gets an explanation.
    pub fn context(self, msg: impl Into<String>) -> Self {
        Self::Context {
            errno_as: Box::new(self),
            msg: msg.into(),
        }
    }

    /// `nfsstat3` for this error, used by the NFS backend.
    ///
    /// Named constants for the status codes live in
    /// `backend::nfs::nfs_proto`, not repeated here as magic numbers.
    #[cfg(all(unix, feature = "nfs"))]
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn to_nfsstat3(&self) -> u32 {
        match self {
            Self::NotFound => 2,            // NFS3ERR_NOENT
            Self::PermissionDenied => 13,   // NFS3ERR_ACCES
            Self::NotADirectory => 20,      // NFS3ERR_NOTDIR
            Self::IsADirectory => 21,       // NFS3ERR_ISDIR
            Self::InvalidArgument => 22,    // NFS3ERR_INVAL
            Self::NoXattr => 10_004,        // NFS3ERR_NOTSUPP
            Self::ReadOnly => 30,           // NFS3ERR_ROFS
            Self::Unsupported(_) => 10_004, // NFS3ERR_NOTSUPP
            Self::Io(_) => 5,               // NFS3ERR_IO
            Self::Other(_) => 10_006,       // NFS3ERR_SERVERFAULT
            Self::Context { errno_as, .. } => errno_as.to_nfsstat3(),
        }
    }

    /// Raw `NTSTATUS` value for this error, used by the cfapi backend to fail a
    /// `TRANSFER_DATA`/`TRANSFER_PLACEHOLDERS` operation. Returned as a plain
    /// `i32` rather than `windows::Win32::Foundation::NTSTATUS` so this module
    /// does not need the `windows` crate in scope; the cfapi backend wraps it
    /// at the call site.
    #[cfg(all(windows, feature = "cfapi"))]
    pub(crate) fn to_ntstatus(&self) -> i32 {
        use windows::Win32::Foundation::{
            STATUS_ACCESS_DENIED, STATUS_CLOUD_FILE_UNSUCCESSFUL, STATUS_INVALID_PARAMETER,
            STATUS_NOT_SUPPORTED, STATUS_OBJECT_NAME_NOT_FOUND,
        };

        match self {
            Self::NotFound => STATUS_OBJECT_NAME_NOT_FOUND.0,
            Self::PermissionDenied => STATUS_ACCESS_DENIED.0,
            Self::InvalidArgument => STATUS_INVALID_PARAMETER.0,
            Self::Unsupported(_) | Self::NoXattr => STATUS_NOT_SUPPORTED.0,
            Self::NotADirectory
            | Self::IsADirectory
            | Self::ReadOnly
            | Self::Io(_)
            | Self::Other(_) => STATUS_CLOUD_FILE_UNSUCCESSFUL.0,
            Self::Context { errno_as, .. } => errno_as.to_ntstatus(),
        }
    }

    /// POSIX `errno` for this error, used by the FUSE backend.
    ///
    /// Available on every platform, not only Unix, so the public surface does
    /// not change shape between targets: `errno` is a useful portable error
    /// taxonomy even where nothing consumes it as one. Off Unix the numbers
    /// are the Linux ones, written out as literals since there is no `libc` to
    /// take them from — see the non-Unix implementation for the caveat that
    /// carries.
    #[cfg(unix)]
    pub fn to_errno(&self) -> i32 {
        match self {
            Self::NotFound => libc::ENOENT,
            Self::PermissionDenied => libc::EACCES,
            Self::NotADirectory => libc::ENOTDIR,
            Self::IsADirectory => libc::EISDIR,
            Self::InvalidArgument => libc::EINVAL,
            // ENODATA is the Linux spelling; macOS calls it ENOATTR.
            #[cfg(target_os = "linux")]
            Self::NoXattr => libc::ENODATA,
            #[cfg(target_os = "macos")]
            Self::NoXattr => libc::ENOATTR,
            Self::ReadOnly => libc::EROFS,
            Self::Unsupported(_) => libc::ENOSYS,
            Self::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
            Self::Other(_) => libc::EIO,
            Self::Context { errno_as, .. } => errno_as.to_errno(),
        }
    }

    /// POSIX `errno` for this error. See the Unix implementation above.
    ///
    /// The values are Linux's, spelled out because no `libc` is linked here.
    /// They are a stable taxonomy for a caller that wants to classify an error
    /// portably, not something to hand to a Windows API — the cfapi backend
    /// maps to `NTSTATUS` internally instead. `Io` reports its own raw OS
    /// error where it has one, which on Windows is a Win32 code rather than
    /// an `errno`, so a caller matching on specific numbers should treat that
    /// variant as the exception.
    #[cfg(not(unix))]
    pub fn to_errno(&self) -> i32 {
        // Linux's <asm-generic/errno.h> values.
        const EIO: i32 = 5;
        const ENOENT: i32 = 2;
        const EACCES: i32 = 13;
        const ENOTDIR: i32 = 20;
        const EISDIR: i32 = 21;
        const EINVAL: i32 = 22;
        const EROFS: i32 = 30;
        const ENOSYS: i32 = 38;
        const ENODATA: i32 = 61;

        match self {
            Self::NotFound => ENOENT,
            Self::PermissionDenied => EACCES,
            Self::NotADirectory => ENOTDIR,
            Self::IsADirectory => EISDIR,
            Self::InvalidArgument => EINVAL,
            Self::NoXattr => ENODATA,
            Self::ReadOnly => EROFS,
            Self::Unsupported(_) => ENOSYS,
            Self::Io(e) => e.raw_os_error().unwrap_or(EIO),
            Self::Other(_) => EIO,
            Self::Context { errno_as, .. } => errno_as.to_errno(),
        }
    }
}

/// Wrap a Win32/COM failure from the cfapi backend, preserving its message.
///
/// `windows::core::Error` carries an `HRESULT` and a human-readable message;
/// only `E_ACCESSDENIED` is common enough during mount setup (registering a
/// sync root without sufficient rights on the directory) to warrant its own
/// variant. Everything else becomes [`FsError::Other`] with the message
/// intact rather than losing it behind a generic errno.
#[cfg(all(windows, feature = "cfapi"))]
impl From<windows::core::Error> for FsError {
    fn from(e: windows::core::Error) -> Self {
        if e.code() == windows::Win32::Foundation::E_ACCESSDENIED {
            Self::PermissionDenied
        } else {
            Self::Other(e.message())
        }
    }
}

/// Bridge into [`std::io`], for callers whose own APIs return
/// [`io::Result`].
///
/// An [`FsError::Io`] is returned unchanged, keeping its original kind and
/// raw OS error. Everything else is mapped to the closest
/// [`io::ErrorKind`] and keeps its message, so nothing is
/// lost by converting.
impl From<FsError> for io::Error {
    fn from(e: FsError) -> Self {
        use io::ErrorKind;

        // Unwrap a `Context` down to the error it reports as, keeping the
        // outer message: that message is the useful half.
        let msg = e.to_string();
        let kind = match innermost(&e) {
            FsError::NotFound => ErrorKind::NotFound,
            FsError::PermissionDenied => ErrorKind::PermissionDenied,
            FsError::NotADirectory | FsError::IsADirectory | FsError::InvalidArgument => {
                ErrorKind::InvalidInput
            }
            FsError::ReadOnly => ErrorKind::PermissionDenied,
            FsError::Unsupported(_) | FsError::NoXattr => ErrorKind::Unsupported,
            // Returned whole rather than rebuilt, so the original kind and
            // `raw_os_error` survive the round trip.
            FsError::Io(_) => return unwrap_io(e),
            _ => ErrorKind::Other,
        };
        io::Error::new(kind, msg)
    }
}

/// The error a [`FsError::Context`] chain ultimately reports as.
fn innermost(e: &FsError) -> &FsError {
    match e {
        FsError::Context { errno_as, .. } => innermost(errno_as),
        other => other,
    }
}

/// Take the [`io::Error`] out of a chain known to end in [`FsError::Io`].
fn unwrap_io(e: FsError) -> io::Error {
    match e {
        FsError::Io(inner) => inner,
        FsError::Context { errno_as, .. } => unwrap_io(*errno_as),
        // Unreachable: only called once `innermost` has identified an `Io`.
        other => io::Error::other(other.to_string()),
    }
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, FsError>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
