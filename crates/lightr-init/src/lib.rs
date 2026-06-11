//! lightr-init — the Linux guest's PID 1 (build-spec-prod.md §WP-B-init).
//!
//! The LIBRARY is host-portable and fully unit-tested: the init lifecycle is
//! parameterized over `GuestOps` (mount/spawn) and `ExitSink` (exit-code
//! report), so it runs on Intel/macOS today. The real Linux syscalls + vsock
//! live in `bin/init.rs` behind `#[cfg(target_os = "linux")]`.
//!
//! This replaces the placeholder `exitCode = 0` in the vz shim: the guest
//! process's REAL exit code flows PID1 → ExitSink → host. The lifecycle never
//! synthesizes a success code — `sink.report()` always receives the actual
//! `spawn_wait` result (or 127 when the command cannot be spawned).

use serde::{Deserialize, Serialize};

/// Pre-decided mount targets (build-spec-prod.md §WP-B-init). PID1 mounts the
/// store first-class share at [`STORE_DEST`] and the rootfs at [`ROOTFS_DEST`];
/// the binary later `pivot_root`s into [`ROOTFS_DEST`] (not run_init's job).
pub const ROOTFS_TAG: &str = "rootfs";
/// Mount target for the rootfs virtiofs share.
pub const ROOTFS_DEST: &str = "/newroot";
/// virtiofs tag for the store share.
pub const STORE_TAG: &str = "store";
/// Mount target for the store virtiofs share.
pub const STORE_DEST: &str = "/lightr/store";

/// Exit code reported when the command cannot be spawned (ENOENT etc.). Matches
/// the shell "command not found" convention so the host sees a real, non-zero
/// outcome rather than a fabricated success.
pub const SPAWN_FAILED_CODE: i32 = 127;

/// What PID1 must do, as data — written by the host, read from a mounted spec.
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

/// Where PID1 reports the guest process exit code. Seam: tests use a Vec,
/// the real guest writes a vsock frame to the host.
pub trait ExitSink {
    fn report(&mut self, code: i32) -> std::io::Result<()>;
}

/// OS actions PID1 performs, seamed for host-side testing.
pub trait GuestOps {
    /// Mount a virtiofs share `tag` at `dest` (rootfs, store).
    fn mount_virtiofs(&mut self, tag: &str, dest: &str) -> std::io::Result<()>;
    /// Spawn the command, wait, return its exit code (128+signal on signal).
    fn spawn_wait(
        &mut self,
        cmd: &[String],
        cwd: &str,
        env: &[(String, String)],
    ) -> std::io::Result<i32>;
}

/// The init lifecycle: mount shares → spawn the command → report exit.
///
/// Order is fixed: rootfs share first, then store share, then spawn the
/// command with its cwd+env, capture the exit code, report it through `sink`,
/// and return it.
///
/// Honesty invariant (the whole point of this WP): `sink.report()` is called
/// with the ACTUAL exit code — never a hardcoded success.
/// - A mount failure propagates as `Err` and reports NOTHING (no fake code).
/// - A spawn failure (e.g. ENOENT) is a real outcome: report
///   [`SPAWN_FAILED_CODE`] (127) and return it.
pub fn run_init<M: GuestOps>(
    spec: &InitSpec,
    ops: &mut M,
    sink: &mut dyn ExitSink,
) -> std::io::Result<i32> {
    // 1. Mount the shares in order. A mount failure is unrecoverable for PID1:
    //    propagate the io::Error and report NOTHING — never a fake exit code.
    ops.mount_virtiofs(ROOTFS_TAG, ROOTFS_DEST)?;
    ops.mount_virtiofs(STORE_TAG, STORE_DEST)?;

    // 2. Spawn the command and capture its REAL exit code. A spawn failure
    //    (command not found) is still a real outcome → 127, not an Err.
    let code = match ops.spawn_wait(&spec.command, &spec.cwd, &spec.env) {
        Ok(code) => code,
        Err(_) => SPAWN_FAILED_CODE,
    };

    // 3. Report the actual code, then return it. This is the line that kills
    //    the vz shim's hardcoded exitCode=0.
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
        // Re-serializing the parsed value reproduces the same bytes (canonical).
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

    /// Records mount calls in order and returns a configurable spawn outcome.
    struct FakeOps {
        mounts: Vec<(String, String)>,
        spawn_result: io::Result<i32>,
        spawned: Option<SpawnCall>,
        fail_mount_tag: Option<String>,
    }

    impl FakeOps {
        fn spawning(code: i32) -> Self {
            FakeOps {
                mounts: Vec::new(),
                spawn_result: Ok(code),
                spawned: None,
                fail_mount_tag: None,
            }
        }

        fn spawn_failing() -> Self {
            FakeOps {
                mounts: Vec::new(),
                spawn_result: Err(io::Error::from_raw_os_error(libc_enoent())),
                spawned: None,
                fail_mount_tag: None,
            }
        }

        fn mount_failing(tag: &str) -> Self {
            FakeOps {
                mounts: Vec::new(),
                spawn_result: Ok(0),
                spawned: None,
                fail_mount_tag: Some(tag.to_string()),
            }
        }
    }

    // ENOENT without pulling libc into the lib's host-test deps.
    fn libc_enoent() -> i32 {
        2
    }

    impl GuestOps for FakeOps {
        fn mount_virtiofs(&mut self, tag: &str, dest: &str) -> io::Result<()> {
            if self.fail_mount_tag.as_deref() == Some(tag) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("mount {tag} denied"),
                ));
            }
            self.mounts.push((tag.to_string(), dest.to_string()));
            Ok(())
        }

        fn spawn_wait(
            &mut self,
            cmd: &[String],
            cwd: &str,
            env: &[(String, String)],
        ) -> io::Result<i32> {
            self.spawned = Some((cmd.to_vec(), cwd.to_string(), env.to_vec()));
            // Reproduce the configured outcome (io::Error isn't Clone).
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
    fn run_init_mounts_in_order_spawns_and_reports_exact_code() {
        let spec = sample_spec();
        let mut ops = FakeOps::spawning(42);
        let mut sink = VecSink::default();

        let rc = run_init(&spec, &mut ops, &mut sink).expect("ok");

        // (a) mounts rootfs THEN store, at the pre-decided targets.
        assert_eq!(
            ops.mounts,
            vec![
                (ROOTFS_TAG.to_string(), ROOTFS_DEST.to_string()),
                (STORE_TAG.to_string(), STORE_DEST.to_string()),
            ],
            "rootfs must mount before store, at the frozen targets"
        );

        // (b) spawns with the right cmd / cwd / env.
        let (cmd, cwd, env) = ops.spawned.expect("command was spawned");
        assert_eq!(cmd, spec.command);
        assert_eq!(cwd, spec.cwd);
        assert_eq!(env, spec.env);

        // (c) sink receives EXACTLY the spawn's exit code — not a hardcoded 0.
        assert_eq!(sink.reports, vec![42], "sink got the real exit code");
        // (d) and run_init returns it.
        assert_eq!(rc, 42);
    }

    #[test]
    fn run_init_propagates_a_nonzero_code_unchanged() {
        let spec = sample_spec();
        let mut ops = FakeOps::spawning(3);
        let mut sink = VecSink::default();

        let rc = run_init(&spec, &mut ops, &mut sink).expect("ok");

        assert_eq!(rc, 3);
        assert_eq!(sink.reports, vec![3]);
    }

    // ── spawn failure ⇒ 127, reported (a real outcome, not an Err) ─────────

    #[test]
    fn run_init_reports_127_on_spawn_failure() {
        let spec = sample_spec();
        let mut ops = FakeOps::spawn_failing();
        let mut sink = VecSink::default();

        let rc = run_init(&spec, &mut ops, &mut sink).expect("spawn failure is a real outcome");

        assert_eq!(rc, SPAWN_FAILED_CODE, "command-not-found ⇒ 127");
        assert_eq!(sink.reports, vec![SPAWN_FAILED_CODE], "127 is reported");
        // Mounts still happened before the failed spawn.
        assert_eq!(ops.mounts.len(), 2);
    }

    // ── mount failure ⇒ Err, NOTHING reported (no fake success) ────────────

    #[test]
    fn run_init_errs_on_rootfs_mount_failure_and_reports_nothing() {
        let spec = sample_spec();
        let mut ops = FakeOps::mount_failing(ROOTFS_TAG);
        let mut sink = VecSink::default();

        let err = run_init(&spec, &mut ops, &mut sink).expect_err("mount failure propagates");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        // The KEY honesty invariant: a failed boot reports NO exit code.
        assert!(sink.reports.is_empty(), "no fake code on mount failure");
        // We never reached the store mount or the spawn.
        assert!(ops.mounts.is_empty());
        assert!(ops.spawned.is_none());
    }

    #[test]
    fn run_init_errs_on_store_mount_failure_and_reports_nothing() {
        let spec = sample_spec();
        let mut ops = FakeOps::mount_failing(STORE_TAG);
        let mut sink = VecSink::default();

        let err = run_init(&spec, &mut ops, &mut sink).expect_err("mount failure propagates");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        assert!(sink.reports.is_empty(), "no fake code on mount failure");
        // rootfs mounted, store did not, spawn never ran.
        assert_eq!(
            ops.mounts,
            vec![(ROOTFS_TAG.to_string(), ROOTFS_DEST.to_string())]
        );
        assert!(ops.spawned.is_none());
    }
}
