//! Networking Phase 1 acceptance — `lightr run -d -p HOST:CONTAINER`.
//!
//! Proves the daemonless userspace forward-proxy: a detached published run is
//! reachable on the host port (forwarded to 127.0.0.1:CONTAINER where the
//! server listens), and `lightr stop` tears the forwarder down with the run.
//!
//! The server is `python3 -m http.server` (present on macOS + Linux CI). If
//! `python3` is not on PATH the reachability test SKIPS gracefully (prints why,
//! passes) — a missing server binary must not fail the suite.
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo test -p lightr-acceptance.

#[path = "common/mod.rs"]
#[allow(dead_code)] // shared helpers; this suite uses only `lightr_cmd`
mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::lightr_cmd;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Guard: stop a detached run on Drop so no process/forwarder is leaked.
// ---------------------------------------------------------------------------
struct RunGuard {
    id: String,
    home: PathBuf,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let _ = lightr_cmd(&self.home)
            .args(["stop", &self.id, "--grace", "1"])
            .output();
    }
}

/// Parse `id=<id>` from stdout.
fn parse_id_from_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("id=") {
            return rest.trim().to_owned();
        }
    }
    panic!("could not find 'id=<id>' in stdout:\n{text}");
}

/// True if `python3` is on PATH (probe `python3 --version`).
fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Pick a free localhost TCP port by binding :0 and releasing it.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().unwrap().port()
}

/// Try one HTTP GET / through `127.0.0.1:port`; return the response bytes on
/// success (connect + write + any bytes read back), else None.
fn http_probe(port: u16) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(800)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_millis(800)))
        .ok();
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .ok()?;
    stream.flush().ok();
    let mut buf = Vec::new();
    // Read whatever the server sends before it closes / times out.
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 16 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// Poll `http_probe` up to `timeout`; return the first response that arrives.
fn poll_http(port: u16, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(resp) = http_probe(port) {
            return Some(resp);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// True once `127.0.0.1:port` refuses connections (forwarder gone).
fn port_closed(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

// ---------------------------------------------------------------------------
// Reachability + teardown of a detached published run.
// ---------------------------------------------------------------------------
#[test]
fn net_published_run_is_reachable_then_torn_down() {
    if !python3_available() {
        eprintln!(
            "SKIP net_published_run_is_reachable_then_torn_down: python3 not on PATH \
             (cannot start a real HTTP server). Phase-1 forwarder wiring is still \
             exercised by the lightr-run portforward unit tests."
        );
        return;
    }

    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();

    // High ephemeral ports to avoid clashes.
    let host_port = free_port().max(39000);
    let container_port = free_port().max(39001);

    // Start a detached published run: server binds 127.0.0.1:<container_port>,
    // forwarder publishes 127.0.0.1:<host_port> → it.
    let hp = host_port.to_string();
    let cp = container_port.to_string();
    let publish = format!("{host_port}:{container_port}");
    let out = lightr_cmd(home.path())
        .args([
            "run",
            "-d",
            "-p",
            &publish,
            "--dir",
            ws.path().to_str().unwrap(),
            "--",
            "python3",
            "-m",
            "http.server",
            &cp,
            "--bind",
            "127.0.0.1",
        ])
        .output()
        .expect("run -d -p must launch");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "run -d -p must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = parse_id_from_stdout(&out.stdout);
    let _guard = RunGuard {
        id: id.clone(),
        home: home.path().to_path_buf(),
    };

    // Reachability: an HTTP response must come back THROUGH the forwarder.
    let resp = poll_http(host_port, Duration::from_secs(8));
    assert!(
        resp.is_some(),
        "no HTTP response on host port {hp} within 8s (forwarder→127.0.0.1:{cp})"
    );
    let resp = resp.unwrap();
    let text = String::from_utf8_lossy(&resp);
    assert!(
        text.starts_with("HTTP/"),
        "response through forwarder must be HTTP, got: {:?}",
        &text.chars().take(40).collect::<String>()
    );

    // Teardown: stop the run, then the host port must stop serving.
    let stop_out = lightr_cmd(home.path())
        .args(["stop", &id, "--grace", "2"])
        .output()
        .expect("stop must launch");
    let _ = stop_out.status.code();

    assert!(
        port_closed(host_port, Duration::from_secs(5)),
        "host port {hp} must stop serving after stop (forwarder dropped)"
    );
}

// ---------------------------------------------------------------------------
// Fast negative: -p without -d exits 2 (no server needed).
// ---------------------------------------------------------------------------
#[test]
fn net_publish_without_detach_exits_2() {
    let home = TempDir::new().unwrap();
    let publish = format!("{}:{}", free_port().max(39000), free_port().max(39001));
    let out = lightr_cmd(home.path())
        .args(["run", "-p", &publish, "--", "true"])
        .output()
        .expect("run -p (no -d) must launch");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        2,
        "-p without -d must exit 2; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires -d"),
        "error must explain -p requires -d, got: {stderr}"
    );
}
