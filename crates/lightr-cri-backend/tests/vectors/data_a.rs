//! Transcribed conformance vectors (godfile-split half) —
//! verbatim from `lightr-cri/vectors/*.json` @ seam-contract-v1.1.

use super::{Category, VectorDef};

pub const GROUP: &[VectorDef] = &[
    VectorDef {
        name: "container-lifecycle-basic",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "container-lifecycle-basic",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/busy", "command": ["/bin/sh", "-c", "exit 0"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "wait_exited", "id": "$1", "timeout_seconds": 10 },
    { "op": "assert_status", "id": "$1", "state": "Exited", "exit_code": 0 },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "container-remove-while-running-force",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "container-remove-while-running-force",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sleep", "command": ["/bin/sleep", "30"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "container-start-from-exited-refused",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "container-start-from-exited-refused",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/exit0", "command": ["/bin/sh", "-c", "exit 0"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "wait_exited", "id": "$1", "timeout_seconds": 10 },
    { "op": "start_container", "id": "$1", "expect_err": "FailedPrecondition" },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "container-start-from-running-refused",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "container-start-from-running-refused",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sleep", "command": ["/bin/sleep", "30"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "start_container", "id": "$1", "expect_err": "FailedPrecondition" },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "container-status-unknown-not-found",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "container-status-unknown-not-found",
  "steps": [
    { "op": "assert_status", "id": "does-not-exist-container-id", "state": "Created", "expect_err": "NotFound" }
  ]
}
"#,
    },
    VectorDef {
        name: "crash-recovery-reopen-after-stop-preserves-exited",
        category: Category::DeferSandbox,
        json: r#"{
  "name": "crash-recovery-reopen-after-stop-preserves-exited",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sh", "command": ["/bin/sh", "-c", "exit 42"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "wait_exited", "id": "$1", "timeout_seconds": 10 },
    { "op": "assert_status", "id": "$1", "state": "Exited", "exit_code": 42 },
    { "op": "reopen_backend" },
    { "op": "assert_status", "id": "$1", "state": "Exited", "exit_code": 42 },
    { "op": "sandbox_status", "id": "$0", "expect_state": "Ready" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "crash-recovery-reopen-preserves-images-and-not-ready-sandbox",
        category: Category::DeferSandbox,
        json: r#"{
  "name": "crash-recovery-reopen-preserves-images-and-not-ready-sandbox",
  "steps": [
    { "op": "pull_image", "ref": "ref/persist-test:1" },
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "stop_sandbox", "id": "$1" },
    { "op": "sandbox_status", "id": "$1", "expect_state": "NotReady" },
    { "op": "reopen_backend" },
    { "op": "image_status", "ref": "ref/persist-test:1", "expect_present": true },
    { "op": "sandbox_status", "id": "$1", "expect_state": "NotReady" },
    { "op": "remove_sandbox", "id": "$1" }
  ]
}
"#,
    },
    VectorDef {
        name: "crash-recovery-sandbox-survives",
        category: Category::DeferSandbox,
        json: r#"{
  "name": "crash-recovery-sandbox-survives",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sleeper", "command": ["/bin/sleep", "30"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "reopen_backend" },
    { "op": "assert_status", "id": "$1", "state": "Running" },
    { "op": "sandbox_status", "id": "$0", "expect_state": "Ready" },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "create-container-in-not-ready-sandbox",
        category: Category::DeferSandbox,
        json: r#"{
  "name": "create-container-in-not-ready-sandbox",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "stop_sandbox", "id": "$0" },
    { "op": "sandbox_status", "id": "$0", "expect_state": "NotReady" },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/x", "command": ["/bin/true"] }, "expect_err": "FailedPrecondition" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "create-container-in-unknown-sandbox",
        category: Category::DeferSandbox,
        json: r#"{
  "name": "create-container-in-unknown-sandbox",
  "steps": [
    { "op": "create_container", "sandbox": "no-such-sandbox-id", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/x", "command": ["/bin/true"] }, "expect_err": "NotFound" }
  ]
}
"#,
    },
    VectorDef {
        name: "exec-session-echo-exit-code",
        category: Category::DeferStream,
        json: r#"{
  "name": "exec-session-echo-exit-code",
  "steps": [
    {
      "op": "run_sandbox",
      "cfg": {
        "name": "s1",
        "uid": "u1",
        "namespace": "ns",
        "attempt": 0
      }
    },
    {
      "op": "create_container",
      "sandbox": "$0",
      "cfg": {
        "name": "c1",
        "attempt": 0,
        "image_ref": "ref/sh",
        "command": ["/bin/sleep", "60"]
      }
    },
    { "op": "start_container", "id": "$1" },
    {
      "op": "open_exec",
      "id": "$1",
      "cmd": ["/bin/echo", "hi"],
      "expect_exit_code": 0,
      "expect_stdout_contains": "hi"
    },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "exec-session-exit-7",
        category: Category::DeferStream,
        json: r#"{
  "name": "exec-session-exit-7",
  "steps": [
    {
      "op": "run_sandbox",
      "cfg": {
        "name": "s1",
        "uid": "u1",
        "namespace": "ns",
        "attempt": 0
      }
    },
    {
      "op": "create_container",
      "sandbox": "$0",
      "cfg": {
        "name": "c1",
        "attempt": 0,
        "image_ref": "ref/sh",
        "command": ["/bin/sleep", "60"]
      }
    },
    { "op": "start_container", "id": "$1" },
    {
      "op": "open_exec",
      "id": "$1",
      "cmd": ["/bin/sh", "-c", "exit 7"],
      "expect_exit_code": 7
    },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "exec-sync-echo-and-exit-code",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "exec-sync-echo-and-exit-code",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sh", "command": ["/bin/sleep", "60"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "exec_sync", "id": "$1", "cmd": ["/bin/echo", "hello"], "expect_exit_code": 0, "expect_stdout": "hello" },
    { "op": "exec_sync", "id": "$1", "cmd": ["/bin/sh", "-c", "exit 7"], "expect_exit_code": 7 },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "host-network-sandbox-no-ip",
        category: Category::DeferSandbox,
        json: r#"{
  "name": "host-network-sandbox-no-ip",
  "steps": [
    {
      "op": "run_sandbox",
      "cfg": {
        "name": "s1",
        "uid": "u1",
        "namespace": "ns",
        "attempt": 0,
        "host_network": true
      }
    },
    { "op": "sandbox_status_ip", "id": "$0", "expect_ip_present": false },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "idempotency-remove-twice",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "idempotency-remove-twice",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/exit0", "command": ["/bin/sh", "-c", "exit 0"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "wait_exited", "id": "$1", "timeout_seconds": 10 },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
];
