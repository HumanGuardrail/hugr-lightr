//! bin `lightr-cri-serve` — the R2 integration entrypoint: drives the REAL
//! `LightrBackend` (this workspace) through the kubelet-facing `lightr-cri`
//! server shell (sibling repo) via the `Adapter` seam bridge.
//!
//! This is the integrated counterpart of the sibling's own `lightr-cri` bin
//! (which drives the in-memory fake). It calls the SAME backend-agnostic
//! `lightr_cri_server::run_blocking` entry — a backend-construction swap, never
//! a copy-paste of the wiring (contract-swap law).
//!
//! Args mirror the sibling bin (clap-free): `--socket PATH` (default
//! /run/lightr/cri.sock), `--state PATH` (the LightrBackend home root; default
//! honours $LIGHTR_CRI_STATE, else $TMPDIR/lightr-cri). A production deployment
//! should pass an explicit persistent `--state` root.

mod adapter;
mod convert;
mod ns_exec_shim;
mod ns_shim;

use std::path::PathBuf;
use std::sync::Arc;

use adapter::Adapter;
use lightr_cri_backend::LightrBackend;

fn parse_args() -> Option<(PathBuf, Option<PathBuf>)> {
    let mut args = std::env::args().skip(1);
    let mut socket: PathBuf = PathBuf::from("/run/lightr/cri.sock");
    let mut state: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = PathBuf::from(args.next()?),
            "--state" => state = Some(PathBuf::from(args.next()?)),
            other => {
                eprintln!("Usage: lightr-cri-serve [--socket PATH] [--state PATH]");
                eprintln!("Unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }
    Some((socket, state))
}

/// Resolve the backend state root (the `LightrBackend` home): explicit `--state`
/// wins, then $LIGHTR_CRI_STATE, then a $TMPDIR fallback (mirrors the sibling
/// fake bin's convention).
fn resolve_state(state: Option<PathBuf>) -> PathBuf {
    if let Some(p) = state {
        return p;
    }
    if let Ok(v) = std::env::var("LIGHTR_CRI_STATE") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    std::env::temp_dir().join("lightr-cri")
}

fn main() {
    // WP-#99 (CRI slice 1): hidden `__ns-run` re-exec dispatch (mirrors the CLI's
    // `__supervise` shape). The backend spawns `<current_exe> __ns-run` with a
    // `RunDescriptor` piped on stdin to run a container under the `ns` engine. This
    // MUST precede `parse_args` (which rejects unknown args).
    if std::env::args().nth(1).as_deref() == Some("__ns-run") {
        ns_shim::main();
    }
    // WP-#100 (CRI exec slice 1): hidden `__ns-exec` re-exec dispatch (mirrors
    // `__ns-run`). The backend spawns `<current_exe> __ns-exec` with an
    // `ExecDescriptor` in `LIGHTR_NSEXEC_DESC` to ENTER a running ns container
    // (setns into PID-1's namespaces). Also MUST precede `parse_args`.
    if std::env::args().nth(1).as_deref() == Some("__ns-exec") {
        ns_exec_shim::main();
    }

    let (socket_path, state_arg) = match parse_args() {
        Some(v) => v,
        None => {
            eprintln!("Usage: lightr-cri-serve [--socket PATH] [--state PATH]");
            std::process::exit(1);
        }
    };

    let state_path = resolve_state(state_arg);

    // Construct the REAL backend rooted at the state dir (infallible: it
    // degrades to an empty cache rather than panicking — crash-only law), wrap
    // it in the canonical-seam Adapter, and hand it to the generic server.
    let backend = LightrBackend::new(&state_path);
    let adapter = Adapter(backend);

    std::process::exit(lightr_cri_server::run_blocking(
        Arc::new(adapter),
        socket_path,
    ));
}
