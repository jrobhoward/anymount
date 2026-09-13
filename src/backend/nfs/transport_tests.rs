#![allow(non_snake_case)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! Both transports through one generic function, so a signature that only
//! happens to fit `TcpStream` does not pass for `UnixStream` too.

use super::*;

use std::thread;

/// Accept one connection through the trait, echo the byte it carries.
fn echo_once<L: Listener>(listener: L) -> u8 {
    listener.set_nonblocking(false).expect("blocking mode");
    let mut conn = listener.accept_conn().expect("accept");
    conn.set_nonblocking(false).expect("clear inherited flag");
    conn.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let mut byte = [0u8; 1];
    conn.read_exact(&mut byte).expect("read");
    byte[0]
}

#[test]
fn listener____loopback_tcp____accepts_and_reads_through_the_trait() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");

    let server = thread::spawn(move || echo_once(listener));
    let mut client = TcpStream::connect(addr).expect("connect");
    client.write_all(&[0x5a]).expect("write");

    assert_eq!(server.join().expect("server thread"), 0x5a);
}

#[test]
fn listener____unix_socket____accepts_and_reads_through_the_trait() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("s");

    let listener = UnixListener::bind(&path).expect("bind unix socket");
    let server = thread::spawn(move || echo_once(listener));

    let mut client = UnixStream::connect(&path).expect("connect");
    client.write_all(&[0xa5]).expect("write");

    assert_eq!(server.join().expect("server thread"), 0xa5);
}

/// `accept` inherits `O_NONBLOCK` from its listener on macOS and the BSDs, and
/// a non-blocking socket ignores `SO_RCVTIMEO`. `serve_connection` clears the
/// flag through the trait, so the trait has to actually reach the socket —
/// a no-op impl would leave every read failing instantly instead of waiting.
#[test]
fn connection____unix_socket____honours_the_read_timeout_after_clearing_nonblocking() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("s");

    let listener = UnixListener::bind(&path).expect("bind unix socket");
    Listener::set_nonblocking(&listener, true).expect("non-blocking listener");

    let _client = UnixStream::connect(&path).expect("connect");

    let mut accepted = None;
    for _ in 0..100 {
        if let Ok(conn) = listener.accept_conn() {
            accepted = Some(conn);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let mut conn = accepted.expect("accept the client connection");

    let wait = Duration::from_millis(50);
    Connection::set_nonblocking(&conn, false).expect("clear inherited flag");
    conn.set_read_timeout(Some(wait)).expect("read timeout");

    let start = std::time::Instant::now();
    let mut buf = [0u8; 4];
    let result = conn.read(&mut buf);
    let waited = start.elapsed();

    assert!(result.is_err(), "an idle connection must not yield bytes");
    assert!(
        waited >= wait / 2,
        "read returned after {waited:?} instead of waiting ~{wait:?}; \
         the socket is still non-blocking, so the read timeout is inert"
    );
}
