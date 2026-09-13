//! The socket operations the accept loop needs, abstracted over the two
//! transports the macOS mount path can use.
//!
//! `rpc::read_message` and `rpc::write_message` are already generic over
//! [`Read`] and [`Write`], so a connection needs nothing beyond those two once
//! it has been accepted. What is left is the handful of calls that are not
//! part of either trait: putting the listener into non-blocking mode so the
//! accept loop can poll the stop flag, clearing the flag an accepted socket
//! inherits, and arming the read timeout that bounds each worker's wait.
//!
//! Both implementations are compiled wherever the wire layer is, rather than
//! being gated on the transport features. Which listener is *bound* is the
//! decision the features make, and that decision lives in this module's
//! parent; keeping the forwarding impls unconditional means the accept loop
//! and its tests compile the same way in every feature combination.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

/// A bound socket the server accepts RPC connections on.
pub(super) trait Listener {
    /// The connection type [`accept_conn`](Listener::accept_conn) yields.
    ///
    /// `Send + 'static` because the accept loop hands each one to its own
    /// worker thread.
    type Conn: Connection + Send + 'static;

    /// Put the listener into non-blocking mode, so `accept_conn` returns
    /// `WouldBlock` instead of parking a thread that would never see the stop
    /// flag.
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;

    /// Accept one connection, discarding the peer address.
    ///
    /// The address is never used: a loopback peer is not identified by it, and
    /// a Unix socket peer has none worth reading. Dropping it here keeps the
    /// two transports to one signature.
    fn accept_conn(&self) -> io::Result<Self::Conn>;
}

/// An accepted connection carrying RPC messages.
pub(super) trait Connection: Read + Write {
    /// Clear the non-blocking flag an accepted socket inherits from its
    /// listener on macOS and the BSDs, without which the read timeout below
    /// is silently inert.
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;

    /// Bound each read, so a worker notices shutdown between requests.
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl Listener for TcpListener {
    type Conn = TcpStream;

    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        TcpListener::set_nonblocking(self, nonblocking)
    }

    fn accept_conn(&self) -> io::Result<TcpStream> {
        TcpListener::accept(self).map(|(stream, _addr)| stream)
    }
}

impl Connection for TcpStream {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        TcpStream::set_nonblocking(self, nonblocking)
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
}

impl Listener for UnixListener {
    type Conn = UnixStream;

    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        UnixListener::set_nonblocking(self, nonblocking)
    }

    fn accept_conn(&self) -> io::Result<UnixStream> {
        UnixListener::accept(self).map(|(stream, _addr)| stream)
    }
}

impl Connection for UnixStream {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        UnixStream::set_nonblocking(self, nonblocking)
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
