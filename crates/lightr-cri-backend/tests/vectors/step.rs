//! Conformance-vector JSON shape (`Vector` + `Step`) — TRANSCRIBED from
//! `lightr-cri/crates/lightr-cri-vectors/src/lib.rs` @ seam-contract-v1.1.
//!
//! Split from `runner.rs` only to honor the 400-LOC godfile guard. `$N` =
//! result of step N; `expect_err` = exact `BackendError` variant name;
//! `reopen_backend` = crash-recovery step. WIRE-LEVEL SEAM PROOF, NOT a dep.

use lightr_cri_backend::{ContainerConfig, ContainerState, SandboxConfig, SandboxState};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Vector {
    pub name: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Step {
    RunSandbox {
        cfg: SandboxConfig,
        #[serde(default)]
        expect_err: Option<String>,
    },
    StopSandbox {
        id: String,
        #[serde(default)]
        expect_err: Option<String>,
    },
    RemoveSandbox {
        id: String,
        #[serde(default)]
        expect_err: Option<String>,
    },
    SandboxStatus {
        id: String,
        #[serde(default)]
        expect_state: Option<SandboxState>,
        #[serde(default)]
        expect_err: Option<String>,
    },
    CreateContainer {
        sandbox: String,
        cfg: ContainerConfig,
        #[serde(default)]
        expect_err: Option<String>,
    },
    StartContainer {
        id: String,
        #[serde(default)]
        expect_err: Option<String>,
    },
    StopContainer {
        id: String,
        #[serde(default)]
        grace_seconds: i64,
        #[serde(default)]
        expect_err: Option<String>,
    },
    RemoveContainer {
        id: String,
        #[serde(default)]
        expect_err: Option<String>,
    },
    AssertStatus {
        id: String,
        state: ContainerState,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default)]
        expect_err: Option<String>,
    },
    WaitExited {
        id: String,
        timeout_seconds: u64,
        #[serde(default)]
        expect_err: Option<String>,
    },
    ExecSync {
        id: String,
        cmd: Vec<String>,
        #[serde(default)]
        expect_exit_code: Option<i32>,
        #[serde(default)]
        expect_stdout: Option<String>,
        #[serde(default)]
        expect_err: Option<String>,
    },
    PullImage {
        #[serde(rename = "ref")]
        image_ref: String,
        #[serde(default)]
        store_as_result: bool,
        #[serde(default)]
        expect_err: Option<String>,
    },
    ImageStatus {
        #[serde(rename = "ref")]
        image_ref: String,
        #[serde(default)]
        expect_present: Option<bool>,
        #[serde(default)]
        expect_err: Option<String>,
    },
    ListImages {
        #[serde(default)]
        expect_count: Option<usize>,
        #[serde(default)]
        expect_err: Option<String>,
    },
    RemoveImage {
        #[serde(rename = "ref")]
        image_ref: String,
        #[serde(default)]
        expect_err: Option<String>,
    },
    ReopenBackend {},
    OpenExec {
        id: String,
        cmd: Vec<String>,
        #[serde(default)]
        tty: bool,
        #[serde(default)]
        stdin: bool,
        #[serde(default)]
        expect_exit_code: Option<i32>,
        #[serde(default)]
        expect_stdout_contains: Option<String>,
        #[serde(default)]
        expect_err: Option<String>,
    },
    AssertLogExists {
        sandbox_id: String,
        container_id: String,
        #[serde(default)]
        expect_err: Option<String>,
    },
    AssertLogFormat {
        sandbox_id: String,
        container_id: String,
        #[serde(default)]
        expect_err: Option<String>,
    },
    SandboxStatusIp {
        id: String,
        expect_ip_present: bool,
        #[serde(default)]
        expect_err: Option<String>,
    },
}
