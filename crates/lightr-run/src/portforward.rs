//! Networking Phase 1 — userspace TCP port forwarder for published runs.
//!
//! This is the daemonless forward-proxy model: exactly how rootless docker /
//! podman publish ports (slirp/pasta are userspace). A detached, supervised run
//! that publishes `-p HOST:CONTAINER` gets, for each mapping, a forwarder that
//! binds `127.0.0.1:HOST`, accepts connections, and for EACH accepted
//! connection dials `127.0.0.1:CONTAINER` (where the run's server listens) and
//! pumps bytes both ways.
//!
//! It generalizes compose's proven `proxy_bidirectional` (lightr-build) and
//! IMPROVES on it: the accept-loop serves multiple connections — sequential AND
//! concurrent — by spawning a thread per accepted connection. Compose is left
//! untouched in Phase 1.
//!
//! Lifetime: `start` returns a [`Forwarder`] the caller owns. Dropping it (or
//! the supervisor exiting) closes the listener; the accept loop then ends. TCP
//! only in v1.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use lightr_core::{LightrError, Result};

/// A live port forwarder. Owns the accept-loop thread; the caller controls its
/// lifetime by holding/dropping this handle.
///
/// Shutdown: the accept loop checks a shared `stop` flag each iteration. On
/// `Drop` we set the flag and make one throwaway connection to `127.0.0.1:host`
/// to unblock the pending `accept`, then join the thread. In the supervisor's
/// real lifetime the process simply exits and the OS closes the listener — the
/// explicit Drop path is what makes a dropped `Forwarder` (e.g. in tests) tear
/// down cleanly.
pub struct Forwarder {
    host_port: u16,
    container_port: u16,
    stop: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept loop with a throwaway connection; best-effort.
        let _ = TcpStream::connect(("127.0.0.1", self.host_port));
        if let Some(jh) = self.accept_thread.take() {
            let _ = jh.join();
        }
    }
}

impl Forwarder {
    /// The host port this forwarder bound on `127.0.0.1`.
    pub fn host_port(&self) -> u16 {
        self.host_port
    }

    /// The container port this forwarder dials on `127.0.0.1`.
    pub fn container_port(&self) -> u16 {
        self.container_port
    }
}

/// Bind `127.0.0.1:host_port` and run an accept loop that forwards every
/// connection to `127.0.0.1:container_port`. Returns a [`Forwarder`] handle.
///
/// A bind failure is returned as an error (the supervisor logs + skips it). The
/// accept loop runs on its own thread; each accepted connection is served on a
/// further thread, so slow/long-lived clients never block new accepts.
pub fn start(host_port: u16, container_port: u16) -> Result<Forwarder> {
    let addr = format!("127.0.0.1:{host_port}");
    let listener = TcpListener::bind(&addr).map_err(LightrError::Io)?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);

    let accept_thread = std::thread::spawn(move || {
        // Each `accept` yields one inbound client. After every accept we check
        // the stop flag, so a Drop (which sets the flag and pokes the port)
        // ends the loop. A bare listener-close (process exit) also ends it via
        // an accept error.
        for inbound in listener.incoming() {
            if stop_thread.load(Ordering::SeqCst) {
                break;
            }
            let inbound = match inbound {
                Ok(s) => s,
                Err(_) => break,
            };
            // Serve this connection on its own thread so concurrent clients are
            // independent. Dial the container only after a client connects.
            std::thread::spawn(move || {
                let dest = format!("127.0.0.1:{container_port}");
                if let Ok(outbound) = TcpStream::connect(&dest) {
                    proxy_bidirectional(inbound, outbound);
                }
                // If the container isn't reachable, drop `inbound` and end the
                // thread — the client sees a closed connection.
            });
        }
    });

    Ok(Forwarder {
        host_port,
        container_port,
        stop,
        accept_thread: Some(accept_thread),
    })
}

/// Bidirectional byte pump between two TCP streams. Copies the proven compose
/// shape (lightr-build `proxy_bidirectional`): two halves, two reader threads,
/// each closing on EOF/error. Joins both before returning.
fn proxy_bidirectional(a: TcpStream, b: TcpStream) {
    let a2 = a.try_clone();
    let b2 = b.try_clone();
    if a2.is_err() || b2.is_err() {
        return;
    }
    let mut a_read = a;
    let mut b_read = b;
    let mut a_write = a2.unwrap();
    let mut b_write = b2.unwrap();

    let t1 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match a_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if b_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let t2 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match b_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if a_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let _ = t1.join();
    let _ = t2.join();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Spawn a localhost echo server on an ephemeral port. Returns the bound
    /// port; the server runs until the test process exits (best-effort).
    fn spawn_echo() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind echo");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for inbound in listener.incoming() {
                let Ok(mut stream) = inbound else { break };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    loop {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if stream.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        port
    }

    /// Connect to `127.0.0.1:port`, retrying briefly so we don't race the
    /// listener coming up.
    fn connect_retry(port: u16) -> TcpStream {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => return s,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("connect to 127.0.0.1:{port} failed: {e}"),
            }
        }
    }

    /// One round-trip through a connected stream: write `msg`, read it back.
    fn round_trip(stream: &mut TcpStream, msg: &[u8]) {
        stream.write_all(msg).expect("write through forwarder");
        stream.flush().ok();
        let mut got = vec![0u8; msg.len()];
        stream.read_exact(&mut got).expect("read echoed bytes");
        assert_eq!(got, msg, "bytes must round-trip through the forwarder");
    }

    #[test]
    fn forwards_bytes_round_trip() {
        let container_port = spawn_echo();
        // host_port 0 ⇒ ephemeral; read the real port back off the forwarder.
        // We can't bind 0 and learn the port from the public API, so bind a
        // real ephemeral port ourselves to discover a free one, drop it, then
        // hand it to the forwarder.
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let host_port = free.local_addr().unwrap().port();
        drop(free);

        let fwd = start(host_port, container_port).expect("start forwarder");
        assert_eq!(fwd.host_port(), host_port);
        assert_eq!(fwd.container_port(), container_port);

        let mut c = connect_retry(host_port);
        round_trip(&mut c, b"hello-phase1");
    }

    #[test]
    fn handles_a_second_connection() {
        let container_port = spawn_echo();
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let host_port = free.local_addr().unwrap().port();
        drop(free);

        let _fwd = start(host_port, container_port).expect("start forwarder");

        // First connection.
        let mut c1 = connect_retry(host_port);
        round_trip(&mut c1, b"first");

        // Second, independent connection through the same forwarder — proves the
        // accept loop serves multiple (sequential) connections, not just one.
        let mut c2 = connect_retry(host_port);
        round_trip(&mut c2, b"second");

        // And concurrent: keep c1 open while c2 also round-trips.
        round_trip(&mut c1, b"first-again");
        round_trip(&mut c2, b"second-again");
    }
}
