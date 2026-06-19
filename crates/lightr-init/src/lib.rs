//! lightr-init — the Linux guest's PID 1 (build-spec-prod.md §WP-B-init,
//! reworked 2026-06-12 for the real macOS `vz` boot).
//!
//! The LIBRARY is host-portable and fully unit-tested: the init lifecycle is
//! parameterized over [`GuestOps`] (mount / read-spec / enter-rootfs / spawn)
//! and [`ExitSink`] (exit-code report), so it runs on Intel/macOS today. The
//! real Linux syscalls live in `bin/init.rs` behind `#[cfg(target_os="linux")]`.
//!
//! ## Channel design (why files, not vsock/cmdline)
//!
//! macOS has NO host `AF_VSOCK`, and the kernel cmdline cannot carry args with
//! spaces (`sh -c 'exit 7'`) without bespoke quoting. So the host↔guest channel
//! is two small files on the **shared, writable** rootfs virtiofs share:
//!   - host writes the command [`InitSpec`] JSON to [`CMD_FILE`] before boot;
//!   - guest reads it, runs the command, writes the REAL exit code to
//!     [`EXIT_FILE`]; the host reads that back after the VM stops.
//!
//! The lifecycle never synthesizes a success — `sink.report()` always receives
//! the actual `spawn_wait` result (or 127 when the command cannot be spawned).

use serde::{Deserialize, Serialize};

/// virtiofs tag for the rootfs share (matches the Swift shim's `rootfs` tag).
pub const ROOTFS_TAG: &str = "rootfs";
/// Mount target for the rootfs virtiofs share (before chroot).
pub const ROOTFS_DEST: &str = "/newroot";

/// Command file: the host writes the [`InitSpec`] JSON here on the rootfs share
/// (so the guest sees it at `ROOTFS_DEST` + `CMD_FILE` before chroot). Replaces
/// kernel-cmdline `LIGHTR_CMD` (which cannot carry spaces). Must match
/// `CMD_FILE_NAME` in crates/lightr-engine/src/lib.rs (vz_impl).
pub const CMD_FILE: &str = "/.lightr-cmd";

/// Exit file: the guest writes its REAL exit code as a decimal integer here (on
/// the rootfs share, after chroot → rootfs root); the host reads it back after
/// the VM stops. The macOS `vz` exit channel (no host AF_VSOCK). Must match
/// `EXIT_FILE_NAME` in crates/lightr-engine/src/lib.rs (vz_impl).
pub const EXIT_FILE: &str = "/.lightr-exit-code";

/// Stdout capture file: the guest redirects the command's stdout here (on the
/// rootfs share, after chroot → rootfs root). The host reads it back after the
/// VM stops so the run can be MEMOIZED exactly like the native path — the vz
/// memo replays {exit, stdout, stderr} from the Action Cache on a HIT. The
/// macOS `vz` output channel (no host AF_VSOCK), the sibling of [`EXIT_FILE`].
pub const STDOUT_FILE: &str = "/.lightr-stdout";

/// Stderr capture file: the guest redirects the command's stderr here (on the
/// rootfs share, after chroot → rootfs root). The host reads it back after the
/// VM stops for the vz memo (replayed on a HIT). The sibling of [`STDOUT_FILE`]
/// / [`EXIT_FILE`].
pub const STDERR_FILE: &str = "/.lightr-stderr";

/// Exit code reported when the command cannot be spawned (ENOENT etc.). Matches
/// the shell "command not found" convention so the host sees a real, non-zero
/// outcome rather than a fabricated success.
pub const SPAWN_FAILED_CODE: i32 = 127;

/// What PID1 must do, as data — written by the host to [`CMD_FILE`] on the
/// rootfs share, read back by the guest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitSpec {
    pub command: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
}

impl InitSpec {
    /// Parse an `InitSpec` from canonical serde_json bytes. Roundtrip-stable
    /// with [`InitSpec::to_json`].
    pub fn from_json(b: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(b).map_err(|e| e.to_string())
    }

    /// Serialize to canonical serde_json bytes. Roundtrip-stable with
    /// [`InitSpec::from_json`].
    pub fn to_json(&self) -> Vec<u8> {
        // Serializing an owned struct of String/Vec never fails; surface any
        // future failure loudly rather than fabricate empty bytes.
        serde_json::to_vec(self).expect("InitSpec serializes to JSON")
    }
}

/// Where PID1 reports the guest process exit code. Seam: tests use a Vec, the
/// real guest writes [`EXIT_FILE`] on the rootfs share.
pub trait ExitSink {
    fn report(&mut self, code: i32) -> std::io::Result<()>;
}

/// OS actions PID1 performs, seamed for host-side testing. The real impls live
/// in `bin/init.rs` (`#[cfg(target_os="linux")]`); tests use a fake.
pub trait GuestOps {
    /// Mount the rootfs virtiofs share ([`ROOTFS_TAG`]) at [`ROOTFS_DEST`].
    fn mount_rootfs(&mut self) -> std::io::Result<()>;
    /// Read + parse the command [`InitSpec`] from [`CMD_FILE`] on the mounted
    /// rootfs (i.e. `ROOTFS_DEST` + `CMD_FILE`), before chroot.
    fn read_spec(&mut self) -> std::io::Result<InitSpec>;
    /// Enter the rootfs (chroot [`ROOTFS_DEST`] + chdir `/`) so the command
    /// resolves inside the guest rootfs, not the initrd.
    fn enter_rootfs(&mut self) -> std::io::Result<()>;
    /// Spawn the command, wait, return its exit code (128+signal on signal).
    fn spawn_wait(
        &mut self,
        cmd: &[String],
        cwd: &str,
        env: &[(String, String)],
    ) -> std::io::Result<i32>;
}

/// The init lifecycle: mount rootfs → read the command → enter the rootfs →
/// spawn → report the exit code. Fixed order.
///
/// Honesty invariant (the whole point of this WP): `sink.report()` is called
/// with the ACTUAL exit code — never a hardcoded success.
/// - A mount or spec-read failure propagates as `Err` and reports NOTHING (no
///   fake code — the host then maps the missing exit file to a real non-zero).
/// - A spawn failure (e.g. ENOENT) is a real outcome: report
///   [`SPAWN_FAILED_CODE`] (127) and return it.
pub fn run_init<M: GuestOps>(ops: &mut M, sink: &mut dyn ExitSink) -> std::io::Result<i32> {
    // 1. Mount the rootfs share. A mount failure is unrecoverable → propagate,
    //    report NOTHING.
    ops.mount_rootfs()?;

    // 2. Read the command the host placed on the share. A missing/garbled spec
    //    is also unrecoverable → propagate, report NOTHING.
    let spec = ops.read_spec()?;

    // 3. Enter the rootfs so the command resolves there (not the initrd).
    ops.enter_rootfs()?;

    // 4. Spawn and capture the REAL exit code. A spawn failure (command not
    //    found) is still a real outcome → 127, not an Err.
    let code = match ops.spawn_wait(&spec.command, &spec.cwd, &spec.env) {
        Ok(code) => code,
        Err(_) => SPAWN_FAILED_CODE,
    };

    // 5. Report the actual code, then return it. This is the line that kills the
    //    vz shim's old hardcoded exitCode=0.
    sink.report(code)?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn sample_spec() -> InitSpec {
        InitSpec {
            command: vec!["/bin/echo".to_string(), "hi".to_string()],
            cwd: "/work".to_string(),
            env: vec![
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("LANG".to_string(), "C".to_string()),
            ],
        }
    }

    // ── InitSpec json roundtrip ────────────────────────────────────────────

    #[test]
    fn initspec_json_roundtrip_is_stable() {
        let spec = sample_spec();
        let bytes = spec.to_json();
        let back = InitSpec::from_json(&bytes).expect("roundtrip parses");
        assert_eq!(spec, back, "roundtrip must preserve the spec");
        assert_eq!(bytes, back.to_json(), "serialization is stable");
    }

    #[test]
    fn initspec_from_json_rejects_garbage() {
        let err = InitSpec::from_json(b"{ not json").unwrap_err();
        assert!(!err.is_empty(), "parse error must carry a message");
    }

    // ── FakeOps / VecSink seams ────────────────────────────────────────────

    /// A captured `spawn_wait` call: (command, cwd, env).
    type SpawnCall = (Vec<String>, String, Vec<(String, String)>);

    /// Records the lifecycle steps in order and returns configurable outcomes.
    struct FakeOps {
        steps: Vec<&'static str>,
        spec: InitSpec,
        spawn_result: io::Result<i32>,
        spawned: Option<SpawnCall>,
        fail_at: Option<&'static str>, // "mount" | "read" | "enter"
    }

    impl FakeOps {
        fn spawning(code: i32) -> Self {
            FakeOps {
                steps: Vec::new(),
                spec: sample_spec(),
                spawn_result: Ok(code),
                spawned: None,
                fail_at: None,
            }
        }

        fn spawn_failing() -> Self {
            FakeOps {
                steps: Vec::new(),
                spec: sample_spec(),
                spawn_result: Err(io::Error::from_raw_os_error(2)), // ENOENT
                spawned: None,
                fail_at: None,
            }
        }

        fn failing_at(step: &'static str) -> Self {
            FakeOps {
                steps: Vec::new(),
                spec: sample_spec(),
                spawn_result: Ok(0),
                spawned: None,
                fail_at: Some(step),
            }
        }

        fn maybe_fail(&mut self, step: &'static str) -> io::Result<()> {
            self.steps.push(step);
            if self.fail_at == Some(step) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{step} denied"),
                ));
            }
            Ok(())
        }
    }

    impl GuestOps for FakeOps {
        fn mount_rootfs(&mut self) -> io::Result<()> {
            self.maybe_fail("mount")
        }

        fn read_spec(&mut self) -> io::Result<InitSpec> {
            self.maybe_fail("read")?;
            Ok(self.spec.clone())
        }

        fn enter_rootfs(&mut self) -> io::Result<()> {
            self.maybe_fail("enter")
        }

        fn spawn_wait(
            &mut self,
            cmd: &[String],
            cwd: &str,
            env: &[(String, String)],
        ) -> io::Result<i32> {
            self.steps.push("spawn");
            self.spawned = Some((cmd.to_vec(), cwd.to_string(), env.to_vec()));
            match &self.spawn_result {
                Ok(code) => Ok(*code),
                Err(e) => Err(io::Error::from(e.kind())),
            }
        }
    }

    /// Captures every reported exit code so tests can prove EXACT propagation.
    #[derive(Default)]
    struct VecSink {
        reports: Vec<i32>,
    }

    impl ExitSink for VecSink {
        fn report(&mut self, code: i32) -> io::Result<()> {
            self.reports.push(code);
            Ok(())
        }
    }

    // ── run_init: the happy path proves the code is REAL ───────────────────

    #[test]
    fn run_init_runs_lifecycle_in_order_and_reports_exact_code() {
        let mut ops = FakeOps::spawning(42);
        let mut sink = VecSink::default();

        let rc = run_init(&mut ops, &mut sink).expect("ok");

        // (a) lifecycle order: mount → read → enter → spawn.
        assert_eq!(
            ops.steps,
            vec!["mount", "read", "enter", "spawn"],
            "fixed lifecycle order"
        );
        // (b) spawns with the spec's cmd / cwd / env.
        let (cmd, cwd, env) = ops.spawned.expect("command was spawned");
        assert_eq!(cmd, sample_spec().command);
        assert_eq!(cwd, sample_spec().cwd);
        assert_eq!(env, sample_spec().env);
        // (c) sink receives EXACTLY the spawn's exit code — not a hardcoded 0.
        assert_eq!(sink.reports, vec![42], "sink got the real exit code");
        assert_eq!(rc, 42);
    }

    #[test]
    fn run_init_propagates_a_nonzero_code_unchanged() {
        let mut ops = FakeOps::spawning(7);
        let mut sink = VecSink::default();
        let rc = run_init(&mut ops, &mut sink).expect("ok");
        assert_eq!(rc, 7);
        assert_eq!(sink.reports, vec![7]);
    }

    // ── spawn failure ⇒ 127, reported (a real outcome, not an Err) ─────────

    #[test]
    fn run_init_reports_127_on_spawn_failure() {
        let mut ops = FakeOps::spawn_failing();
        let mut sink = VecSink::default();

        let rc = run_init(&mut ops, &mut sink).expect("spawn failure is a real outcome");

        assert_eq!(rc, SPAWN_FAILED_CODE, "command-not-found ⇒ 127");
        assert_eq!(sink.reports, vec![SPAWN_FAILED_CODE], "127 is reported");
        assert_eq!(ops.steps, vec!["mount", "read", "enter", "spawn"]);
    }

    // ── mount / read / enter failure ⇒ Err, NOTHING reported ───────────────

    #[test]
    fn run_init_errs_on_mount_failure_and_reports_nothing() {
        let mut ops = FakeOps::failing_at("mount");
        let mut sink = VecSink::default();

        let err = run_init(&mut ops, &mut sink).expect_err("mount failure propagates");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        assert!(sink.reports.is_empty(), "no fake code on mount failure");
        assert_eq!(ops.steps, vec!["mount"], "stopped at mount");
        assert!(ops.spawned.is_none());
    }

    #[test]
    fn run_init_errs_on_spec_read_failure_and_reports_nothing() {
        let mut ops = FakeOps::failing_at("read");
        let mut sink = VecSink::default();

        let err = run_init(&mut ops, &mut sink).expect_err("read failure propagates");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        assert!(sink.reports.is_empty(), "no fake code on spec-read failure");
        assert_eq!(ops.steps, vec!["mount", "read"], "stopped at read");
        assert!(ops.spawned.is_none());
    }

    #[test]
    fn run_init_errs_on_enter_rootfs_failure_and_reports_nothing() {
        let mut ops = FakeOps::failing_at("enter");
        let mut sink = VecSink::default();

        let err = run_init(&mut ops, &mut sink).expect_err("enter failure propagates");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        assert!(
            sink.reports.is_empty(),
            "no fake code on enter-rootfs failure"
        );
        assert_eq!(
            ops.steps,
            vec!["mount", "read", "enter"],
            "stopped at enter"
        );
        assert!(ops.spawned.is_none(), "never spawned after enter failure");
    }
}
