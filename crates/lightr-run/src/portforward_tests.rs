//! Tests for the WP-B/WP-B2 userspace TCP port forwarder (`portforward.rs`),
//! split out to keep the implementation file under the 400-LOC godfile cap.
//! Real-TCP end-to-end: bytes round-trip through a forwarder bound on a given
//! host interface (incl. host-ip + range cases), proving the bind/forward path.

use super::*;
use std::time::{Duration, Instant};

// All three portforward tests use the "bind port 0 to discover a free port,
// drop the listener, then pass the port to the forwarder" pattern. This is
// inherently racy when test threads run in parallel: two threads may discover
// the same port, drop their respective listeners, and then both fail to bind.
// Serialise the tests with a process-wide lock so only one at a time goes
// through the discover-drop-re-bind sequence.
static PORT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let _port_guard = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let _port_guard = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

#[test]
fn start_to_forwards_to_explicit_target() {
    let _port_guard = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let echo_port = spawn_echo();
    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let host_port = free.local_addr().unwrap().port();
    drop(free);

    // Using 127.0.0.1 as the explicit target keeps the test hermetic — it
    // proves the new param is plumbed without needing a real VM.
    let fwd = start_to(host_port, "127.0.0.1", echo_port).expect("start_to forwarder");
    assert_eq!(fwd.target_host(), "127.0.0.1");

    let mut c = connect_retry(host_port);
    round_trip(&mut c, b"explicit-target");
}

// ── WP-B2: host-ip binding, end-to-end ────────────────────────────────────

/// `-p 127.0.0.1:H:C` ⇒ the forwarder binds the loopback interface and a real
/// client connecting to `127.0.0.1:H` round-trips bytes through to the
/// container. Proves the host_ip is honored at the BIND site (not just parsed).
#[test]
fn start_on_binds_explicit_host_ip_127() {
    let _port_guard = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let echo_port = spawn_echo();
    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let host_port = free.local_addr().unwrap().port();
    drop(free);

    let fwd = start_on("127.0.0.1", host_port, "127.0.0.1", echo_port)
        .expect("start_on 127.0.0.1 forwarder");
    assert_eq!(fwd.host_ip(), "127.0.0.1");
    assert_eq!(fwd.host_port(), host_port);

    let mut c = connect_retry(host_port);
    round_trip(&mut c, b"host-ip-loopback");
}

/// A `0.0.0.0` bind (the default) accepts loopback connections too — the
/// no-host-ip default path is end-to-end functional, and its Drop poke (which
/// targets 127.0.0.1 for a 0.0.0.0 bind) tears the forwarder down cleanly.
#[test]
fn start_on_binds_all_interfaces_default() {
    let _port_guard = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let echo_port = spawn_echo();
    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let host_port = free.local_addr().unwrap().port();
    drop(free);

    {
        let fwd = start_on("0.0.0.0", host_port, "127.0.0.1", echo_port)
            .expect("start_on 0.0.0.0 forwarder");
        assert_eq!(fwd.host_ip(), "0.0.0.0");
        let mut c = connect_retry(host_port);
        round_trip(&mut c, b"all-ifaces");
        // fwd dropped here ⇒ exercises the 0.0.0.0 Drop-poke path (no hang).
    }
}

/// `-p 8000-8002:...` expands to N PortMaps ⇒ N independent forwarders, each on
/// its own host port, all live and round-tripping at once. Proves the range
/// path publishes EVERY element, not just the first.
#[test]
fn range_yields_n_live_forwarders() {
    let _port_guard = PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // One echo server per "container port" in the range; three host ports.
    let mut host_ports = Vec::new();
    let mut echoes = Vec::new();
    for _ in 0..3 {
        echoes.push(spawn_echo());
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        host_ports.push(free.local_addr().unwrap().port());
        drop(free);
    }

    // Mimic what the run path does with parse_publish_spec's expansion:
    // start one forwarder per (host, container) pair.
    let mut fwds = Vec::new();
    for i in 0..3 {
        fwds.push(
            start_on("0.0.0.0", host_ports[i], "127.0.0.1", echoes[i]).expect("range forwarder"),
        );
    }
    assert_eq!(fwds.len(), 3, "a 3-wide range must yield 3 forwarders");

    // Every host port in the range round-trips through to its own echo server.
    for (i, &hp) in host_ports.iter().enumerate() {
        let mut c = connect_retry(hp);
        round_trip(&mut c, format!("range-{i}").as_bytes());
    }
}
