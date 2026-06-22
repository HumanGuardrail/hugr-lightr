//! Tests for the streaming plane (WP-CRI-STREAM) — open_exec / open_attach.
//!
//! Parallel-safe: each test owns a unique tempdir home (atomic counter + nanos,
//! no process-global mutation) and spawns real, short-lived host processes.
//! unix-only (the plane is unix-only; the windows gate compiles but never runs).

use std::io::Read;
use std::path::PathBuf;

use crate::vocab::{BackendError, ContainerConfig, ContainerId, ContainerState, ContainerStatus};
use crate::{CriBackend, LightrBackend};

fn temp_home() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("lightr-cri-stream-{nanos}-{n}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn cfg(command: Vec<&str>) -> ContainerConfig {
    ContainerConfig {
        name: "c".into(),
        attempt: 0,
        image_ref: "img".into(),
        command: command.into_iter().map(String::from).collect(),
        args: Vec::new(),
        working_dir: String::new(),
        envs: Vec::new(),
        mounts: Vec::new(),
        labels: Default::default(),
        annotations: Default::default(),
        log_path: String::new(),
        tty: false,
        stdin: false,
    }
}

/// Create + start a container with `command` (empty ⇒ keep-alive). Returns a
/// Running container id (or the keep-alive one). Polls until Running.
fn running_container(b: &LightrBackend, command: Vec<&str>) -> ContainerId {
    let sb = crate::vocab::SandboxId("sb-test".into());
    let id = b.create_container(&sb, cfg(command)).unwrap();
    b.start_container(&id).unwrap();
    id
}

fn wait_state(b: &LightrBackend, id: &ContainerId, want: ContainerState) {
    for _ in 0..200 {
        if let Ok(ContainerStatus { state, .. }) = b.container_status(id) {
            if state == want {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("container {} did not reach {want:?}", id.0);
}

// ── open_exec: non-tty stdout + waiter exit code ─────────────────────────────

#[test]
fn open_exec_pipe_yields_stdout_and_zero_exit() {
    let b = LightrBackend::new(temp_home());
    let id = running_container(&b, vec![]); // keep-alive container

    let mut s = b
        .open_exec(&id, &["echo".into(), "hello-exec".into()], false, false)
        .unwrap();
    assert!(s.pty_master.is_none());
    let mut out = String::new();
    s.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert_eq!(out, "hello-exec\n");
    // stderr stream exists (piped) but is empty.
    let mut err = String::new();
    s.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert!(err.is_empty(), "stderr: {err:?}");
    // The real waiter reaps the child and yields the exit code.
    assert_eq!(s.waiter.wait().unwrap(), 0);
}

#[test]
fn open_exec_waiter_yields_nonzero_exit() {
    let b = LightrBackend::new(temp_home());
    let id = running_container(&b, vec![]);

    let s = b
        .open_exec(
            &id,
            &["sh".into(), "-c".into(), "exit 7".into()],
            false,
            false,
        )
        .unwrap();
    assert_eq!(s.waiter.wait().unwrap(), 7);
}

#[test]
fn open_exec_stdin_pipe_present_when_requested() {
    let b = LightrBackend::new(temp_home());
    let id = running_container(&b, vec![]);

    // `cat` echoes stdin to stdout; feed it, close stdin, read it back.
    let mut s = b.open_exec(&id, &["cat".into()], false, true).unwrap();
    {
        use std::io::Write;
        let mut stdin = s.stdin.take().expect("stdin pipe requested");
        stdin.write_all(b"piped-in\n").unwrap();
        // drop stdin → EOF so cat exits
    }
    let mut out = String::new();
    s.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert_eq!(out, "piped-in\n");
    assert_eq!(s.waiter.wait().unwrap(), 0);
}

// ── open_exec: tty ───────────────────────────────────────────────────────────

#[test]
fn open_exec_tty_uses_pty_master_no_stderr() {
    let b = LightrBackend::new(temp_home());
    let id = running_container(&b, vec![]);

    let mut s = b
        .open_exec(&id, &["echo".into(), "hello-tty".into()], true, false)
        .unwrap();
    assert!(s.pty_master.is_some(), "tty must hand back a pty master");
    assert!(s.stderr.is_none(), "tty merges stderr onto the pty stream");
    assert!(
        s.stdin.is_none(),
        "tty: write to the master, no separate stdin"
    );

    // Drain the pty master on a thread (a master read can block until the slave
    // closes, and the post-close behavior — EOF vs EIO — is platform-specific,
    // so we read one chunk with a timeout rather than read_to_end). The echoed
    // line is in the pty buffer once `echo` writes it.
    let mut master = s.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 128];
        let n = master.read(&mut buf).unwrap_or(0);
        let _ = tx.send(buf[..n].to_vec());
    });
    let out = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("pty read timed out");
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("hello-tty"), "pty output: {text:?}");

    let code = s.waiter.wait().unwrap();
    assert_eq!(code, 0);
}

// ── open_exec: precondition / not-found ──────────────────────────────────────

#[test]
fn open_exec_requires_running_and_existing() {
    let b = LightrBackend::new(temp_home());
    // missing container → NotFound
    assert!(matches!(
        b.open_exec(&ContainerId("nope".into()), &["true".into()], false, false),
        Err(BackendError::NotFound(_))
    ));
    // created-but-not-started container → FailedPrecondition
    let sb = crate::vocab::SandboxId("sb".into());
    let id = b.create_container(&sb, cfg(vec!["true"])).unwrap();
    assert!(matches!(
        b.open_exec(&id, &["true".into()], false, false),
        Err(BackendError::FailedPrecondition(_))
    ));
    // empty command on a running container → InvalidArgument
    let rid = running_container(&b, vec![]);
    assert!(matches!(
        b.open_exec(&rid, &[], false, false),
        Err(BackendError::InvalidArgument(_))
    ));
}

// ── open_attach: pipe mode receives live output, waiter on exit ──────────────

#[test]
fn open_attach_pipe_receives_live_output() {
    let b = LightrBackend::new(temp_home());
    // A container that keeps emitting lines while Running, so an attach that
    // registers after start still catches subsequent ticks (no start race).
    let id = running_container(
        &b,
        vec![
            "sh",
            "-c",
            "i=0; while [ $i -lt 200 ]; do echo tick; i=$((i+1)); sleep 0.02; done",
        ],
    );
    wait_state(&b, &id, ContainerState::Running);

    let mut s = b.open_attach(&id).unwrap();
    assert!(s.pty_master.is_none());
    let mut stdout = s.stdout.take().expect("pipe-mode stdout sink");

    // Read at least one tick from the fan-out sink.
    let mut buf = [0u8; 64];
    let n = stdout.read(&mut buf).unwrap();
    assert!(n > 0, "attach received no bytes");
    let got = String::from_utf8_lossy(&buf[..n]);
    assert!(got.contains("tick"), "attach output: {got:?}");

    // Stop the container; the attach waiter completes on exit.
    b.stop_container(&id, 0).unwrap();
    wait_state(&b, &id, ContainerState::Exited);
    // Waiter returns the recorded exit code (SIGKILL ⇒ 137); just assert it
    // returns rather than blocking forever.
    let code = s.waiter.wait().unwrap();
    assert!(code != 0, "expected a non-zero (killed) exit, got {code}");
}

#[test]
fn open_attach_requires_running_and_existing() {
    let b = LightrBackend::new(temp_home());
    assert!(matches!(
        b.open_attach(&ContainerId("nope".into())),
        Err(BackendError::NotFound(_))
    ));
    let sb = crate::vocab::SandboxId("sb".into());
    let id = b.create_container(&sb, cfg(vec!["true"])).unwrap();
    assert!(matches!(
        b.open_attach(&id),
        Err(BackendError::FailedPrecondition(_))
    ));
}

// ── parallel-safe: two backends + two exec sessions concurrently ─────────────

#[test]
fn parallel_exec_sessions_are_independent() {
    let b1 = LightrBackend::new(temp_home());
    let b2 = LightrBackend::new(temp_home());
    let id1 = running_container(&b1, vec![]);
    let id2 = running_container(&b2, vec![]);

    let h1 = {
        let s = b1
            .open_exec(
                &id1,
                &["sh".into(), "-c".into(), "exit 3".into()],
                false,
                false,
            )
            .unwrap();
        std::thread::spawn(move || s.waiter.wait().unwrap())
    };
    let h2 = {
        let s = b2
            .open_exec(
                &id2,
                &["sh".into(), "-c".into(), "exit 5".into()],
                false,
                false,
            )
            .unwrap();
        std::thread::spawn(move || s.waiter.wait().unwrap())
    };
    assert_eq!(h1.join().unwrap(), 3);
    assert_eq!(h2.join().unwrap(), 5);
}
