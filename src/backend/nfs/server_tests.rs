#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! Routing only: which `(prog, vers)` reaches which procedure table, and what
//! a call for anything else is answered with. The procedures themselves are
//! covered by `nfs_proto_tests.rs` and `mount_proto_tests.rs`.

use super::*;
use crate::backend::nfs::xdr::Writer;
use crate::error::{FsError, Result};
use crate::types::{DirEntry, FileAttr, FileHandle, Ino};
use std::ffi::OsStr;
use std::net::{TcpListener, TcpStream};

/// The routing tests never reach a filesystem operation, so every method
/// fails: reaching one would be the bug.
struct NeverCalled;

impl ReadOnlyFs for NeverCalled {
    fn lookup(&self, _parent: Ino, _name: &OsStr) -> Result<FileAttr> {
        Err(FsError::NotFound)
    }
    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        Ok(FileAttr::dir(ino))
    }
    fn readdir(&self, _ino: Ino, _offset: u64) -> Result<Vec<DirEntry>> {
        Ok(Vec::new())
    }
    fn open(&self, _ino: Ino) -> Result<FileHandle> {
        Err(FsError::IsADirectory)
    }
    fn read_at(&self, _fh: FileHandle, _o: u64, _b: &mut [u8]) -> Result<usize> {
        Err(FsError::IsADirectory)
    }
    fn release(&self, _fh: FileHandle) -> Result<()> {
        Ok(())
    }
}

fn call(prog: u32, vers: u32, proc_: u32) -> Vec<u8> {
    let mut w = Writer::new();
    w.write_u32(77); // xid
    w.write_u32(0); // CALL
    w.write_u32(2); // rpcvers
    w.write_u32(prog);
    w.write_u32(vers);
    w.write_u32(proc_);
    for _ in 0..2 {
        w.write_u32(0); // AUTH_NONE
        w.write_u32(0);
    }
    w.into_bytes()
}

/// A server that still answers `MNT`, as one does between binding the socket
/// and `mount_nfs` exiting.
fn serving_mount() -> Config {
    Config {
        #[cfg(feature = "nfs-tcp")]
        serve_mount: Arc::new(AtomicBool::new(true)),
        #[cfg(feature = "nfs-tcp")]
        export: super::super::handle::ExportSecret::for_test(1),
    }
}

fn dispatch_to(prog: u32, vers: u32, proc_: u32) -> Option<Vec<u8>> {
    let handle = FileHandle3::for_test(1);
    handle_message(
        &call(prog, vers, proc_),
        &NeverCalled,
        &handle,
        &serving_mount(),
    )
}

/// `(xid, accept_stat)` from an accepted reply.
fn accepted(bytes: &[u8]) -> (u32, u32) {
    let mut r = Reader::new(bytes);
    let xid = r.read_u32().unwrap();
    assert_eq!(r.read_u32(), Some(1), "REPLY");
    assert_eq!(r.read_u32(), Some(0), "MSG_ACCEPTED");
    r.read_u32().unwrap();
    r.read_u32().unwrap();
    (xid, r.read_u32().unwrap())
}

#[test]
fn handle_message____nfs_v3_null____is_accepted() {
    let reply = dispatch_to(NFS_PROG, 3, 0).expect("a well-formed call gets a reply");
    assert_eq!(accepted(&reply), (77, 0));
}

#[cfg(feature = "nfs-tcp")]
#[test]
fn handle_message____mount_v3_null____is_accepted() {
    let reply = dispatch_to(MOUNT_PROG, 3, 0).expect("a well-formed call gets a reply");
    assert_eq!(accepted(&reply), (77, 0));
}

/// The MOUNT program is compiled in with `nfs-tcp` and only with it: the
/// local-socket transport hands the root handle to `mount(2)` itself. With
/// that transport alone, MOUNT is a program this server does not serve at any
/// version, so every call for it is PROG_UNAVAIL rather than PROG_MISMATCH.
#[cfg(not(feature = "nfs-tcp"))]
#[test]
fn handle_message____mount_without_the_tcp_transport____is_prog_unavail() {
    for vers in [2, 3] {
        let reply = dispatch_to(MOUNT_PROG, vers, 0).expect("a well-formed call gets a reply");
        assert_eq!(accepted(&reply), (77, 1), "vers {vers}"); // PROG_UNAVAIL
    }
}

#[test]
fn handle_message____a_program_this_server_does_not_serve____is_prog_unavail() {
    // 100000 is the portmapper. This server is reached by port, not through
    // it, so a call for it must be declined rather than misrouted.
    let reply = dispatch_to(100_000, 2, 3).expect("a well-formed call gets a reply");
    assert_eq!(accepted(&reply).1, 1); // PROG_UNAVAIL
}

#[test]
fn handle_message____a_version_other_than_three____is_prog_mismatch() {
    // MOUNT is only one of the served programs with `nfs-tcp`; the case
    // without it is covered above.
    #[cfg(feature = "nfs-tcp")]
    let progs = [NFS_PROG, MOUNT_PROG];
    #[cfg(not(feature = "nfs-tcp"))]
    let progs = [NFS_PROG];

    for prog in progs {
        let reply = dispatch_to(prog, 2, 0).expect("a well-formed call gets a reply");
        assert_eq!(accepted(&reply).1, 2, "prog {prog}"); // PROG_MISMATCH
    }
}

#[test]
fn handle_message____an_unknown_procedure____is_proc_unavail() {
    let reply = dispatch_to(NFS_PROG, 3, 99).expect("a well-formed call gets a reply");
    assert_eq!(accepted(&reply).1, 3); // PROC_UNAVAIL
}

#[test]
fn handle_message____a_malformed_header____gets_no_reply_at_all() {
    // No trustworthy xid, so there is nothing to address a reply to; the
    // caller closes the connection instead.
    let handle = FileHandle3::for_test(1);
    assert!(handle_message(b"\x00\x00", &NeverCalled, &handle, &serving_mount()).is_none());
}

#[test]
fn handle_message____an_unsupported_rpcvers____is_denied_rather_than_accepted() {
    let mut w = Writer::new();
    w.write_u32(77);
    w.write_u32(0); // CALL
    w.write_u32(3); // rpcvers this server does not speak
    let bytes = w.into_bytes();
    let handle = FileHandle3::for_test(1);
    let reply =
        handle_message(&bytes, &NeverCalled, &handle, &serving_mount()).expect("xid is known");
    let mut r = Reader::new(&reply);
    assert_eq!(r.read_u32(), Some(77));
    assert_eq!(r.read_u32(), Some(1), "REPLY");
    assert_eq!(r.read_u32(), Some(1), "MSG_DENIED");
}

/// `serve_connection` relies on `SO_RCVTIMEO` to bound each read so the loop
/// can check the stop flag between requests. On macOS and the BSDs `accept`
/// inherits `O_NONBLOCK` from the listener (Linux does not), and a
/// non-blocking socket ignores `SO_RCVTIMEO` entirely — so without an
/// explicit `set_nonblocking(false)` the timeout is silently inert and each
/// read fails with `EAGAIN` the moment it is called.
///
/// This pins the socket setup rather than the loop: after the same
/// `set_nonblocking(false)` + `set_read_timeout` the worker does, a read on
/// an idle connection must *wait* rather than fail instantly.
#[test]
fn serve_connection____accepted_socket____honours_the_read_timeout_rather_than_spinning() {
    use TcpStream as ClientStream;
    use std::io::Read;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    // The accept loop puts the listener in non-blocking mode; the flag is
    // what gets inherited, so the test has to reproduce it.
    listener.set_nonblocking(true).expect("set nonblocking");

    let _client = ClientStream::connect(addr).expect("connect");

    let mut accepted = None;
    for _ in 0..100 {
        if let Ok((stream, _)) = listener.accept() {
            accepted = Some(stream);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut stream = accepted.expect("accept the client connection");

    // Exactly what `serve_connection` does before its read loop.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(ACCEPT_POLL_INTERVAL));

    // The peer never sends anything, so this read can only time out.
    let start = std::time::Instant::now();
    let mut buf = [0u8; 4];
    let result = stream.read(&mut buf);
    let waited = start.elapsed();

    assert!(result.is_err(), "an idle connection must not yield bytes");
    assert!(
        waited >= ACCEPT_POLL_INTERVAL / 2,
        "read returned after {waited:?} instead of waiting ~{ACCEPT_POLL_INTERVAL:?}; \
         the socket is still non-blocking, so the read timeout is inert"
    );
}

/// A dropped connection frees its slot, and a client that redials is served.
///
/// `run` caps concurrent connections at `MAX_CONNECTIONS` and reaps finished
/// workers each time round the accept loop. If a closed connection kept its
/// slot, a client that reconnected often enough would find the server at
/// capacity and sit in the listen backlog until unmount — so this reconnects
/// more times than the cap allows and requires every one of them to be
/// answered.
///
/// This is the server's half of a reconnection, over the same `AF_UNIX`
/// transport the macOS local-socket path mounts. Whether the kernel NFS
/// client redials a connection it lost is a property of the client, not of
/// this code, and is not exercised here — see `docs/GAPS.md`.
#[test]
fn run____a_client_reconnecting_more_times_than_the_cap____is_served_every_time() {
    use std::os::unix::net::{UnixListener, UnixStream};

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).expect("bind the socket");

    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            run(
                listener,
                Arc::new(NeverCalled),
                Arc::new(FileHandle3::for_test(1)),
                Arc::new(serving_mount()),
                stop,
            );
        })
    };

    for cycle in 0..MAX_CONNECTIONS + 4 {
        let mut stream = UnixStream::connect(&path).expect("connect to the socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("arm a read timeout");

        rpc::write_message(&mut stream, &call(NFS_PROG, 3, 0)).expect("send NFS NULL");
        let reply = rpc::read_message(&mut stream)
            .unwrap_or_else(|e| panic!("cycle {cycle}: reading the reply failed: {e}"))
            .unwrap_or_else(|| panic!("cycle {cycle}: server closed without replying"));

        let (xid, accept_stat) = accepted(&reply);
        assert_eq!(xid, 77, "cycle {cycle}: wrong xid");
        assert_eq!(accept_stat, 0, "cycle {cycle}: not SUCCESS");

        // Closing is the point: the next iteration is a fresh connection.
        drop(stream);
    }

    stop.store(true, Ordering::Relaxed);
    server.join().expect("the accept loop exits when stopped");
}
