//! WP-RC-FLAGS — the PER-FIELD runtime-config apply seam (RC-SEAM-FREEZE filled).
//!
//! Each RC carry-field (`RunSpec`/`SpecOnDisk`) has exactly ONE applier here:
//! `apply_<field>(value, &mut Command)`. A run that sets NONE of these fields
//! behaves EXACTLY as before — every applier is a no-op on its default value
//! (`None`/empty/`false`), so the seam is behaviour-preserving.
//!
//! Two thin dispatch entry points fan out to the SAME per-field appliers from
//! both native exec sites — the synchronous memo path (`RunSpec`, `memo.rs`) and
//! the detached supervisor (`SpecOnDisk`, `supervise_native.rs`) — so a field
//! honored here is honored on every native run with no further wiring.
//!
//! ── Honest-boundary law (CLAUDE.md principle 7; mirrors `limits.rs`) ──────────
//! lightr's native engine is a HOST PROCESS, NOT a sandbox (CLAUDE.md principle
//! 4 — `native` = reproducibility, not isolation). Some Docker run-config flags
//! require namespaces / cgroups / a root daemon the native engine deliberately
//! does NOT have. For those, the applier does NOT silently drop the flag: the
//! field PERSISTS (spec.json + `inspect`) and the applier carries an HONEST note
//! (here, in the doc) that the native engine records-but-does-not-enforce it.
//! The `vz` engine (a real microVM) is where these gain hardware teeth.
//!
//! ENFORCED on the native exec:
//!   * `hostname`  — best-effort: exported as `$HOSTNAME` to the child (the var
//!     programs read; Docker also sets it). No UTS namespace, so `uname -n` is
//!     unchanged — but the conventional channel IS honored.
//!   * `oom_score_adj` — Linux: a `pre_exec` hook writes the child's own
//!     `/proc/self/oom_score_adj`. A real per-process effect needing no
//!     namespace; a value the kernel forbids (negative w/o CAP_SYS_RESOURCE)
//!     surfaces as an HONEST spawn error, never silently ignored.
//!
//! HONEST-RECORDED (persisted + inspectable, NOT enforced on native — needs a
//! namespace/cgroup the native engine lacks; honored by the `vz` engine):
//!   * `labels` (metadata only — no exec effect by definition),
//!   * `tty` (a real pty would rewrite the streaming/memo stdio plumbing).
//!
//! HONEST-ERRORED upstream (WP-#92 — the run handler refuses the run, exit 2,
//! BEFORE any engine dispatch; recorded-only here so spec.json roundtrips):
//!   * `cap_add` / `cap_drop` / `privileged` (Linux capabilities) — a silent
//!     no-op on a SECURITY flag is worse than an error (the user believes they are
//!     sandboxed and isn't). The rootless ns userns already BOUNDS the cap set;
//!     full capset management is a separate tracked WP, so the handler refuses
//!     rather than give false security. These appliers are pure no-ops.
//!
//! HONEST-STAGED (recorded, documented, NOT silently claimed — WP-#92):
//!   * `init` (PID 1 zombie reaper) — a real pid1 init is entangled with the ns
//!     pid-namespace handling and is NOT yet applied; the handler emits an honest
//!     stderr note when `--init` is set. `apply_init` is a documented no-op.
//!
//! RECORDED-ONLY here, ENFORCED on the `ns` engine:
//!   * `pids_limit` (cgroup `pids.max`, WP-#90) — the native engine honest-ERRORS
//!     on a pids request (`limits::check_native_support`); the real cap is the `ns`
//!     engine's `ResourceLimits.pids_max` → `apply_cgroup`.
//!   * `read_only` (WP-#92) — the `ns` engine remounts the pivoted rootfs RO
//!     (`ExecSpec.read_only`); native has no rootfs to remount.
//!   * `shm_size` (WP-#92) — the `ns` engine sizes a `/dev/shm` tmpfs
//!     (`ExecSpec.shm_size`); native has no mount ns to own one.
//!
//! All three stay recorded + inspectable (spec.json / `inspect`) but enforce
//! NOTHING on native (see `apply_pids_limit` / `apply_read_only` / `apply_shm_size`).
//!
//! Cross-platform (template 8a): the unix-only appliers are `#[cfg(unix)]` on the
//! fn ITSELF and their dispatch calls are `#[cfg(unix)]`-gated, so none is dead
//! code on the windows clippy gate. `hostname`/`labels` are platform-neutral.

use super::types::{RunSpec, SpecOnDisk};
use std::process::Command;

// ── Dispatch entry points ───────────────────────────────────────────────────

/// Apply the RC carry-fields of a `RunSpec` (synchronous native memo path) to
/// the child `Command`. A default-field run is a no-op (behaviour-preserving).
pub(super) fn apply_run_config_spec(spec: &RunSpec, cmd: &mut Command) {
    apply_hostname(spec.hostname.as_deref(), cmd);
    apply_labels(&spec.labels, cmd);
    #[cfg(unix)]
    {
        apply_cap_add(&spec.cap_add, cmd);
        apply_cap_drop(&spec.cap_drop, cmd);
        apply_privileged(spec.privileged, cmd);
        apply_tty(spec.tty, cmd);
        apply_init(spec.init, cmd);
        apply_read_only(spec.read_only, cmd);
        apply_oom_score_adj(spec.oom_score_adj, cmd);
        apply_pids_limit(spec.pids_limit, cmd);
        apply_shm_size(spec.shm_size, cmd);
    }
}

/// Apply the RC carry-fields of a `SpecOnDisk` (detached supervisor path) to the
/// child `Command`. Same per-field appliers as the `RunSpec` entry point — the
/// supervisor reads the persisted spec and honors the identical config.
pub(super) fn apply_run_config_ondisk(spec: &SpecOnDisk, cmd: &mut Command) {
    apply_hostname(spec.hostname.as_deref(), cmd);
    apply_labels(&spec.labels, cmd);
    #[cfg(unix)]
    {
        apply_cap_add(&spec.cap_add, cmd);
        apply_cap_drop(&spec.cap_drop, cmd);
        apply_privileged(spec.privileged, cmd);
        apply_tty(spec.tty, cmd);
        apply_init(spec.init, cmd);
        apply_read_only(spec.read_only, cmd);
        apply_oom_score_adj(spec.oom_score_adj, cmd);
        apply_pids_limit(spec.pids_limit, cmd);
        apply_shm_size(spec.shm_size, cmd);
    }
}

// ── Per-field appliers (one slot each) ──────────────────────────────────────

/// `--hostname`. ENFORCED (best-effort): export `$HOSTNAME` to the child — the
/// conventional channel programs read (Docker sets it too). `None` ⇒ no-op (the
/// child inherits the host's `$HOSTNAME`, byte-identical to before). The native
/// engine has no UTS namespace, so `uname -n` is unchanged; the env channel is
/// the faithful-as-feasible honoring. Platform-neutral.
fn apply_hostname(hostname: Option<&str>, cmd: &mut Command) {
    if let Some(h) = hostname {
        cmd.env("HOSTNAME", h);
    }
}

/// `--label`/`-l`. HONEST-RECORDED: labels are run metadata with NO exec effect
/// by definition — they are persisted to spec.json and surfaced by `inspect`
/// (Docker's `Config.Labels`). No `Command` mutation. Always a no-op here.
fn apply_labels(labels: &[(String, String)], cmd: &mut Command) {
    // Metadata only — recorded in spec.json + shown by inspect, never an exec
    // effect. Consume the args so the seam stays uniform (no-op on the Command).
    let _ = (labels, cmd);
}

/// `--cap-add`. HONEST-ERRORED upstream (WP-#92): the run handler refuses the run
/// (exit 2) whenever `--cap-add`/`--cap-drop` is set — capability enforcement is
/// not yet wired, and a silently-dropped capability flag would give FALSE
/// security. This carry-field is recorded-only (spec.json roundtrip); the applier
/// is a pure no-op (the handler never lets a cap request reach exec). Empty ⇒ no-op.
#[cfg(unix)]
fn apply_cap_add(cap_add: &[String], cmd: &mut Command) {
    let _ = (cap_add, cmd);
}

/// `--cap-drop`. HONEST-ERRORED upstream (WP-#92) — see [`apply_cap_add`].
#[cfg(unix)]
fn apply_cap_drop(cap_drop: &[String], cmd: &mut Command) {
    let _ = (cap_drop, cmd);
}

/// `--privileged`. HONEST-ERRORED upstream (WP-#92): the run handler refuses the
/// run (exit 2) when `--privileged` is set — an unprivileged user namespace grants
/// no real privilege, so accepting the flag would be a false-security lie; tracked
/// for `--engine vz`. Recorded-only here (spec.json roundtrip); pure no-op applier.
/// `false` ⇒ no-op.
#[cfg(unix)]
fn apply_privileged(privileged: bool, cmd: &mut Command) {
    let _ = (privileged, cmd);
}

/// `-t`/`--tty`. HONEST-RECORDED. Allocating a real pty would rewrite the
/// streaming + memo stdio plumbing (the memoized path captures stdout/stderr as
/// bytes); a pty is out of the native engine's scope. Recorded + inspectable.
/// `false` ⇒ no-op.
#[cfg(unix)]
fn apply_tty(tty: bool, cmd: &mut Command) {
    let _ = (tty, cmd);
}

/// `--init`. HONEST-STAGED (WP-#92): recorded only — a pid1 init reaper is
/// entangled with the ns engine's pid-namespace handling and is NOT yet applied on
/// ANY engine. The run handler emits an honest stderr note when `--init` is set so
/// the flag is never a silent false claim; this applier stays a documented no-op
/// until the pid1 reaper lands. `false` ⇒ no-op.
#[cfg(unix)]
fn apply_init(init: bool, cmd: &mut Command) {
    let _ = (init, cmd);
}

/// `--read-only`. RECORDED-ONLY on the native supervisor (WP-#92 — the prior
/// "HONEST-RECORDED" comment implied enforcement this slot never had). A read-only
/// root needs a mount namespace the native host process does not own. ENFORCEMENT
/// lives on the `ns` engine via `ExecSpec.read_only` → a non-recursive
/// `MS_BIND|MS_REMOUNT|MS_RDONLY` remount of the pivoted rootfs (rootfs immutable,
/// /dev + /dev/shm writable), fail-closed. This carry-field is recorded +
/// inspectable (spec.json / `inspect`) only, never enforced here. `false` ⇒ no-op.
#[cfg(unix)]
fn apply_read_only(read_only: bool, cmd: &mut Command) {
    let _ = (read_only, cmd);
}

/// `--oom-score-adj`. ENFORCED on Linux: a `pre_exec` hook writes the child's own
/// `/proc/self/oom_score_adj` (a real per-process effect needing no namespace).
/// A value the kernel forbids (e.g. negative without `CAP_SYS_RESOURCE`) surfaces
/// as an HONEST spawn error, never a silent drop. Off Linux (e.g. macOS) there is
/// no `/proc` mechanism ⇒ recorded + inspectable, not enforced. `None` ⇒ no-op.
#[cfg(unix)]
fn apply_oom_score_adj(oom_score_adj: Option<i32>, cmd: &mut Command) {
    let adj = match oom_score_adj {
        None => return,
        Some(a) => a,
    };
    #[cfg(target_os = "linux")]
    {
        install_oom_score_adj(cmd, adj);
    }
    #[cfg(not(target_os = "linux"))]
    {
        // No /proc/self/oom_score_adj off Linux — recorded (spec.json) but not
        // enforced. Consume the binding so non-Linux unix builds see no unused var.
        let _ = (adj, cmd);
    }
}

/// Install a `pre_exec` hook that writes `value` to the child's own
/// `/proc/self/oom_score_adj` AFTER fork, BEFORE exec — so it sets the CHILD's
/// score, not the parent's.
///
/// Safety: the closure runs in the forked child before `execvp`. It uses only
/// the async-signal-safe `open`/`write`/`close` libc calls (no allocation, no
/// shared locks), captures a single `i32` by copy, and formats the value into a
/// fixed stack buffer with no heap use.
#[cfg(target_os = "linux")]
fn install_oom_score_adj(cmd: &mut std::process::Command, value: i32) {
    use std::os::unix::process::CommandExt;

    // SAFETY: see the doc comment — allocation-free, async-signal-safe syscalls.
    unsafe {
        cmd.pre_exec(move || {
            // Format `value` into a fixed stack buffer (no heap): an i32 fits in
            // 11 bytes ("-2147483648"); the procfs file accepts the bare integer.
            let mut buf = [0u8; 12];
            let s = i32_to_bytes(value, &mut buf);

            let path = c"/proc/self/oom_score_adj";
            let fd = libc::open(path.as_ptr(), libc::O_WRONLY);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let n = libc::write(fd, s.as_ptr() as *const libc::c_void, s.len());
            let werr = if n < 0 {
                Some(std::io::Error::last_os_error())
            } else {
                None
            };
            libc::close(fd);
            if let Some(e) = werr {
                return Err(e);
            }
            Ok(())
        });
    }
}

/// Write the decimal text of `value` into `buf` and return the filled slice.
/// Allocation-free + async-signal-safe (only stack writes) so it is callable
/// from a `pre_exec` hook. Handles `i32::MIN` correctly via `i64` widening.
#[cfg(target_os = "linux")]
fn i32_to_bytes(value: i32, buf: &mut [u8; 12]) -> &[u8] {
    let neg = value < 0;
    let mut v = (value as i64).unsigned_abs();
    // Fill from the end with digits.
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    if neg {
        i -= 1;
        buf[i] = b'-';
    }
    &buf[i..]
}

/// `--pids-limit`. RECORDED-ONLY on the native supervisor (WP-#90 — the prior
/// "HONEST-RECORDED (cgroup pids.max)" comment LIED: this slot enforces nothing).
/// Capping the pid count needs cgroup v2 `pids.max`, which the native host process
/// cannot create. ENFORCEMENT lives on the `ns` engine via
/// `ResourceLimits.pids_max` → `lightr_engine::limits::apply_cgroup` (a real
/// `pids.max` write in a transient cgroup). The native engine HONEST-ERRORS on a
/// pids request upstream (`check_native_support`), and vz honest-errors at the CLI,
/// so a pids cap is never silently dropped — this carry-field is recorded +
/// inspectable (spec.json / `inspect`) only, never enforced here. Intentional
/// no-op; `RunSpec`/`SpecOnDisk.pids_limit` remain as recorded fields (memo-key +
/// spec.json roundtrip tests depend on them). `None` ⇒ no-op.
#[cfg(unix)]
fn apply_pids_limit(pids_limit: Option<i64>, cmd: &mut Command) {
    let _ = (pids_limit, cmd);
}

/// `--shm-size`. RECORDED-ONLY on the native supervisor (WP-#92 — the prior
/// "HONEST-RECORDED" comment implied enforcement this slot never had). Sizing
/// `/dev/shm` needs a mount namespace the native host process does not own.
/// ENFORCEMENT lives on the `ns` engine via `ExecSpec.shm_size` → a `tmpfs` mount
/// at `/dev/shm` sized to the requested bytes (fail-closed for an explicit size;
/// a default 64 MiB mount otherwise). Recorded + inspectable only, never enforced
/// here. `None` ⇒ no-op.
#[cfg(unix)]
fn apply_shm_size(shm_size: Option<u64>, cmd: &mut Command) {
    let _ = (shm_size, cmd);
}

#[cfg(test)]
#[path = "apply_cfg_tests.rs"]
mod tests;
