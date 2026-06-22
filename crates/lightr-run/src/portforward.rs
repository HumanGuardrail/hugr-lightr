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
    host_ip: String,
    host_port: u16,
    container_port: u16,
    target_host: String,
    stop: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept loop with a throwaway connection; best-effort.
        // Poke the actual bound interface so a non-loopback bind is also woken;
        // a `0.0.0.0` bind also accepts loopback, so `127.0.0.1` is the safe poke
        // there. Both branches are best-effort — process exit closes the listener
        // regardless.
        let poke_ip = if self.host_ip == "0.0.0.0" {
            "127.0.0.1"
        } else {
            &self.host_ip
        };
        let _ = TcpStream::connect((poke_ip, self.host_port));
        if let Some(jh) = self.accept_thread.take() {
            let _ = jh.join();
        }
    }
}

impl Forwarder {
    /// The host interface this forwarder bound on (e.g. `127.0.0.1`, `0.0.0.0`).
    pub fn host_ip(&self) -> &str {
        &self.host_ip
    }

    /// The host port this forwarder bound on [`Self::host_ip`].
    pub fn host_port(&self) -> u16 {
        self.host_port
    }

    /// The container port this forwarder dials on `127.0.0.1`.
    pub fn container_port(&self) -> u16 {
        self.container_port
    }

    /// The target host this forwarder dials (`127.0.0.1` native, or a guest IP).
    pub fn target_host(&self) -> &str {
        &self.target_host
    }
}

/// Bind `127.0.0.1:host_port` and run an accept loop that forwards every
/// connection to `127.0.0.1:container_port`. Returns a [`Forwarder`] handle.
///
/// A bind failure is returned as an error (the supervisor logs + skips it). The
/// accept loop runs on its own thread; each accepted connection is served on a
/// further thread, so slow/long-lived clients never block new accepts.
pub fn start(host_port: u16, container_port: u16) -> Result<Forwarder> {
    start_to(host_port, "127.0.0.1", container_port)
}

/// Bind `127.0.0.1:host_port` and forward every accepted connection to
/// `target_host:container_port`. `target_host` is `127.0.0.1` for a native run,
/// or a microVM guest IP for a `vz` container run. Returns a [`Forwarder`].
pub fn start_to(host_port: u16, target_host: &str, container_port: u16) -> Result<Forwarder> {
    start_on("127.0.0.1", host_port, target_host, container_port)
}

/// Bind `host_ip:host_port` and forward every accepted connection to
/// `target_host:container_port` (WP-B2). `host_ip` is the host interface the
/// published port binds on — Docker's `-p HOST_IP:HOST:CONTAINER` (e.g.
/// `127.0.0.1` for loopback-only, `0.0.0.0` for all interfaces). The older
/// [`start`]/[`start_to`] are thin wrappers that bind `127.0.0.1` for back-compat
/// with their existing call sites. `target_host` is `127.0.0.1` for a native run,
/// or a microVM guest IP for a `vz` container run.
pub fn start_on(
    host_ip: &str,
    host_port: u16,
    target_host: &str,
    container_port: u16,
) -> Result<Forwarder> {
    let addr = format!("{host_ip}:{host_port}");
    let listener = TcpListener::bind(&addr).map_err(LightrError::Io)?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);

    let dial_base = format!("{target_host}:{container_port}");

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
            let dest = dial_base.clone();
            std::thread::spawn(move || {
                if let Ok(outbound) = TcpStream::connect(&dest) {
                    proxy_bidirectional(inbound, outbound);
                }
                // If the container isn't reachable, drop `inbound` and end the
                // thread — the client sees a closed connection.
            });
        }
    });

    Ok(Forwarder {
        host_ip: host_ip.to_string(),
        host_port,
        container_port,
        target_host: target_host.to_string(),
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
#[path = "portforward_tests.rs"]
mod tests;
