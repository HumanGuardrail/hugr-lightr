//! Conformance-vector RUNNER — TRANSCRIBED from
//! `lightr-cri/crates/lightr-cri-vectors/src/lib.rs` @ seam-contract-v1.1
//! (the Vector/Step JSON shape + the `&dyn CriBackend` executor + the
//! `BackendFactory` fresh()/reopen() crash-recovery contract).
//!
//! WIRE-LEVEL SEAM PROOF, NOT a git/path dep (ADR-0017 decision 3). Drift
//! between this transcription and the lightr-cri runner is caught when a frozen
//! vector stops parsing or stops passing — never by a crate import. The
//! executor is split across `runner.rs`/`runner2.rs` only to honor the 400-LOC
//! godfile guard; the two together are the faithful single executor.

use std::path::Path;
use std::time::{Duration, Instant};

use lightr_cri_backend::{BackendError, ContainerId, ContainerState, CriBackend, SandboxId};

pub use crate::step::{Step, Vector};

/// Factory so each vector runs ISOLATED and crash-recovery vectors can drop and
/// reopen the same state (`reopen_backend` step). TRANSCRIBED from the runner's
/// `BackendFactory`: `fresh()` = new isolated state; `reopen()` = re-derive the
/// most-recent `fresh()`'s state from disk (crash-only law).
pub trait BackendFactory {
    fn fresh(&self) -> Box<dyn CriBackend>;
    fn reopen(&self) -> Box<dyn CriBackend>;
}

// ── CRI log format validation (transcribed) ──────────────────────────────────

/// Validate a single CRI log line: `<RFC3339Nano> <stdout|stderr> <F|P> <data>`.
pub fn validate_cri_log_line(line: &str) -> Result<(), String> {
    let mut parts = line.splitn(4, char::is_whitespace);
    let timestamp = parts.next().unwrap_or("");
    let stream = parts.next().unwrap_or("");
    let tag = parts.next().unwrap_or("");
    if timestamp.is_empty() {
        return Err(format!("missing timestamp in line: {line:?}"));
    }
    if !timestamp.contains('T') {
        return Err(format!(
            "timestamp {timestamp:?} does not look like RFC3339 (missing 'T')"
        ));
    }
    let ends_ok = timestamp.ends_with('Z')
        || timestamp.ends_with('z')
        || timestamp
            .chars()
            .rev()
            .nth(5)
            .map(|c| c == '+' || c == '-')
            .unwrap_or(false)
        || timestamp
            .chars()
            .rev()
            .nth(2)
            .map(|c| c == '+' || c == '-')
            .unwrap_or(false);
    if !ends_ok {
        return Err(format!(
            "timestamp {timestamp:?} does not end with 'Z' or UTC offset"
        ));
    }
    if stream != "stdout" && stream != "stderr" {
        return Err(format!("stream {stream:?} must be 'stdout' or 'stderr'"));
    }
    if tag != "F" && tag != "P" {
        return Err(format!("tag {tag:?} must be 'F' or 'P'"));
    }
    Ok(())
}

// ── $N substitution + variant-name matching (transcribed) ────────────────────

pub fn subst(s: &str, results: &[Option<String>]) -> String {
    if let Some(rest) = s.strip_prefix('$') {
        if let Ok(idx) = rest.parse::<usize>() {
            if let Some(Some(val)) = results.get(idx) {
                return val.clone();
            }
        }
    }
    s.to_string()
}

pub fn variant_name(e: &BackendError) -> &'static str {
    match e {
        BackendError::NotFound(_) => "NotFound",
        BackendError::AlreadyExists(_) => "AlreadyExists",
        BackendError::InvalidArgument(_) => "InvalidArgument",
        BackendError::FailedPrecondition(_) => "FailedPrecondition",
        BackendError::InUse(_) => "InUse",
        BackendError::Internal(_) => "Internal",
        BackendError::Io(_) => "Io",
    }
}

pub enum StepOutcome {
    Ok(Option<String>),
    Fail(String),
}

/// Check `expect_err`; return a StepOutcome (transcribed helper).
pub fn check_err_expectation<T>(
    result: lightr_cri_backend::Result<T>,
    expect_err: &Option<String>,
    step_name: &str,
    value_extractor: impl FnOnce(T) -> Option<String>,
) -> StepOutcome {
    match (result, expect_err) {
        (Ok(val), None) => StepOutcome::Ok(value_extractor(val)),
        (Ok(_), Some(expected)) => StepOutcome::Fail(format!(
            "{step_name}: expected error '{expected}' but call succeeded"
        )),
        (Err(e), None) => StepOutcome::Fail(format!("{step_name}: unexpected error: {e}")),
        (Err(e), Some(expected)) => {
            let actual = variant_name(&e);
            if actual == expected.as_str() {
                StepOutcome::Ok(None)
            } else {
                StepOutcome::Fail(format!(
                    "{step_name}: expected error '{expected}', got '{actual}': {e}"
                ))
            }
        }
    }
}

// ── Single vector execution (transcribed) ────────────────────────────────────

/// Run one vector. Ok(()) on pass, Err(message) on first failure.
pub fn run_vector(factory: &dyn BackendFactory, vector: &Vector) -> Result<(), String> {
    let mut backend: Box<dyn CriBackend> = factory.fresh();
    let mut results: Vec<Option<String>> = Vec::new();
    for (step_idx, step) in vector.steps.iter().enumerate() {
        match crate::runner2::execute_step(&mut backend, factory, step, &results) {
            StepOutcome::Ok(val) => results.push(val),
            StepOutcome::Fail(msg) => {
                return Err(format!(
                    "vector '{}' step {}: {}",
                    vector.name, step_idx, msg
                ));
            }
        }
    }
    Ok(())
}

// ── Container/exec step handlers (image/sandbox/stream live in runner2) ───────

pub fn step_container_lifecycle(
    backend: &mut Box<dyn CriBackend>,
    step: &Step,
    results: &[Option<String>],
) -> Option<StepOutcome> {
    Some(match step {
        Step::CreateContainer {
            sandbox,
            cfg,
            expect_err,
        } => {
            let sid = SandboxId(subst(sandbox, results));
            let result = backend.create_container(&sid, cfg.clone());
            check_err_expectation(result, expect_err, "create_container", |id| Some(id.0))
        }
        Step::StartContainer { id, expect_err } => {
            let cid = ContainerId(subst(id, results));
            let result = backend.start_container(&cid);
            check_err_expectation(result, expect_err, "start_container", |_| None)
        }
        Step::StopContainer {
            id,
            grace_seconds,
            expect_err,
        } => {
            let cid = ContainerId(subst(id, results));
            let result = backend.stop_container(&cid, *grace_seconds);
            check_err_expectation(result, expect_err, "stop_container", |_| None)
        }
        Step::RemoveContainer { id, expect_err } => {
            let cid = ContainerId(subst(id, results));
            let result = backend.remove_container(&cid);
            check_err_expectation(result, expect_err, "remove_container", |_| None)
        }
        Step::AssertStatus {
            id,
            state,
            exit_code,
            expect_err,
        } => step_assert_status(backend, id, state, exit_code, expect_err, results),
        Step::WaitExited {
            id,
            timeout_seconds,
            expect_err,
        } => step_wait_exited(backend, id, *timeout_seconds, expect_err, results),
        _ => return None,
    })
}

fn step_assert_status(
    backend: &mut Box<dyn CriBackend>,
    id: &str,
    state: &ContainerState,
    exit_code: &Option<i32>,
    expect_err: &Option<String>,
    results: &[Option<String>],
) -> StepOutcome {
    let cid = ContainerId(subst(id, results));
    match backend.container_status(&cid) {
        Ok(status) => {
            if let Some(expected) = expect_err {
                return StepOutcome::Fail(format!(
                    "assert_status: expected error '{expected}' but call succeeded"
                ));
            }
            if status.state != *state {
                return StepOutcome::Fail(format!(
                    "assert_status: expected state {:?}, got {:?}",
                    state, status.state
                ));
            }
            if let Some(expected_code) = exit_code {
                if status.exit_code != *expected_code {
                    return StepOutcome::Fail(format!(
                        "assert_status: expected exit_code {}, got {}",
                        expected_code, status.exit_code
                    ));
                }
            }
            StepOutcome::Ok(None)
        }
        Err(e) => match_err(&e, expect_err, "assert_status"),
    }
}

fn step_wait_exited(
    backend: &mut Box<dyn CriBackend>,
    id: &str,
    timeout_seconds: u64,
    expect_err: &Option<String>,
    results: &[Option<String>],
) -> StepOutcome {
    let cid = ContainerId(subst(id, results));
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    loop {
        match backend.container_status(&cid) {
            Ok(status) => {
                if status.state == ContainerState::Exited {
                    if let Some(expected) = expect_err {
                        return StepOutcome::Fail(format!(
                            "wait_exited: expected error '{expected}' but container exited"
                        ));
                    }
                    return StepOutcome::Ok(None);
                }
                if Instant::now() >= deadline {
                    return StepOutcome::Fail(format!(
                        "wait_exited: timeout after {timeout_seconds}s — state was {:?}",
                        status.state
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return match_err(&e, expect_err, "wait_exited"),
        }
    }
}

/// Shared error-matcher for the status-shaped steps (transcribed inline arms).
pub fn match_err(e: &BackendError, expect_err: &Option<String>, step: &str) -> StepOutcome {
    match expect_err {
        None => StepOutcome::Fail(format!("{step}: unexpected error: {e}")),
        Some(expected) => {
            let actual = variant_name(e);
            if actual == expected.as_str() {
                StepOutcome::Ok(None)
            } else {
                StepOutcome::Fail(format!(
                    "{step}: expected error '{expected}', got '{actual}': {e}"
                ))
            }
        }
    }
}

/// Path-join helper used by the log steps (transcribed).
pub fn log_full_path(log_dir: &str, log_path: &str) -> std::path::PathBuf {
    Path::new(log_dir).join(log_path)
}
