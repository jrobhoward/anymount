//! Feature-gated logging shim, shared by every backend.
//!
//! Backends discard some errors on purpose: a failed `release` cannot be
//! reported to the application that triggered it, and a failed unmount during
//! `drop` has nowhere to go. [`ReadOnlyFs::release`](crate::ReadOnlyFs::release)
//! documents those as logged rather than propagated, so this is where the
//! logging happens.
//!
//! Without the `tracing` feature the macros expand to a `let _ = ...` over
//! their arguments: no dependency, no code, and no unused-variable warnings at
//! the call sites.

/// Log a discarded error or an unexpected-but-survivable condition.
macro_rules! backend_warn {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        ::tracing::warn!($($arg)*);
        #[cfg(not(feature = "tracing"))]
        let _ = format_args!($($arg)*);
    }};
}

/// Log a lifecycle transition: a mount established, a mount torn down.
macro_rules! backend_info {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        ::tracing::info!($($arg)*);
        #[cfg(not(feature = "tracing"))]
        let _ = format_args!($($arg)*);
    }};
}

pub(crate) use {backend_info, backend_warn};
