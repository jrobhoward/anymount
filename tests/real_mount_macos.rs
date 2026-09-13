//! A real mount on a real machine, checked with the tools a user would reach
//! for rather than with the crate's own return values.
//!
//! `#[ignore]` by default: it mounts a filesystem, so it is a deliberate act
//! rather than something `cargo test` does on the way past. Run it with
//! `cargo test --test real_mount_macos -- --ignored --test-threads=1`.
//!
//! What it checks is the thing a mount's return code cannot: that the bytes
//! read back through the mountpoint are the bytes the filesystem serves, and
//! that the credential the transport was chosen to hide is not on any surface
//! another local account can read.

#![cfg(all(target_os = "macos", feature = "nfs"))]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(non_snake_case)]

use std::ffi::{OsStr, OsString};
use std::process::Command;

use anymount::{
    DirEntry, FileAttr, FileHandle, FileKind, FsError, Ino, MountBuilder, ROOT_INO, ReadOnlyFs,
    Result,
};

const FILE_INO: Ino = Ino(2);
const FILE_NAME: &str = "numbers.txt";
/// Content with enough bytes to cross a read boundary rather than fitting in
/// whatever the first `READ3` happens to ask for.
const CONTENT_LEN: usize = 300_000;

fn content() -> Vec<u8> {
    (0..CONTENT_LEN).map(|i| b'a' + (i % 26) as u8).collect()
}

struct OneFile(Vec<u8>);

impl ReadOnlyFs for OneFile {
    fn lookup(&self, parent: Ino, name: &OsStr) -> Result<FileAttr> {
        if parent == ROOT_INO && name == OsStr::new(FILE_NAME) {
            Ok(FileAttr::file(FILE_INO, self.0.len() as u64))
        } else {
            Err(FsError::NotFound)
        }
    }

    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        match ino {
            ROOT_INO => Ok(FileAttr::dir(ROOT_INO)),
            FILE_INO => Ok(FileAttr::file(FILE_INO, self.0.len() as u64)),
            _ => Err(FsError::NotFound),
        }
    }

    fn readdir(&self, ino: Ino, offset: u64) -> Result<Vec<DirEntry>> {
        if ino != ROOT_INO {
            return Err(FsError::NotADirectory);
        }
        if offset > 0 {
            return Ok(Vec::new());
        }
        Ok(vec![DirEntry {
            ino: FILE_INO,
            name: OsString::from(FILE_NAME),
            kind: FileKind::File,
        }])
    }

    fn open(&self, ino: Ino) -> Result<FileHandle> {
        if ino == FILE_INO {
            Ok(FileHandle(0))
        } else {
            Err(FsError::IsADirectory)
        }
    }

    fn read_at(&self, _fh: FileHandle, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let start = (offset as usize).min(self.0.len());
        let end = (start + buf.len()).min(self.0.len());
        let n = end - start;
        buf[..n].copy_from_slice(&self.0[start..end]);
        Ok(n)
    }

    fn release(&self, _fh: FileHandle) -> Result<()> {
        Ok(())
    }
}

fn stdout_of(program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running {program}: {e}"));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A mountpoint that is cleaned up even when an assertion unwinds.
struct Dir(std::path::PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = Command::new("umount").arg(&self.0).output();
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn mountpoint(tag: &str) -> Dir {
    let path = std::path::PathBuf::from(format!("/tmp/anymount-test-{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir(&path);
    Dir(path)
}

/// The whole point of the local-socket transport, checked end to end.
#[test]
#[ignore = "mounts a real filesystem"]
#[cfg(feature = "nfs-local-socket")]
fn local_socket____a_real_mount____serves_the_right_bytes_and_publishes_no_credential() {
    let dir = mountpoint("local");
    let expected = content();

    let mount = MountBuilder::new(&dir.0)
        .fs_name("anymount-test")
        .nfs_require_local_socket(true)
        .mount(OneFile(expected.clone()))
        .expect("mount over the local socket");

    assert!(
        mount.nfs_uses_local_socket(),
        "nfs_require_local_socket must not have fallen back"
    );

    // `mount(8)`, not the crate's own report.
    let table = stdout_of("mount", &[]);
    let line = table
        .lines()
        .find(|l| l.contains(&dir.0.display().to_string()))
        .expect("the mountpoint appears in mount(8) output");
    assert!(line.contains("nfs"), "mounted as NFS: {line}");

    // Nothing is listening on the network for this mount.
    let sockets = stdout_of("netstat", &["-an", "-p", "tcp"]);
    assert!(
        !sockets.contains(".2049 "),
        "no NFS port should be listening"
    );

    // The directory listing and the file's size, through the mount.
    let listing = stdout_of("ls", &["-la", &dir.0.display().to_string()]);
    assert!(listing.contains(FILE_NAME), "ls shows the file: {listing}");

    let path = dir.0.join(FILE_NAME);
    let stat = stdout_of("stat", &["-f", "%z", &path.display().to_string()]);
    assert_eq!(
        stat.trim(),
        CONTENT_LEN.to_string(),
        "stat reports the size"
    );

    // The bytes, digested by a tool that never saw the filesystem, against a
    // digest computed here.
    let through_mount = stdout_of("shasum", &["-a", "256", &path.display().to_string()]);
    let digest = through_mount
        .split_whitespace()
        .next()
        .expect("shasum output");
    assert_eq!(
        digest,
        sha256_hex(&expected),
        "content matches byte for byte"
    );

    mount.unmount().expect("unmount");

    // The socket directory is gone, not left in /tmp.
    let leftovers: Vec<_> = std::fs::read_dir("/tmp")
        .expect("read /tmp")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".anymount-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "unmount left socket directories behind: {:?}",
        leftovers.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
}

/// The fallback still has to work, and has to report itself honestly.
#[test]
#[ignore = "mounts a real filesystem"]
#[cfg(feature = "nfs-tcp")]
fn tcp____a_real_mount____serves_the_right_bytes_and_stops_answering_mnt() {
    let dir = mountpoint("tcp");
    let expected = content();

    // Reaching the TCP path deliberately: the local socket cannot bind when
    // its parent is not a directory this process can create in.
    let mount = MountBuilder::new(&dir.0)
        .fs_name("anymount-test")
        .mount(OneFile(expected.clone()))
        .expect("mount");

    let path = dir.0.join(FILE_NAME);
    let through_mount = stdout_of("shasum", &["-a", "256", &path.display().to_string()]);
    let digest = through_mount
        .split_whitespace()
        .next()
        .expect("shasum output");
    assert_eq!(digest, sha256_hex(&expected));

    if !mount.nfs_uses_local_socket() {
        // The public value is in the mount table, which is what makes this the
        // weaker path — but it no longer opens anything, because `MNT` stopped
        // being answered when `mount_nfs` exited.
        let locations = stdout_of("nfsstat", &["-m"]);
        assert!(
            locations.contains("/export/"),
            "the export path is visible through nfsstat -m, as documented"
        );
    }

    mount.unmount().expect("unmount");
}

fn sha256_hex(bytes: &[u8]) -> String {
    let out = {
        use std::io::Write as _;
        let mut child = Command::new("shasum")
            .args(["-a", "256"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn shasum");
        child
            .stdin
            .as_mut()
            .expect("shasum stdin")
            .write_all(bytes)
            .expect("write to shasum");
        child.wait_with_output().expect("shasum output")
    };
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("digest")
        .to_owned()
}
