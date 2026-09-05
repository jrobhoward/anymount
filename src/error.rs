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
    #[error("no such file or directory")]
    NotFound,

    #[error("permission denied")]
    PermissionDenied,

    #[error("not a directory")]
    NotADirectory,

    #[error("is a directory")]
    IsADirectory,

    #[error("invalid argument")]
    InvalidArgument,

    #[error("no such extended attribute")]
    NoXattr,

    #[error("read-only filesystem")]
    ReadOnly,

    #[error("operation not supported: {0}")]
    Unsupported(&'static str),

    #[error("i/o error: {0}")]
    Io(#[from] io::Error),

    #[error("{0}")]
    Other(String),

    /// An error carrying extra explanation, mapping to the inner errno.
    #[error("{msg}")]
    Context { errno_as: Box<FsError>, msg: String },
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
    #[cfg(all(target_os = "macos", feature = "nfs"))]
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
            Self::NoXattr => 93, // ENOATTR
            Self::ReadOnly => libc::EROFS,
            Self::Unsupported(_) => libc::ENOSYS,
            Self::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
            Self::Other(_) => libc::EIO,
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

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, FsError>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
