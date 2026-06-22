//! Conformance-vector RUNNER (executor, second half) — TRANSCRIBED from
//! `lightr-cri/crates/lightr-cri-vectors/src/lib.rs` @ seam-contract-v1.1.
//!
//! Split from `runner.rs` only to honor the 400-LOC godfile guard; together the
//! two files are the faithful single `execute_step` the lightr-cri runner uses
//! to drive a Vector against `&dyn CriBackend`. Image/sandbox/stream/log/reopen
//! step bodies live here; container/exec step bodies live in `runner.rs`.

use std::io::Read as _;

use lightr_cri_backend::{ContainerId, CriBackend, SandboxId};

use crate::runner::{
    check_err_expectation, log_full_path, match_err, step_container_lifecycle, subst,
    validate_cri_log_line, variant_name, BackendFactory, Step, StepOutcome,
};

/// Dispatch one step. Container/exec steps are handled by `runner.rs`; the rest
/// (sandbox, image, streaming, log, reopen) are handled here. TRANSCRIBED.
pub fn execute_step(
    backend: &mut Box<dyn CriBackend>,
    factory: &dyn BackendFactory,
    step: &Step,
    results: &[Option<String>],
) -> StepOutcome {
    if let Some(outcome) = step_container_lifecycle(backend, step, results) {
        return outcome;
    }
    match step {
        // ── sandbox plane ──────────────────────────────────────────────────
        Step::RunSandbox { cfg, expect_err } => {
            let result = backend.run_sandbox(cfg.clone());
            check_err_expectation(result, expect_err, "run_sandbox", |id| Some(id.0))
        }
        Step::StopSandbox { id, expect_err } => {
            let sid = SandboxId(subst(id, results));
            let result = backend.stop_sandbox(&sid);
            check_err_expectation(result, expect_err, "stop_sandbox", |_| None)
        }
        Step::RemoveSandbox { id, expect_err } => {
            let sid = SandboxId(subst(id, results));
            let result = backend.remove_sandbox(&sid);
            check_err_expectation(result, expect_err, "remove_sandbox", |_| None)
        }
        Step::SandboxStatus {
            id,
            expect_state,
            expect_err,
        } => {
            let sid = SandboxId(subst(id, results));
            match backend.sandbox_status(&sid) {
                Ok(status) => {
                    if let Some(expected) = expect_err {
                        return StepOutcome::Fail(format!(
                            "sandbox_status: expected error '{expected}' but call succeeded"
                        ));
                    }
                    if let Some(expected) = expect_state {
                        if status.state != *expected {
                            return StepOutcome::Fail(format!(
                                "sandbox_status: expected state {:?}, got {:?}",
                                expected, status.state
                            ));
                        }
                    }
                    StepOutcome::Ok(None)
                }
                Err(e) => match_err(&e, expect_err, "sandbox_status"),
            }
        }
        Step::SandboxStatusIp {
            id,
            expect_ip_present,
            expect_err,
        } => {
            let sid = SandboxId(subst(id, results));
            match backend.sandbox_status(&sid) {
                Ok(status) => {
                    if let Some(expected) = expect_err {
                        return StepOutcome::Fail(format!(
                            "sandbox_status_ip: expected error '{expected}' but call succeeded"
                        ));
                    }
                    let ip_present = status.ip.is_some();
                    if ip_present != *expect_ip_present {
                        return StepOutcome::Fail(format!(
                            "sandbox_status_ip: expected ip_present={}, got ip_present={} (ip={:?})",
                            expect_ip_present, ip_present, status.ip
                        ));
                    }
                    StepOutcome::Ok(None)
                }
                Err(e) => match_err(&e, expect_err, "sandbox_status_ip"),
            }
        }

        // ── exec (sync) ────────────────────────────────────────────────────
        Step::ExecSync {
            id,
            cmd,
            expect_exit_code,
            expect_stdout,
            expect_err,
        } => {
            let cid = ContainerId(subst(id, results));
            match backend.exec_sync(&cid, cmd, 30) {
                Ok(r) => {
                    if let Some(expected) = expect_err {
                        return StepOutcome::Fail(format!(
                            "exec_sync: expected error '{expected}' but call succeeded"
                        ));
                    }
                    if let Some(code) = expect_exit_code {
                        if r.exit_code != *code {
                            return StepOutcome::Fail(format!(
                                "exec_sync: expected exit_code {}, got {}",
                                code, r.exit_code
                            ));
                        }
                    }
                    if let Some(expected_out) = expect_stdout {
                        let actual = String::from_utf8_lossy(&r.stdout).into_owned();
                        if actual.trim_end_matches('\n') != expected_out.trim_end_matches('\n') {
                            return StepOutcome::Fail(format!(
                                "exec_sync: expected stdout {expected_out:?}, got {actual:?}"
                            ));
                        }
                    }
                    StepOutcome::Ok(None)
                }
                Err(e) => match_err(&e, expect_err, "exec_sync"),
            }
        }

        // ── image plane ────────────────────────────────────────────────────
        Step::PullImage {
            image_ref,
            store_as_result,
            expect_err,
        } => {
            let result = backend.pull_image(image_ref);
            check_err_expectation(result, expect_err, "pull_image", |pulled| {
                if *store_as_result {
                    Some(pulled.root_hex)
                } else {
                    None
                }
            })
        }
        Step::ImageStatus {
            image_ref,
            expect_present,
            expect_err,
        } => match backend.image_status(image_ref) {
            Ok(maybe) => {
                if let Some(expected) = expect_err {
                    return StepOutcome::Fail(format!(
                        "image_status: expected error '{expected}' but call succeeded"
                    ));
                }
                if let Some(expected) = expect_present {
                    if maybe.is_some() != *expected {
                        return StepOutcome::Fail(format!(
                            "image_status: expected present={}, got present={}",
                            expected,
                            maybe.is_some()
                        ));
                    }
                }
                StepOutcome::Ok(None)
            }
            Err(e) => match_err(&e, expect_err, "image_status"),
        },
        Step::ListImages {
            expect_count,
            expect_err,
        } => match backend.list_images() {
            Ok(images) => {
                if let Some(expected) = expect_err {
                    return StepOutcome::Fail(format!(
                        "list_images: expected error '{expected}' but call succeeded"
                    ));
                }
                if let Some(count) = expect_count {
                    if images.len() != *count {
                        return StepOutcome::Fail(format!(
                            "list_images: expected {} images, got {}",
                            count,
                            images.len()
                        ));
                    }
                }
                StepOutcome::Ok(None)
            }
            Err(e) => match_err(&e, expect_err, "list_images"),
        },
        Step::RemoveImage {
            image_ref,
            expect_err,
        } => {
            let result = backend.remove_image(image_ref);
            check_err_expectation(result, expect_err, "remove_image", |_| None)
        }

        // ── crash-recovery ─────────────────────────────────────────────────
        Step::ReopenBackend {} => {
            *backend = factory.reopen();
            StepOutcome::Ok(None)
        }

        // ── v1.1 streaming + log ───────────────────────────────────────────
        Step::OpenExec {
            id,
            cmd,
            tty,
            stdin,
            expect_exit_code,
            expect_stdout_contains,
            expect_err,
        } => step_open_exec(
            backend,
            id,
            cmd,
            *tty,
            *stdin,
            expect_exit_code,
            expect_stdout_contains,
            expect_err,
            results,
        ),
        Step::AssertLogExists {
            sandbox_id,
            container_id,
            expect_err,
        } => step_assert_log(
            backend,
            sandbox_id,
            container_id,
            expect_err,
            results,
            false,
        ),
        Step::AssertLogFormat {
            sandbox_id,
            container_id,
            expect_err,
        } => step_assert_log(backend, sandbox_id, container_id, expect_err, results, true),

        // The container/exec-lifecycle steps are handled by the early return
        // from `step_container_lifecycle` above; they never reach this match.
        Step::CreateContainer { .. }
        | Step::StartContainer { .. }
        | Step::StopContainer { .. }
        | Step::RemoveContainer { .. }
        | Step::AssertStatus { .. }
        | Step::WaitExited { .. } => {
            unreachable!("container/exec-lifecycle step handled in step_container_lifecycle")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn step_open_exec(
    backend: &mut Box<dyn CriBackend>,
    id: &str,
    cmd: &[String],
    tty: bool,
    stdin: bool,
    expect_exit_code: &Option<i32>,
    expect_stdout_contains: &Option<String>,
    expect_err: &Option<String>,
    results: &[Option<String>],
) -> StepOutcome {
    let cid = ContainerId(subst(id, results));
    match backend.open_exec(&cid, cmd, tty, stdin) {
        Err(e) => match_err(&e, expect_err, "open_exec"),
        Ok(mut session) => {
            if let Some(expected) = expect_err {
                return StepOutcome::Fail(format!(
                    "open_exec: expected error '{expected}' but call succeeded"
                ));
            }
            drop(session.stdin.take());
            let stdout_bytes = if let Some(mut f) = session.stdout.take() {
                let mut buf = Vec::new();
                if let Err(e) = f.read_to_end(&mut buf) {
                    return StepOutcome::Fail(format!("open_exec: read stdout: {e}"));
                }
                buf
            } else {
                Vec::new()
            };
            drop(session.stderr.take());
            drop(session.pty_master.take());
            let exit_code = match session.waiter.wait() {
                Ok(code) => code,
                Err(e) => return StepOutcome::Fail(format!("open_exec: waiter.wait(): {e}")),
            };
            if let Some(code) = expect_exit_code {
                if exit_code != *code {
                    return StepOutcome::Fail(format!(
                        "open_exec: expected exit_code {code}, got {exit_code}"
                    ));
                }
            }
            if let Some(needle) = expect_stdout_contains {
                let s = String::from_utf8_lossy(&stdout_bytes);
                if !s.contains(needle.as_str()) {
                    return StepOutcome::Fail(format!(
                        "open_exec: stdout {s:?} does not contain {needle:?}"
                    ));
                }
            }
            StepOutcome::Ok(None)
        }
    }
}

fn step_assert_log(
    backend: &mut Box<dyn CriBackend>,
    sandbox_id: &str,
    container_id: &str,
    expect_err: &Option<String>,
    results: &[Option<String>],
    check_format: bool,
) -> StepOutcome {
    let name = if check_format {
        "assert_log_format"
    } else {
        "assert_log_exists"
    };
    let sid = SandboxId(subst(sandbox_id, results));
    let cid = ContainerId(subst(container_id, results));

    let sandbox_status = match backend.sandbox_status(&sid) {
        Ok(s) => s,
        Err(e) => return log_lookup_err(&e, expect_err, name, "sandbox_status"),
    };
    let container_status = match backend.container_status(&cid) {
        Ok(s) => s,
        Err(e) => return log_lookup_err(&e, expect_err, name, "container_status"),
    };
    if expect_err.is_some() {
        return StepOutcome::Fail(format!("{name}: expected error but lookups succeeded"));
    }
    let log_dir = &sandbox_status.config.log_directory;
    let log_path = &container_status.config.log_path;
    if log_dir.is_empty() || log_path.is_empty() {
        return StepOutcome::Fail(format!(
            "{name}: log_directory={log_dir:?} or log_path={log_path:?} is empty"
        ));
    }
    let full = log_full_path(log_dir, log_path);
    if !full.exists() {
        return StepOutcome::Fail(format!("{name}: log file {full:?} does not exist"));
    }
    if check_format {
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(e) => return StepOutcome::Fail(format!("{name}: read {full:?}: {e}")),
        };
        for (line_no, line) in content.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            if let Err(msg) = validate_cri_log_line(line) {
                return StepOutcome::Fail(format!("{name}: line {}: {msg}", line_no + 1));
            }
        }
    }
    StepOutcome::Ok(None)
}

fn log_lookup_err(
    e: &lightr_cri_backend::BackendError,
    expect_err: &Option<String>,
    name: &str,
    which: &str,
) -> StepOutcome {
    if let Some(expected) = expect_err {
        let actual = variant_name(e);
        if actual == expected.as_str() {
            return StepOutcome::Ok(None);
        }
        return StepOutcome::Fail(format!(
            "{name}: {which} error: expected '{expected}', got '{actual}': {e}"
        ));
    }
    StepOutcome::Fail(format!("{name}: {which} error: {e}"))
}
