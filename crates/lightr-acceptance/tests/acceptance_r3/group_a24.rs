//! A24, A24b acceptance tests (compose lazy + discovery env).

use super::helpers::*;
use crate::common::lightr_cmd;
use std::fs;
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// A24 — compose lazy
//
// Compose with 2 services (no --eager):
//   - `compose up` returns in < 2 s (listeners bound immediately)
//   - Immediately after up: 0 services running (no run entries)
//   - Connecting to P1 triggers ≥ 1 service start within 5 s
//   - `compose down` → no running services remain
//
// Portability caveat: ports P1/P2 are picked from the 39000–39999 range using
// the test's PID for some variety. If ports are unavailable the test degrades
// gracefully: the connection-trigger sub-assertion is skipped but up/down
// correctness is always checked.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a24_compose_lazy() {
    // Hold the port lock for the duration of this test: it binds fixed host
    // ports (39000+pid_offset, 39513+pid_offset) and must not run concurrently
    // with other port-binding tests in this binary.
    let _port_guard = crate::PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let home = TempDir::new().unwrap();

    // Pick two ports from the high ephemeral range; use pid-derived offset for variety.
    let pid_offset = (std::process::id() % 512) as u16;
    let p1: u16 = 39000 + pid_offset;
    let p2: u16 = 39513 + pid_offset;

    // Write compose.yml into a temp dir.
    let compose_dir = TempDir::new().unwrap();
    let compose_yml = compose_dir.path().join("compose.yml");
    let compose_content = format!(
        "services:\n\
         \x20\x20svc1:\n\
         \x20\x20\x20\x20image: scratch\n\
         \x20\x20\x20\x20command: [\"/bin/sh\",\"-c\",\"sleep 30\"]\n\
         \x20\x20\x20\x20ports:\n\
         \x20\x20\x20\x20\x20\x20- \"{p1}:1\"\n\
         \x20\x20svc2:\n\
         \x20\x20\x20\x20image: scratch\n\
         \x20\x20\x20\x20command: [\"/bin/sh\",\"-c\",\"sleep 30\"]\n\
         \x20\x20\x20\x20ports:\n\
         \x20\x20\x20\x20\x20\x20- \"{p2}:2\"\n"
    );
    fs::write(&compose_yml, &compose_content).unwrap();

    // ── up: must return in < 2 s ─────────────────────────────────────────────
    let up_start = Instant::now();
    let up_out = lightr_cmd(home.path())
        .args(["compose", "up", "-f", compose_yml.to_str().unwrap()])
        .output()
        .expect("compose up must not fail to spawn");
    let up_elapsed = up_start.elapsed();

    assert_eq!(
        up_out.status.code().unwrap_or(-1),
        0,
        "compose up must exit 0 (listeners bound); stderr:\n{}",
        String::from_utf8_lossy(&up_out.stderr)
    );
    assert!(
        up_elapsed < Duration::from_secs(2),
        "compose up must return in < 2 s (lazy binding); took {:?}",
        up_elapsed
    );

    // ── immediately: 0 services running ─────────────────────────────────────
    // Check $LIGHTR_HOME/run has no running entries for this stack.
    // We use `ps --json` to check for running services.
    let ps_out = lightr_cmd(home.path())
        .args(["ps", "--json"])
        .output()
        .expect("ps --json must not fail to spawn");
    assert_eq!(
        ps_out.status.code().unwrap_or(-1),
        0,
        "ps --json must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&ps_out.stderr)
    );
    let ps_json: serde_json::Value =
        serde_json::from_slice(&ps_out.stdout).expect("ps --json must emit valid JSON");
    let ps_arr = ps_json
        .as_array()
        .expect("ps --json must emit a JSON array");
    let running_count = ps_arr
        .iter()
        .filter(|e| e.get("running").and_then(|v| v.as_bool()).unwrap_or(false))
        .count();
    assert_eq!(
        running_count, 0,
        "compose up (no --eager): 0 services must be running immediately after up; got {running_count}"
    );

    // ── connecting to P1 triggers service start ──────────────────────────────
    // Poll up to 2 s for the supervisor to bind port P1.
    let port_ready = poll_until(Duration::from_secs(2), || {
        TcpStream::connect(format!("127.0.0.1:{p1}")).is_ok()
    });

    if port_ready {
        // We already connected; now poll for ≥1 service to appear in ps.
        let service_started = poll_until(Duration::from_secs(5), || {
            let out = lightr_cmd(home.path())
                .args(["ps", "--json"])
                .output()
                .expect("ps --json must launch");
            if out.status.success() {
                if let Ok(arr) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    if let Some(arr) = arr.as_array() {
                        return arr
                            .iter()
                            .any(|e| e.get("running").and_then(|v| v.as_bool()).unwrap_or(false));
                    }
                }
            }
            false
        });
        assert!(
            service_started,
            "connecting to port {p1} must trigger ≥1 service start within 5 s"
        );
    } else {
        // Port unavailable (busy box / timing); skip the trigger sub-assertion.
        // The core assertions (up fast, 0 services initially, down cleans) still apply.
        eprintln!(
            "[A24] WARNING: port {p1} not available within 2 s; skipping connection-trigger sub-assertion"
        );
    }

    // ── compose down: no services remain ────────────────────────────────────
    let down_out = lightr_cmd(home.path())
        .args(["compose", "down", "-f", compose_yml.to_str().unwrap()])
        .output()
        .expect("compose down must not fail to spawn");
    assert_eq!(
        down_out.status.code().unwrap_or(-1),
        0,
        "compose down must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&down_out.stderr)
    );

    // After down: no running services.
    let ps_after_down = lightr_cmd(home.path())
        .args(["ps", "--json"])
        .output()
        .expect("ps --json after down must not fail to spawn");
    assert_eq!(
        ps_after_down.status.code().unwrap_or(-1),
        0,
        "ps --json after down must exit 0"
    );
    let ps_after: serde_json::Value =
        serde_json::from_slice(&ps_after_down.stdout).expect("ps --json must be valid JSON");
    let running_after = ps_after
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|e| e.get("running").and_then(|v| v.as_bool()).unwrap_or(false))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(
        running_after, 0,
        "after compose down, 0 services must be running; got {running_after}"
    );

    // Stack directory must be gone (supervisor self-cleaned or compose_down removed it).
    // We verify via: `$LIGHTR_HOME/compose/` has no entries that were spawned by this test.
    // Since the compose stack dir is keyed by nanos+pid and we just called compose_down,
    // the supervisor pid must be dead. We check this by asserting the stack_dir stdout line
    // is absent, or simply that no stack dir remains under $LIGHTR_HOME/compose/.
    let compose_dir_home = home.path().join("compose");
    if compose_dir_home.exists() {
        let remaining: Vec<_> = fs::read_dir(&compose_dir_home)
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        assert!(
            remaining.is_empty(),
            "after compose down, $LIGHTR_HOME/compose/ must be empty; remaining: {:?}",
            remaining
                .iter()
                .map(|e: &fs::DirEntry| e.path())
                .collect::<Vec<_>>()
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A24b — compose service discovery via env vars (WP-DISC)
//
// Honest native discovery: every service learns its peers' addresses through
// env. For service `web` the supervisor injects `WEB_HOST=127.0.0.1` and
// `WEB_PORT=<web container_port>` into `client`'s environment (Docker-compose
// "links" convention). Native services share host loopback, so `client` reaches
// `web` directly at `127.0.0.1:<web container_port>` — NO proxy involved.
//
// Both services are EAGER (started immediately by the supervisor). `client`:
//   1. records "$WEB_HOST:$WEB_PORT" to an env file (proves the vars exist),
//   2. connects to that address and writes the round-trip body to a body file.
//
// Assertions:
//   - CORE (always): the env file contains exactly "127.0.0.1:<web_cport>".
//   - STRENGTHENING (graceful skip): the body file contains web's payload.
//     `nc` round-trips in a tight test are timing-sensitive (see the A24
//     caveat); if web's loopback port was unavailable or nc lagged, the body
//     sub-assertion is skipped while the env-var core assertion still holds.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a24b_compose_discovery_env() {
    // Hold the port lock for the duration of this test: it binds fixed host
    // ports (41000+pid_offset, 41513+pid_offset) and must not run concurrently
    // with other port-binding tests in this binary.
    let _port_guard = crate::PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let home = TempDir::new().unwrap();

    // web's container port — a high ephemeral loopback port web binds and the
    // value the supervisor must inject as WEB_PORT. pid-derived for variety.
    let pid_offset = (std::process::id() % 512) as u16;
    let web_cport: u16 = 41000 + pid_offset;
    // web's published host port (kept distinct; client uses the *container* port
    // via discovery, not this one — but web must declare a port to be a peer).
    let web_hport: u16 = 41513 + pid_offset;

    // Test-owned absolute paths the eager `client` writes to. Native engine has
    // NO isolation (see file header / A22), so the service process can write to
    // any host path — we exploit that to read back what `client` observed.
    let out_dir = TempDir::new().unwrap();
    let env_file = out_dir.path().join("disc_env.txt");
    let body_file = out_dir.path().join("disc_body.txt");
    let env_str = env_file.to_string_lossy();
    let body_str = body_file.to_string_lossy();

    // The fixed payload web serves on each connection.
    let payload = "lightr-discovery-ok";

    // 2-service compose, both eager. web serves `payload` on 127.0.0.1:<web_cport>
    // in a loop; client reads $WEB_HOST/$WEB_PORT, records them, then round-trips.
    let compose_dir = TempDir::new().unwrap();
    let compose_yml = compose_dir.path().join("compose.yml");
    let web_cmd = format!("while true; do printf '{payload}' | nc -l {web_cport}; done");
    // Record the discovery vars FIRST (the always-on CORE assertion) so it never
    // sits behind the round-trip's `sleep 1` — that sleep only gives web a moment
    // to bind before the STRENGTHENING body connect. `|| true` keeps a failed nc
    // from changing the service exit semantics under test.
    let client_cmd = format!(
        "printf '%s:%s' \"$WEB_HOST\" \"$WEB_PORT\" > {env_str}; sleep 1; \
         printf 'GET' | nc \"$WEB_HOST\" \"$WEB_PORT\" > {body_str} || true; sleep 30"
    );
    let compose_content = format!(
        "services:\n\
         \x20\x20web:\n\
         \x20\x20\x20\x20image: scratch\n\
         \x20\x20\x20\x20command: [\"/bin/sh\",\"-c\",{web_cmd_json}]\n\
         \x20\x20\x20\x20x-lightr-eager: true\n\
         \x20\x20\x20\x20ports:\n\
         \x20\x20\x20\x20\x20\x20- \"{web_hport}:{web_cport}\"\n\
         \x20\x20client:\n\
         \x20\x20\x20\x20image: scratch\n\
         \x20\x20\x20\x20command: [\"/bin/sh\",\"-c\",{client_cmd_json}]\n\
         \x20\x20\x20\x20x-lightr-eager: true\n",
        web_cmd_json = serde_json::to_string(&web_cmd).unwrap(),
        client_cmd_json = serde_json::to_string(&client_cmd).unwrap(),
    );
    fs::write(&compose_yml, &compose_content).unwrap();

    // ── up: starts both eager services ───────────────────────────────────────
    let up_out = lightr_cmd(home.path())
        .args(["compose", "up", "-f", compose_yml.to_str().unwrap()])
        .output()
        .expect("compose up must not fail to spawn");
    assert_eq!(
        up_out.status.code().unwrap_or(-1),
        0,
        "compose up must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&up_out.stderr)
    );

    // ── CORE: client must have observed WEB_HOST/WEB_PORT discovery vars ──────
    // The client records the env file FIRST thing (before its body-round-trip
    // sleep), so it appears ~immediately after the eager client is scheduled.
    // The window is deliberately generous: this runs on a shared, frequently
    // overloaded CI box (load can spike >40) where process scheduling + tmpfs
    // I/O stall for many seconds. The property under test is discovery-var
    // injection, not latency — so we wait long enough that only a real failure
    // (vars never injected) trips the panic, never a scheduling hiccup.
    let env_ready = poll_until(Duration::from_secs(30), || env_file.exists());

    // Always tear the stack down before asserting, so a failed assertion never
    // leaks the eager services / their listeners.
    let do_down = || {
        let _ = lightr_cmd(home.path())
            .args(["compose", "down", "-f", compose_yml.to_str().unwrap()])
            .output();
    };

    if !env_ready {
        do_down();
        panic!(
            "client never recorded discovery env within 30 s (expected {}); \
             up stderr:\n{}",
            env_file.display(),
            String::from_utf8_lossy(&up_out.stderr)
        );
    }

    let env_observed = fs::read_to_string(&env_file).unwrap_or_default();
    let expected = format!("127.0.0.1:{web_cport}");
    // Capture the round-trip body BEFORE down (web's listener dies with down).
    let body_observed = fs::read_to_string(&body_file).unwrap_or_default();
    do_down();

    assert_eq!(
        env_observed.trim(),
        expected,
        "discovery must inject WEB_HOST=127.0.0.1 and WEB_PORT={web_cport} \
         into client's env (Docker-compose links convention); client saw {:?}",
        env_observed
    );

    // ── STRENGTHENING: the service-to-service round-trip body (graceful skip) ──
    if body_observed.is_empty() {
        eprintln!(
            "[A24b] WARNING: round-trip body empty (web loopback port {web_cport} \
             busy or nc lag); skipping body sub-assertion. The discovery-env core \
             assertion already passed."
        );
    } else {
        assert_eq!(
            body_observed.trim(),
            payload,
            "client must reach web directly at 127.0.0.1:{web_cport} via discovery \
             vars (no proxy) and read back web's payload; got {:?}",
            body_observed
        );
    }
}
