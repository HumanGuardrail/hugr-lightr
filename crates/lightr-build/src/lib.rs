//! lightr-build -- frozen contract: build-spec-r3.md §2+§3.
//! Dockerfile build graph (step-memoized) + lazy compose. Bodies: R3-W1/W2.
//!
//! # Compose YAML subset supported by `parse_compose`
//!
//! Hand-rolled; **no external YAML dep**. Supports:
//! ```yaml
//! services:
//!   web:
//!     image: myref
//!     command: ["sh", "-c", "sleep 1"]   # JSON array or bare string
//!     ports:
//!       - "8080:80"
//!     environment:
//!       - FOO=bar          # list form
//!       # OR map form:
//!       # FOO: bar
//!     x-lightr-eager: true
//! ```
//! Unknown keys are silently ignored. Parse errors include the 1-based line
//! number for quick diagnosis.
//!
//! # Compose supervisor model (ADR-0015)
//!
//! `compose_up` writes a `spec.json` under `$LIGHTR_HOME/compose/<nanos-pid>/`,
//! spawns a detached `lightr __compose-supervise <stack_dir>` process (re-uses
//! the same re-exec pattern as `lightr_run::spawn_detached`), then returns a
//! `ComposeHandle`. The supervisor (implemented as `compose_supervise`) does
//! the bind/accept/proxy loop and self-exits when `$stack_dir/stop` exists or
//! the TTL fires.
//!
//! **Known limits (document-only, not bugs):**
//! - Proxy is a simple bidirectional byte-copy; no TLS, no HTTP semantics.
//! - Service start latency on first connect = the service startup time (honest).
//! - No healthcheck before proxying; first-packet arrives as soon as
//!   `spawn_detached` returns.
//! - `compose_down` kills via `pid` file; on SIGKILL the proxy threads are
//!   reaped with the process (no zombie sockets on modern kernels).
//! - Proxy correctness is validated as an integration test (A24), not a unit
//!   test (tcp round-trip is flaky in tight loops).
//!
//! # RUN determinism caveat
//!
//! RUN steps that read the clock or network are not reproducible.
//! `step_reads_clock_or_net` provides a heuristic for `--explain` (W3/CLI).
//! Flagging is CLI-level; `build` itself records every step faithfully.
//!
//! # Native-engine note
//!
//! R3 executes RUN steps via the **native** engine (`rootfs: None`). There
//! is no filesystem isolation -- RUN writes directly into the CoW working
//! tree. This is stated loudly in build output by the CLI (W3).

mod build;

pub use build::{
    build, compose_down, compose_supervise, compose_up, parse_compose, parse_dockerfile,
    step_reads_clock_or_net, BuildReport, BuildStep, Compose, ComposeHandle, Instr, Service,
    ServiceSpec, StackSpec,
};
