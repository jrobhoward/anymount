//! TCP accept loop and per-connection RPC dispatch, routing by `(prog, vers,
//! proc)` to the MOUNT or NFS procedure tables.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::fs::ReadOnlyFs;

use super::handle::FileHandle3;
use super::mount_proto;
use super::nfs_proto::{self, Ctx};
use super::rpc::{self, HeaderOutcome};
use super::xdr::Reader;
use super::{MOUNT_PROG, NFS_PROG};

/// How often the accept loop wakes to check the stop flag.
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Concurrent connections served at once.
///
/// The listener binds to loopback, so only local processes can reach it, and
/// only one of them — this mount's own `mount_nfs` client — has any business
/// connecting. Anything else is either a stray probe or a process opening
/// sockets in a loop, and without a cap each of those costs a thread that
/// lives until unmount. A client that has legitimately saturated this will
/// be accepted as soon as an existing connection closes; the kernel holds it
/// in the listen backlog meanwhile.
const MAX_CONNECTIONS: usize = 16;

/// Runs the accept loop until `stop` is set, spawning one worker thread per
/// accepted connection.
pub(super) fn run<F: ReadOnlyFs>(
    listener: TcpListener,
    fs: Arc<F>,
    handle: Arc<FileHandle3>,
    stop: Arc<AtomicBool>,
) {
    let _ = listener.set_nonblocking(true);
    let mut workers: Vec<JoinHandle<()>> = Vec::new();

    while !stop.load(Ordering::Relaxed) {
        // Reap first, so a connection that has already closed frees its slot
        // rather than counting against the cap until unmount.
        workers.retain(|w| !w.is_finished());

        if workers.len() >= MAX_CONNECTIONS {
            std::thread::sleep(ACCEPT_POLL_INTERVAL);
            continue;
        }

        match listener.accept() {
            Ok((stream, _addr)) => {
                let fs = Arc::clone(&fs);
                let handle = Arc::clone(&handle);
                let stop = Arc::clone(&stop);
                workers.push(std::thread::spawn(move || {
                    serve_connection(stream, &fs, &handle, &stop);
                }));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => std::thread::sleep(ACCEPT_POLL_INTERVAL),
        }
    }

    for w in workers {
        let _ = w.join();
    }
}

fn serve_connection<F: ReadOnlyFs>(
    mut stream: TcpStream,
    fs: &Arc<F>,
    handle: &Arc<FileHandle3>,
    stop: &Arc<AtomicBool>,
) {
    // `accept` inherits the listener's `O_NONBLOCK` on macOS and the BSDs
    // (Linux does not), and `SO_RCVTIMEO` is ignored on a non-blocking
    // socket — both verified on macOS 26. Without this the read timeout
    // below is silently inert: reads return `EAGAIN` immediately, and the
    // loop's `WouldBlock` arm turns the intended 50ms wait into an
    // unbounded retry. Clear the inherited flag so the timeout is real.
    let _ = stream.set_nonblocking(false);

    // A blocking read with a timeout lets this worker also notice shutdown
    // without a dedicated cancellation mechanism per connection.
    let _ = stream.set_read_timeout(Some(ACCEPT_POLL_INTERVAL));

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let body = match rpc::read_message(&mut stream) {
            Ok(Some(body)) => body,
            Ok(None) => return, // clean EOF
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => return,
        };

        let Some(reply) = handle_message(&body, fs.as_ref(), handle) else {
            return; // malformed header: nothing trustworthy to reply with
        };
        if rpc::write_message(&mut stream, &reply).is_err() {
            return;
        }
    }
}

fn handle_message<F: ReadOnlyFs>(body: &[u8], fs: &F, handle: &FileHandle3) -> Option<Vec<u8>> {
    let mut r = Reader::new(body);
    match rpc::read_call_header(&mut r) {
        HeaderOutcome::Malformed => None,
        HeaderOutcome::BadRpcvers(xid) => Some(rpc::rpc_mismatch_reply(xid)),
        HeaderOutcome::Call(call) => {
            let reply = match (call.prog, call.vers) {
                (MOUNT_PROG, 3) => {
                    let outcome = mount_proto::dispatch(call.proc_, &mut r, handle);
                    rpc::build_reply(call.xid, outcome)
                }
                (MOUNT_PROG, _) => rpc::prog_mismatch_reply(call.xid),
                (NFS_PROG, 3) => {
                    let ctx = Ctx {
                        fs,
                        handle,
                        fsid: 1,
                    };
                    let outcome = nfs_proto::dispatch(call.proc_, &mut r, &ctx);
                    rpc::build_reply(call.xid, outcome)
                }
                (NFS_PROG, _) => rpc::prog_mismatch_reply(call.xid),
                _ => rpc::prog_unavail_reply(call.xid),
            };
            Some(reply)
        }
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;
