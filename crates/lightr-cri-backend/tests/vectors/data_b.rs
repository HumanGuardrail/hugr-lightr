//! Transcribed conformance vectors (godfile-split half) —
//! verbatim from `lightr-cri/vectors/*.json` @ seam-contract-v1.1.

use super::{Category, VectorDef};

pub const GROUP: &[VectorDef] = &[
    VectorDef {
        name: "idempotency-stop-sandbox-twice",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "idempotency-stop-sandbox-twice",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "stop_sandbox", "id": "$0" },
    { "op": "sandbox_status", "id": "$0", "expect_state": "NotReady" },
    { "op": "stop_sandbox", "id": "$0" },
    { "op": "sandbox_status", "id": "$0", "expect_state": "NotReady" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "idempotency-stop-twice",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "idempotency-stop-twice",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sleep", "command": ["/bin/sleep", "30"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "assert_status", "id": "$1", "state": "Exited" },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "assert_status", "id": "$1", "state": "Exited" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "image-invalid-ref-rejected",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "image-invalid-ref-rejected",
  "steps": [
    { "op": "pull_image", "ref": "invalid ref with whitespace", "expect_err": "InvalidArgument" }
  ]
}
"#,
    },
    VectorDef {
        name: "image-pull-list-status-remove",
        category: Category::DeferNet,
        json: r#"{
  "name": "image-pull-list-status-remove",
  "steps": [
    { "op": "list_images", "expect_count": 0 },
    { "op": "pull_image", "ref": "ref/alpine:3.18" },
    { "op": "list_images", "expect_count": 1 },
    { "op": "image_status", "ref": "ref/alpine:3.18", "expect_present": true },
    { "op": "remove_image", "ref": "ref/alpine:3.18" },
    { "op": "list_images", "expect_count": 0 },
    { "op": "image_status", "ref": "ref/alpine:3.18", "expect_present": false }
  ]
}
"#,
    },
    VectorDef {
        name: "image-remove-while-running-in-use",
        category: Category::DeferNet,
        json: r#"{
  "name": "image-remove-while-running-in-use",
  "steps": [
    { "op": "pull_image", "ref": "ref/busy-image:latest" },
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$1", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/busy-image:latest", "command": ["/bin/sleep", "30"] } },
    { "op": "start_container", "id": "$2" },
    { "op": "remove_image", "ref": "ref/busy-image:latest", "expect_err": "InUse" },
    { "op": "stop_container", "id": "$2", "grace_seconds": 0 },
    { "op": "remove_sandbox", "id": "$1" }
  ]
}
"#,
    },
    VectorDef {
        name: "log-file-created-and-format",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "log-file-created-and-format",
  "steps": [
    {
      "op": "run_sandbox",
      "cfg": {
        "name": "s1",
        "uid": "u1",
        "namespace": "ns",
        "attempt": 0,
        "log_directory": "/tmp/lightr-cri-log-test/sb1"
      }
    },
    {
      "op": "create_container",
      "sandbox": "$0",
      "cfg": {
        "name": "c1",
        "attempt": 0,
        "image_ref": "ref/sh",
        "command": ["/bin/sh", "-c", "echo hello"],
        "log_path": "c1/0.log"
      }
    },
    { "op": "start_container", "id": "$1" },
    { "op": "wait_exited", "id": "$1", "timeout_seconds": 10 },
    { "op": "assert_log_exists",  "sandbox_id": "$0", "container_id": "$1" },
    { "op": "assert_log_format",  "sandbox_id": "$0", "container_id": "$1" },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "remove-image-idempotent",
        category: Category::DeferNet,
        json: r#"{
  "name": "remove-image-idempotent",
  "steps": [
    { "op": "remove_image", "ref": "never-pulled:latest" },
    { "op": "remove_image", "ref": "never-pulled:latest" },
    { "op": "pull_image", "ref": "real-img:v1" },
    { "op": "remove_image", "ref": "real-img:v1" },
    { "op": "remove_image", "ref": "real-img:v1" }
  ]
}
"#,
    },
    VectorDef {
        name: "remove-unknown-sandbox-idempotent",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "remove-unknown-sandbox-idempotent",
  "steps": [
    { "op": "remove_sandbox", "id": "sb-never-existed" }
  ]
}
"#,
    },
    VectorDef {
        name: "sandbox-lifecycle-legal",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "sandbox-lifecycle-legal",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "sandbox_status", "id": "$0", "expect_state": "Ready" },
    { "op": "stop_sandbox", "id": "$0" },
    { "op": "sandbox_status", "id": "$0", "expect_state": "NotReady" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "sandbox-removal-cascade",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "sandbox-removal-cascade",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sleep", "command": ["/bin/sleep", "30"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "assert_status", "id": "$1", "state": "Running" },
    { "op": "remove_sandbox", "id": "$0" },
    { "op": "assert_status", "id": "$1", "state": "Exited", "expect_err": "NotFound" }
  ]
}
"#,
    },
    VectorDef {
        name: "sandbox-status-unknown-not-found",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "sandbox-status-unknown-not-found",
  "steps": [
    { "op": "sandbox_status", "id": "does-not-exist-sandbox-id", "expect_err": "NotFound" }
  ]
}
"#,
    },
    VectorDef {
        name: "stop-from-created-noop",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "stop-from-created-noop",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sleep", "command": ["/bin/sleep", "5"] } },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "assert_status", "id": "$1", "state": "Created" },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "stop-running-exit-code-143",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "stop-running-exit-code-143",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sleep", "command": ["/bin/sleep", "30"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "stop_container", "id": "$1", "grace_seconds": 5 },
    { "op": "assert_status", "id": "$1", "state": "Exited", "exit_code": 143 },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
    VectorDef {
        name: "stop-running-grace0-exit-code-137",
        category: Category::RunLifecycle,
        json: r#"{
  "name": "stop-running-grace0-exit-code-137",
  "steps": [
    { "op": "run_sandbox", "cfg": { "name": "s1", "uid": "u1", "namespace": "ns", "attempt": 0 } },
    { "op": "create_container", "sandbox": "$0", "cfg": { "name": "c1", "attempt": 0, "image_ref": "ref/sleep", "command": ["/bin/sleep", "30"] } },
    { "op": "start_container", "id": "$1" },
    { "op": "stop_container", "id": "$1", "grace_seconds": 0 },
    { "op": "assert_status", "id": "$1", "state": "Exited", "exit_code": 137 },
    { "op": "remove_container", "id": "$1" },
    { "op": "remove_sandbox", "id": "$0" }
  ]
}
"#,
    },
];
