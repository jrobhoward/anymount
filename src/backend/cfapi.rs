//! Windows Cloud Files (cfapi) backend — the only Windows backend `anymount`
//! ships. `docs/PLAN.md` (Phase 0) covers why ProjFS was evaluated and cut.
//!
//! Registers the mountpoint as a Cloud Files sync root (`CfRegisterSyncRoot`)
//! and connects a callback table (`CfConnectSyncRoot`) that answers
//! `CF_CALLBACK_TYPE_FETCH_DATA` and `CF_CALLBACK_TYPE_FETCH_PLACEHOLDERS` from
//! [`ReadOnlyFs`]. The mountpoint needs no explicit conversion to a
//! placeholder first — confirmed empirically, see [`connect_sync_root`]'s
//! docs — `CF_POPULATION_POLICY_PARTIAL` at registration is enough on its own
//! for the platform to send `FETCH_PLACEHOLDERS` the first time anything
//! enumerates it, the same as any subdirectory this backend populates later.
//!
//! # Identity
//!
//! Every placeholder this backend creates carries its [`Ino`] as an 8-byte
//! little-endian `FileIdentity` (`to_create_info`). Every callback gets that
//! same identity back in `CF_CALLBACK_INFO` (`decode_ino`), so there is no
//! separate table mapping cfapi's file handles back to inodes — the platform
//! carries the mapping for this backend, the same way NFS embeds a capability
//! secret directly in its file handles (`backend/nfs/handle.rs`) instead of
//! keeping a lookup table. The one exception is the mountpoint itself, which
//! never goes through [`to_create_info`] and so carries no identity of ours;
//! [`connect_sync_root`]'s docs cover how that's handled.
//!
//! # Read pattern
//!
//! Phase 2's spike found cfapi has no ranged-read path an application can
//! reach: `FETCH_DATA` always requests the whole file (`RequiredFileOffset ==
//! 0`, `RequiredLength ==` the file size) regardless of hydration policy or
//! buffering — see [`ReadOnlyFs`]'s "Read patterns differ by backend" docs.
//! [`ReadOnlyFs::read_at`] is still called here at increasing offsets in
//! [`TRANSFER_CHUNK`]-sized pieces rather than once for the whole range, so a
//! large backup file streams through a bounded buffer instead of being
//! materialised in memory by this backend.
//!
//! # Directory population
//!
//! `CF_CALLBACK_TYPE_FETCH_PLACEHOLDERS` carries no size budget (confirmed in
//! the Phase 2 spike, unlike NFS's `READDIR`), so [`readdir::emit`] is used
//! with [`Dots::Omit`] and a sink that always accepts. [`ReadOnlyFs::readdir`]
//! may still page, and `emit` walks those pages, so the sink sees the whole
//! directory however the implementation chose to hand it over; it is then sent
//! back in one `CF_OPERATION_TYPE_TRANSFER_PLACEHOLDERS` call.

use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::SystemTime;

use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::Storage::CloudFilters::{
    CF_CALLBACK_INFO, CF_CALLBACK_PARAMETERS, CF_CALLBACK_REGISTRATION,
    CF_CALLBACK_TYPE_FETCH_DATA, CF_CALLBACK_TYPE_FETCH_PLACEHOLDERS, CF_CALLBACK_TYPE_NONE,
    CF_CONNECT_FLAG_NONE, CF_CONNECTION_KEY, CF_FS_METADATA, CF_HARDLINK_POLICY_NONE,
    CF_HYDRATION_POLICY, CF_HYDRATION_POLICY_FULL,
    CF_HYDRATION_POLICY_MODIFIER_AUTO_DEHYDRATION_ALLOWED,
    CF_HYDRATION_POLICY_MODIFIER_STREAMING_ALLOWED, CF_INSYNC_POLICY_NONE, CF_OPERATION_INFO,
    CF_OPERATION_PARAMETERS, CF_OPERATION_PARAMETERS_0, CF_OPERATION_PARAMETERS_0_0,
    CF_OPERATION_PARAMETERS_0_4, CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
    CF_OPERATION_TRANSFER_PLACEHOLDERS_FLAG_NONE, CF_OPERATION_TYPE_TRANSFER_DATA,
    CF_OPERATION_TYPE_TRANSFER_PLACEHOLDERS, CF_PLACEHOLDER_CREATE_FLAG_NONE,
    CF_PLACEHOLDER_CREATE_INFO, CF_PLACEHOLDER_MANAGEMENT_POLICY_DEFAULT, CF_POPULATION_POLICY,
    CF_POPULATION_POLICY_MODIFIER_NONE, CF_POPULATION_POLICY_PARTIAL, CF_REGISTER_FLAG_NONE,
    CF_SYNC_POLICIES, CF_SYNC_REGISTRATION, CfConnectSyncRoot, CfDisconnectSyncRoot, CfExecute,
    CfRegisterSyncRoot, CfUnregisterSyncRoot,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_BASIC_INFO,
};
use windows::core::{GUID, HRESULT, PCWSTR};

use crate::backend::Mounted;
use crate::backend::preflight::{self, Caps};
use crate::backend::readdir::{self, Dots, Sink};
use crate::error::{FsError, Result};
use crate::fs::ReadOnlyFs;
use crate::mount::{Backend, MountBuilder};
use crate::types::{FileAttr, FileHandle, FileKind, Ino, ROOT_INO};

/// `allow_other` and `auto_unmount` are FUSE mount options with no cfapi
/// counterpart: a sync root is registered by, and visible to, the user running
/// the process, and teardown is owned by [`Mounted`].
///
/// `empty_mountpoint` is the one cap this backend does claim. Unlike a Unix
/// mount, a sync root does not cover the directory — placeholders are created
/// inside it, and [`remove_leftover_placeholders`] clears them on unmount —
/// so anything already there would be destroyed.
const CAPS: Caps = Caps {
    name: "cfapi",
    allow_other: false,
    auto_unmount: false,
    empty_mountpoint: true,
    threads: false,
};

/// Identifies `anymount` as a Cloud Files provider. Only has to be stable and
/// not collide with another provider registered on the same machine — it need
/// not be registered anywhere outside this crate.
const PROVIDER_ID: GUID = GUID::from_u128(0x38f69b72_5e8a_4f53_88e9_6b09e09b88c6);

/// Bytes transferred per `CF_OPERATION_TYPE_TRANSFER_DATA` call. Must be a
/// multiple of 4096: cfapi requires a transferred range's offset and length to
/// be 4KB-aligned unless the range ends at or beyond the file's logical size —
/// true only of the final chunk of a fetch, which this always sends last.
const TRANSFER_CHUNK: usize = 1024 * 1024;

/// Encode `s` as a null-terminated UTF-16 string, the form every wide-string
/// cfapi/Win32 parameter here takes.
fn to_wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Owns the filesystem a mount serves. Its address, taken before it moves into
/// [`CfApiHandle`], is what `CfConnectSyncRoot`'s `CallbackContext` hands back
/// to every callback.
///
/// A plain `F`, not `Arc<F>`: nothing shares ownership of it. [`CfApiHandle`]
/// holds the only [`Box`]; callbacks only ever borrow it through the raw
/// pointer cfapi returns, and only while the mount is connected — `unmount`
/// disconnects before dropping it.
struct Context<F: ReadOnlyFs> {
    fs: F,
}

/// Live cfapi mount: a registered, connected sync root plus the filesystem
/// backing its callbacks.
pub(crate) struct CfApiHandle<F: ReadOnlyFs> {
    mountpoint: PathBuf,
    connection: CF_CONNECTION_KEY,
    /// Kept alive only so its heap allocation — and thus every callback's
    /// `CallbackContext` pointer into it — outlives `CfDisconnectSyncRoot` in
    /// [`unmount`](Mounted::unmount). Never read here directly.
    _context: Box<Context<F>>,
}

impl<F: ReadOnlyFs> std::fmt::Debug for CfApiHandle<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CfApiHandle")
            .field("mountpoint", &self.mountpoint)
            .field("connection", &self.connection.0)
            .finish_non_exhaustive()
    }
}

impl<F: ReadOnlyFs> Mounted for CfApiHandle<F> {
    fn unmount(self: Box<Self>) -> Result<()> {
        // SAFETY: `self.connection` came from a `CfConnectSyncRoot` call that
        // succeeded in `mount`, and this is the only place it is disconnected.
        unsafe { CfDisconnectSyncRoot(self.connection) }?;

        let wide = to_wide(&self.mountpoint);
        // SAFETY: `self.mountpoint` was successfully registered in `mount`;
        // `wide` is a valid null-terminated wide string live for the call.
        unsafe { CfUnregisterSyncRoot(PCWSTR::from_raw(wide.as_ptr())) }?;

        remove_leftover_placeholders(&self.mountpoint);
        Ok(())
    }

    fn backend(&self) -> Backend {
        Backend::CfApi
    }
}

/// Best-effort cleanup after `CfUnregisterSyncRoot`: placeholder entries this
/// backend created remain on disk as reparse-point stubs once the provider
/// disconnects, and nothing else will ever reclaim them. Removing them here is
/// what lets [`unmount`](Mounted::unmount) honor [`Mounted`]'s "leaves nothing
/// behind" contract. Failures are logged, not propagated, matching every other
/// best-effort cleanup in this crate (a failed `release`, a failed unmount
/// during `drop`).
///
/// # Why this is narrow
///
/// This function deletes files, so it deletes as little as it can. Two things
/// bound it. The mountpoint was required to be empty at mount time
/// ([`CAPS`]), so anything found here should be this backend's own work; and
/// each entry must still carry `FILE_ATTRIBUTE_REPARSE_POINT`, which a
/// placeholder does and an ordinary file does not. An entry failing that test
/// is left alone and logged rather than removed — leaving a stray file behind
/// costs a warning and a failed emptiness check at the next mount, while
/// deleting the wrong one costs a user their data. Given that trade, being
/// too cautious is the correct way to be wrong.
///
/// The precise cloud reparse tag is not checked. Reading it needs
/// `GetFileInformationByHandleEx(FileAttributeTagInfo)` and a handle opened
/// with `FILE_FLAG_OPEN_REPARSE_POINT` — more FFI and more unsafe than the
/// remaining risk justifies, given an empty mountpoint is already a
/// precondition.
fn remove_leftover_placeholders(mountpoint: &Path) {
    let entries = match std::fs::read_dir(mountpoint) {
        Ok(entries) => entries,
        Err(e) => {
            backend_warn!(
                "anymount/cfapi: reading {} to clean up after unmount: {e}",
                mountpoint.display()
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();

        match entry.metadata() {
            Ok(meta) if is_reparse_point(&meta) => {}
            Ok(_) => {
                backend_warn!(
                    "anymount/cfapi: leaving {} in place after unmount: it is not a \
                     placeholder, so this backend did not create it",
                    path.display()
                );
                continue;
            }
            Err(e) => {
                backend_warn!(
                    "anymount/cfapi: leaving {} in place after unmount: its attributes \
                     could not be read, so it cannot be confirmed as a placeholder: {e}",
                    path.display()
                );
                continue;
            }
        }

        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(e) = result {
            backend_warn!(
                "anymount/cfapi: removing leftover placeholder {}: {e}",
                path.display()
            );
        }
    }
}

/// Does this entry carry `FILE_ATTRIBUTE_REPARSE_POINT`?
///
/// Every placeholder cfapi creates does; an ordinary file does not. Split out
/// from [`remove_leftover_placeholders`] so the predicate is unit testable
/// against a `Metadata` obtained from a real file.
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

/// Platform version reported by `CfGetPlatformInfo`, proving `CldApi.dll` loads.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PlatformInfo {
    /// Windows build number.
    pub build: u32,
    /// Windows revision number within that build.
    pub revision: u32,
    /// Cloud Files integration number. The value that gates newer features:
    /// the unrestricted placeholder-management policies need `0x310` or
    /// higher.
    pub integration: u32,
}

/// Query the Cloud Files platform version.
///
/// `integration` is the value gating newer features: the unrestricted
/// placeholder-management policies need `0x310` or higher.
pub fn probe() -> Option<PlatformInfo> {
    use windows::Win32::Storage::CloudFilters::CfGetPlatformInfo;

    // SAFETY: the binding allocates and initialises the out-parameter itself
    // and returns it by value; there is nothing for the caller to keep alive.
    //
    // A load-time import of `CldApi.dll` is safe: it ships with every Windows
    // 10 1709+ install and is not an optional feature.
    let info = unsafe { CfGetPlatformInfo() }.ok()?;
    Some(PlatformInfo {
        build: info.BuildNumber,
        revision: info.RevisionNumber,
        integration: info.IntegrationNumber,
    })
}

pub(crate) fn mount<F: ReadOnlyFs>(builder: MountBuilder, fs: F) -> Result<CfApiHandle<F>> {
    preflight::check(&builder, &CAPS)?;

    if probe().is_none() {
        return Err(FsError::Unsupported(
            "Cloud Files API unavailable: requires Windows 10 1709 or later",
        ));
    }

    let path_wide = to_wide(&builder.mountpoint);
    register_sync_root(&path_wide, &builder.fs_name)
        .map_err(|e| e.context("registering the cfapi sync root"))?;

    let context = Box::new(Context { fs });
    // SAFETY: taken before `context` moves into `CfApiHandle` below. A
    // `Box`'s heap allocation does not move when the `Box` value itself does,
    // so this stays valid for as long as `context` lives — which outlives
    // every callback, since `unmount` disconnects before dropping it.
    let context_ptr: *const c_void = (&*context as *const Context<F>).cast();

    let connection = match connect_sync_root::<F>(&path_wide, context_ptr) {
        Ok(connection) => connection,
        Err(e) => {
            // SAFETY: registration above succeeded, so unregistering the same
            // path is valid.
            if let Err(unreg) =
                unsafe { CfUnregisterSyncRoot(PCWSTR::from_raw(path_wide.as_ptr())) }
            {
                backend_warn!("anymount/cfapi: cleaning up a failed mount attempt: {unreg}");
            }
            return Err(e);
        }
    };

    Ok(CfApiHandle {
        mountpoint: builder.mountpoint,
        connection,
        _context: context,
    })
}

fn register_sync_root(path_wide: &[u16], fs_name: &str) -> Result<()> {
    let provider_name = to_wide(fs_name);
    let provider_version = to_wide(env!("CARGO_PKG_VERSION"));

    let registration = CF_SYNC_REGISTRATION {
        StructSize: size_of::<CF_SYNC_REGISTRATION>() as u32,
        ProviderName: PCWSTR::from_raw(provider_name.as_ptr()),
        ProviderVersion: PCWSTR::from_raw(provider_version.as_ptr()),
        SyncRootIdentity: ptr::null(),
        SyncRootIdentityLength: 0,
        FileIdentity: ptr::null(),
        FileIdentityLength: 0,
        ProviderId: PROVIDER_ID,
    };
    // `Population::Primary::PARTIAL` is on-demand enumeration; `Hydration`
    // pairs `FULL` (matching the Phase 2 finding that cfapi never does
    // partial reads) with `STREAMING_ALLOWED` (fetched data is not persisted
    // beyond what NTFS needs to satisfy the read) and
    // `AUTO_DEHYDRATION_ALLOWED` (Windows can reclaim hydrated files under
    // disk pressure without this backend managing eviction itself).
    let policies = CF_SYNC_POLICIES {
        StructSize: size_of::<CF_SYNC_POLICIES>() as u32,
        Hydration: CF_HYDRATION_POLICY {
            Primary: CF_HYDRATION_POLICY_FULL,
            Modifier: CF_HYDRATION_POLICY_MODIFIER_STREAMING_ALLOWED
                | CF_HYDRATION_POLICY_MODIFIER_AUTO_DEHYDRATION_ALLOWED,
        },
        Population: CF_POPULATION_POLICY {
            Primary: CF_POPULATION_POLICY_PARTIAL,
            Modifier: CF_POPULATION_POLICY_MODIFIER_NONE,
        },
        InSync: CF_INSYNC_POLICY_NONE,
        HardLink: CF_HARDLINK_POLICY_NONE,
        PlaceholderManagement: CF_PLACEHOLDER_MANAGEMENT_POLICY_DEFAULT,
    };

    // SAFETY: `path_wide`, `provider_name` and `provider_version` are valid
    // null-terminated wide strings live for the duration of this call;
    // `registration` and `policies` are valid, correctly sized structures.
    unsafe {
        CfRegisterSyncRoot(
            PCWSTR::from_raw(path_wide.as_ptr()),
            &registration,
            &policies,
            CF_REGISTER_FLAG_NONE,
        )
    }
    .map_err(Into::into)
}

/// Connect the callback table for a sync root that was just registered.
///
/// The mountpoint needs no explicit conversion to a placeholder first —
/// confirmed empirically (a `CfConvertToPlaceholder` step was tried here and
/// found both unnecessary and rejected with `ERROR_CLOUD_FILE_INVALID_REQUEST`
/// for the sync root's own directory): `CF_POPULATION_POLICY_PARTIAL` at
/// registration is enough on its own for the platform to send
/// `FETCH_PLACEHOLDERS` the first time anything enumerates the mountpoint.
/// That callback's `FileIdentity` is then empty for the root specifically,
/// since nothing has ever set one on it — `handle_fetch_placeholders` treats
/// that as [`ROOT_INO`].
fn connect_sync_root<F: ReadOnlyFs>(
    path_wide: &[u16],
    context_ptr: *const c_void,
) -> Result<CF_CONNECTION_KEY> {
    let callback_table = [
        CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_FETCH_DATA,
            Callback: Some(fetch_data::<F>),
        },
        CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_FETCH_PLACEHOLDERS,
            Callback: Some(fetch_placeholders::<F>),
        },
        // Terminator: cfapi reads this table until `Type == CF_CALLBACK_TYPE_NONE`.
        CF_CALLBACK_REGISTRATION {
            Type: CF_CALLBACK_TYPE_NONE,
            Callback: None,
        },
    ];

    // SAFETY: `path_wide` is a valid null-terminated wide string;
    // `callback_table` is a local array `CfConnectSyncRoot` reads
    // synchronously within this call; `context_ptr` points at a `Context<F>`
    // the caller keeps alive for at least as long as the connection stays
    // open.
    unsafe {
        CfConnectSyncRoot(
            PCWSTR::from_raw(path_wide.as_ptr()),
            callback_table.as_ptr(),
            Some(context_ptr),
            CF_CONNECT_FLAG_NONE,
        )
    }
    .map_err(|e| FsError::from(e).context("connecting the cfapi sync root"))
}

/// Recover the [`Ino`] embedded as a placeholder's `FileIdentity` by
/// [`to_create_info`]. `None` for anything else — a corrupt or foreign
/// identity, which should never happen for an entry this backend created —
/// so callers can fail the request cleanly instead of trusting a garbage
/// offset. The mountpoint itself carries no identity at all; see
/// [`handle_fetch_placeholders`] for how that's handled.
fn decode_ino(identity: *const c_void, length: u32) -> Option<Ino> {
    if identity.is_null() || length as usize != size_of::<u64>() {
        return None;
    }
    // SAFETY: cfapi guarantees `identity` points at `length` readable bytes
    // for the duration of the callback; the length check above confirms it is
    // exactly the 8 bytes this backend always writes as a `FileIdentity`.
    let bytes = unsafe { std::slice::from_raw_parts(identity.cast::<u8>(), size_of::<u64>()) };
    Some(Ino(u64::from_le_bytes(bytes.try_into().ok()?)))
}

unsafe extern "system" fn fetch_placeholders<F: ReadOnlyFs>(
    info: *const CF_CALLBACK_INFO,
    _params: *const CF_CALLBACK_PARAMETERS,
) {
    // SAFETY: cfapi guarantees `info` is non-null and valid for the duration
    // of this callback.
    let info = unsafe { &*info };
    // A panic crossing this `extern "system"` boundary is undefined behavior;
    // catching it here is the backstop, not a substitute for not panicking.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_fetch_placeholders::<F>(info);
    }));
}

fn handle_fetch_placeholders<F: ReadOnlyFs>(info: &CF_CALLBACK_INFO) {
    // SAFETY: `CallbackContext` is the pointer `mount` passed to
    // `CfConnectSyncRoot`, which points at a live `Context<F>` for as long as
    // the connection is open — guaranteed because `unmount` disconnects
    // before dropping it.
    let context = unsafe { &*info.CallbackContext.cast::<Context<F>>() };

    // The mountpoint itself carries no `FileIdentity` — see
    // `connect_sync_root`'s docs — so an empty one means the root; anything
    // else that fails to decode is a real problem.
    let dir = if info.FileIdentityLength == 0 {
        ROOT_INO
    } else if let Some(ino) = decode_ino(info.FileIdentity, info.FileIdentityLength) {
        ino
    } else {
        backend_warn!("anymount/cfapi: FETCH_PLACEHOLDERS with an unrecognized file identity");
        transfer_placeholders(info, &mut [], FsError::InvalidArgument.to_ntstatus());
        return;
    };

    match list_placeholders(&context.fs, dir) {
        // The listing stays owned across this call: `with_descriptors` holds
        // the buffers cfapi reads through for exactly as long as `CfExecute`
        // can touch them.
        Ok(listing) => listing.with_descriptors(|infos| transfer_placeholders(info, infos, 0)),
        Err(e) => transfer_placeholders(info, &mut [], e.to_ntstatus()),
    }
}

/// One directory entry, owning the buffers a
/// [`CF_PLACEHOLDER_CREATE_INFO`] points at.
struct PreparedEntry {
    identity: [u8; 8],
    name: Vec<u16>,
    attr: FileAttr,
}

/// A prepared directory listing: the descriptors cfapi wants, plus the
/// buffers they point into.
///
/// `CF_PLACEHOLDER_CREATE_INFO` carries `RelativeFileName` and `FileIdentity`
/// as raw pointers with no lifetime attached, so nothing in the type system
/// stops the buffers behind them from being dropped while the descriptors are
/// still in use. [`with_descriptors`](Self::with_descriptors) is the only way
/// to obtain the array, and it borrows `self` for the whole call, so the
/// backing store is provably alive for as long as cfapi can read through it.
/// Building the descriptors in a function that returned them would hand back
/// dangling pointers instead, which no compiler check would catch.
struct Placeholders {
    entries: Vec<PreparedEntry>,
}

impl Placeholders {
    /// Build the descriptor array and run `f` with it, keeping the buffers it
    /// points into borrowed for the duration.
    fn with_descriptors<R>(&self, f: impl FnOnce(&mut [CF_PLACEHOLDER_CREATE_INFO]) -> R) -> R {
        let mut infos: Vec<CF_PLACEHOLDER_CREATE_INFO> = self
            .entries
            .iter()
            .map(|e| to_create_info(&e.name, &e.identity, &e.attr))
            .collect();
        f(&mut infos)
    }
}

/// Prepare one entry per child of `dir`, via [`readdir::emit`] with
/// [`Dots::Omit`] — `FETCH_PLACEHOLDERS` has no `.`/`..` concept and no size
/// budget, so the sink always accepts and the listing is paged only because
/// [`ReadOnlyFs::readdir`] is allowed to return a partial page.
///
/// An entry whose `getattr` fails is skipped and logged rather than failing
/// the whole listing: better an incomplete directory than none at all.
fn list_placeholders<F: ReadOnlyFs>(fs: &F, dir: Ino) -> Result<Placeholders> {
    let mut listed = Vec::new();
    readdir::emit(fs, dir, 0, Dots::Omit, |entry| {
        listed.push((entry.ino, entry.name.to_os_string()));
        Sink::Accepted
    })?;

    let mut entries = Vec::with_capacity(listed.len());
    for (ino, name) in listed {
        match fs.getattr(ino) {
            Ok(attr) => entries.push(PreparedEntry {
                identity: ino.0.to_le_bytes(),
                name: to_wide(&name),
                attr,
            }),
            Err(e) => backend_warn!(
                "anymount/cfapi: getattr for {} failed, omitting it: {e}",
                name.to_string_lossy()
            ),
        }
    }

    Ok(Placeholders { entries })
}

fn to_create_info(
    name_wide: &[u16],
    id_bytes: &[u8; 8],
    attr: &FileAttr,
) -> CF_PLACEHOLDER_CREATE_INFO {
    CF_PLACEHOLDER_CREATE_INFO {
        RelativeFileName: PCWSTR::from_raw(name_wide.as_ptr()),
        FsMetadata: to_fs_metadata(attr),
        FileIdentity: id_bytes.as_ptr().cast(),
        FileIdentityLength: id_bytes.len() as u32,
        Flags: CF_PLACEHOLDER_CREATE_FLAG_NONE,
        Result: HRESULT(0),
        CreateUsn: 0,
    }
}

fn to_fs_metadata(attr: &FileAttr) -> CF_FS_METADATA {
    let attributes = match attr.kind {
        FileKind::Directory => FILE_ATTRIBUTE_DIRECTORY.0,
        FileKind::File => FILE_ATTRIBUTE_READONLY.0,
    };
    CF_FS_METADATA {
        BasicInfo: FILE_BASIC_INFO {
            CreationTime: to_filetime(attr.ctime),
            LastAccessTime: to_filetime(attr.atime),
            LastWriteTime: to_filetime(attr.mtime),
            ChangeTime: to_filetime(attr.ctime),
            FileAttributes: attributes,
        },
        FileSize: attr.size as i64,
    }
}

/// Convert a [`SystemTime`] to a Windows `FILETIME` value (100ns intervals
/// since 1601-01-01), the form [`FILE_BASIC_INFO`]'s timestamp fields take. A
/// time before the Unix epoch — not producible by [`FileAttr`]'s own
/// constructors, but not worth panicking over either — becomes 0, cfapi's
/// "no change/unspecified" sentinel.
fn to_filetime(t: SystemTime) -> i64 {
    const UNIX_EPOCH_IN_FILETIME: i64 = 116_444_736_000_000_000;
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => {
            UNIX_EPOCH_IN_FILETIME
                + (d.as_secs() as i64) * 10_000_000
                + (d.subsec_nanos() as i64) / 100
        }
        Err(_) => 0,
    }
}

fn transfer_placeholders(
    info: &CF_CALLBACK_INFO,
    infos: &mut [CF_PLACEHOLDER_CREATE_INFO],
    status: i32,
) {
    let op_info = CF_OPERATION_INFO {
        StructSize: size_of::<CF_OPERATION_INFO>() as u32,
        Type: CF_OPERATION_TYPE_TRANSFER_PLACEHOLDERS,
        ConnectionKey: info.ConnectionKey,
        TransferKey: info.TransferKey,
        CorrelationVector: ptr::null(),
        SyncStatus: ptr::null(),
        RequestKey: info.RequestKey,
    };
    let mut op_params = CF_OPERATION_PARAMETERS {
        ParamSize: (offset_of!(CF_OPERATION_PARAMETERS, Anonymous)
            + size_of::<CF_OPERATION_PARAMETERS_0_4>()) as u32,
        Anonymous: CF_OPERATION_PARAMETERS_0 {
            TransferPlaceholders: CF_OPERATION_PARAMETERS_0_4 {
                Flags: CF_OPERATION_TRANSFER_PLACEHOLDERS_FLAG_NONE,
                CompletionStatus: NTSTATUS(status),
                PlaceholderTotalCount: infos.len() as i64,
                PlaceholderArray: infos.as_mut_ptr(),
                PlaceholderCount: infos.len() as u32,
                EntriesProcessed: 0,
            },
        },
    };
    // SAFETY: `op_info`/`op_params` are valid, correctly sized structures;
    // `infos` outlives this synchronous call.
    if let Err(e) = unsafe { CfExecute(&op_info, &mut op_params) } {
        backend_warn!("anymount/cfapi: CfExecute(TRANSFER_PLACEHOLDERS) failed: {e}");
    }
}

unsafe extern "system" fn fetch_data<F: ReadOnlyFs>(
    info: *const CF_CALLBACK_INFO,
    params: *const CF_CALLBACK_PARAMETERS,
) {
    // SAFETY: cfapi guarantees both pointers are non-null and valid for the
    // duration of this callback.
    let info = unsafe { &*info };
    let params = unsafe { &*params };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_fetch_data::<F>(info, params);
    }));
}

fn handle_fetch_data<F: ReadOnlyFs>(info: &CF_CALLBACK_INFO, params: &CF_CALLBACK_PARAMETERS) {
    // SAFETY: this function is only ever registered for
    // `CF_CALLBACK_TYPE_FETCH_DATA`, so the platform initialized the
    // `FetchData` union member.
    let fetch = unsafe { params.Anonymous.FetchData };
    let offset = fetch.RequiredFileOffset;
    let length = fetch.RequiredLength;

    // SAFETY: see `handle_fetch_placeholders`.
    let context = unsafe { &*info.CallbackContext.cast::<Context<F>>() };

    let Some(ino) = decode_ino(info.FileIdentity, info.FileIdentityLength) else {
        backend_warn!("anymount/cfapi: FETCH_DATA with an unrecognized file identity");
        transfer_data(
            info,
            &[],
            offset,
            length,
            FsError::InvalidArgument.to_ntstatus(),
        );
        return;
    };

    if let Err(e) = stream_fetch(&context.fs, info, ino, offset, length) {
        backend_warn!("anymount/cfapi: fetching data for ino {ino} failed: {e}");
    }
}

/// Stream `length` bytes starting at `offset` from `ino` to the platform in
/// [`TRANSFER_CHUNK`]-sized pieces, opening and releasing a handle around the
/// whole fetch.
///
/// Every chunk actually read is transferred with success as it's read, rather
/// than buffering the whole range in memory first — cfapi always requests the
/// whole file in one `FETCH_DATA` call (see this module's docs), so for a
/// large backup archive this keeps memory use bounded to one chunk. On
/// failure partway through, the remaining, untransferred range is failed
/// explicitly with the mapped `NTSTATUS` so the platform does not wait out its
/// callback timeout for bytes that are never coming.
fn stream_fetch<F: ReadOnlyFs>(
    fs: &F,
    info: &CF_CALLBACK_INFO,
    ino: Ino,
    offset: i64,
    length: i64,
) -> Result<()> {
    let fh = fs.open(ino)?;
    let result = stream_chunks(fs, info, fh, offset, length);
    if let Err(e) = fs.release(fh) {
        backend_warn!("anymount/cfapi: release of ino {ino} failed: {e}");
    }
    result
}

fn stream_chunks<F: ReadOnlyFs>(
    fs: &F,
    info: &CF_CALLBACK_INFO,
    fh: FileHandle,
    offset: i64,
    length: i64,
) -> Result<()> {
    if length <= 0 {
        // An empty file: nothing to read, but the platform still needs a
        // completion signal for the range it asked about.
        transfer_data(info, &[], offset, 0, 0);
        return Ok(());
    }

    let mut buf = vec![0u8; TRANSFER_CHUNK.min(length as usize)];
    let mut sent: i64 = 0;

    while sent < length {
        let want = ((length - sent) as u64).min(buf.len() as u64) as usize;
        match fs.read_at(fh, (offset + sent) as u64, &mut buf[..want]) {
            Ok(0) => {
                // End of file before the platform's required length was
                // satisfied — the implementor's reported size disagrees with
                // what it can actually read.
                let e = FsError::Io(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
                transfer_data(info, &[], offset + sent, length - sent, e.to_ntstatus());
                return Err(e);
            }
            Ok(n) => {
                transfer_data(info, &buf[..n], offset + sent, n as i64, 0);
                sent += n as i64;
            }
            Err(e) => {
                transfer_data(info, &[], offset + sent, length - sent, e.to_ntstatus());
                return Err(e);
            }
        }
    }
    Ok(())
}

fn transfer_data(info: &CF_CALLBACK_INFO, buffer: &[u8], offset: i64, length: i64, status: i32) {
    let op_info = CF_OPERATION_INFO {
        StructSize: size_of::<CF_OPERATION_INFO>() as u32,
        Type: CF_OPERATION_TYPE_TRANSFER_DATA,
        ConnectionKey: info.ConnectionKey,
        TransferKey: info.TransferKey,
        CorrelationVector: ptr::null(),
        SyncStatus: ptr::null(),
        RequestKey: info.RequestKey,
    };
    let mut op_params = CF_OPERATION_PARAMETERS {
        ParamSize: (offset_of!(CF_OPERATION_PARAMETERS, Anonymous)
            + size_of::<CF_OPERATION_PARAMETERS_0_0>()) as u32,
        Anonymous: CF_OPERATION_PARAMETERS_0 {
            TransferData: CF_OPERATION_PARAMETERS_0_0 {
                Flags: CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
                CompletionStatus: NTSTATUS(status),
                Buffer: buffer.as_ptr().cast(),
                Offset: offset,
                Length: length,
            },
        },
    };
    // SAFETY: `op_info`/`op_params` are valid, correctly sized structures;
    // `buffer` (read only when `status` indicates success) outlives this
    // synchronous call.
    if let Err(e) = unsafe { CfExecute(&op_info, &mut op_params) } {
        backend_warn!("anymount/cfapi: CfExecute(TRANSFER_DATA) failed: {e}");
    }
}

#[cfg(test)]
#[path = "cfapi_tests.rs"]
mod cfapi_tests;
