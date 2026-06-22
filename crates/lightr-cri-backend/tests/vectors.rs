//! WP-CRI-VECTORS-2 — the seam's red→green acceptance bar, FULL-SUITE proof.
//!
//! Proves `LightrBackend` passes the SAME shared conformance vectors the
//! lightr-cri fake passes, for the FULL implemented plane: sandbox state
//! machine + container / exec / image / stats + streaming `open_exec` + the CRI
//! log file. WP-CRI-SANDBOX + WP-CRI-STREAM wired these methods, so the
//! sandbox, streaming, and log vectors that WP-CRI-VECTORS DEFERRED are now
//! UN-DEFERRED and RUN DIRECTLY against the real `LightrBackend`: the scaffold
//! (in-memory sandbox bookkeeping) is GONE; the factory hands the executor a
//! real backend. The runner + the vector corpus are TRANSCRIBED from `lightr-cri`
//! @ seam-contract-v1.1 (wire-level seam proof, NOT a git/path dep; drift is
//! caught HERE; see `vectors/data.rs` + `vectors/runner.rs`).
//!
//! GREENLIST DISCIPLINE (fail-closed, never silent): every vector is either RUN
//! or gated out + LOGGED with its reason (see `Category` in `vectors/data.rs`).
//! The ONLY remaining deferred class is `DeferNet`: vectors that pull image
//! CONTENT from a live OCI registry (the fake fabricates the record in-memory;
//! the real backend performs a live network pull — no network in the macOS
//! gate). The sandbox STATE-MACHINE, streaming, and CRI-log vectors all RUN here
//! (the netns/CNI RUNTIME is cfg(linux) + probe-truthful: on macOS `ip = None`,
//! so the `host-network-sandbox-no-ip` ABSENCE assertion holds; no vector
//! asserts a CNI-assigned IP, so nothing defers on the Linux-runtime axis).
//!
//! Parallel-safe: each vector runs over its own unique tempdir `home` (atomic
//! counter + nanos); no `set_var`, no shared global.

#[path = "vectors/data.rs"]
mod data;
#[path = "vectors/runner.rs"]
mod runner;
#[path = "vectors/runner2.rs"]
mod runner2;
#[path = "vectors/step.rs"]
mod step;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use lightr_cri_backend::{CriBackend, LightrBackend};

use data::Category;
use runner::{BackendFactory, Vector};

// ── unique tempdir home (parallel-safe; no set_var) ──────────────────────────

fn temp_home() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("lightr-cri-vec-{nanos}-{n}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// ── BackendFactory over the REAL LightrBackend (no scaffold) ─────────────────

/// `fresh()` = new tempdir home → a real `LightrBackend`; `reopen()` =
/// `LightrBackend::new(same_home)` (crash-only law: sandbox + container + image
/// state re-derives from disk). The sandbox plane is now real + persistent, so
/// the vectors drive `LightrBackend` directly — no in-memory scaffold.
struct LightrFactory {
    home: Mutex<PathBuf>,
}

impl LightrFactory {
    fn new() -> Self {
        Self {
            home: Mutex::new(temp_home()),
        }
    }
}

impl BackendFactory for LightrFactory {
    fn fresh(&self) -> Box<dyn CriBackend> {
        let home = temp_home();
        *self.home.lock().unwrap() = home.clone();
        Box::new(LightrBackend::new(home))
    }
    fn reopen(&self) -> Box<dyn CriBackend> {
        let home = self.home.lock().unwrap().clone();
        Box::new(LightrBackend::new(home))
    }
}

// ── The acceptance test: RUN the full plane, LOG the network-deferred ─────────

#[test]
fn conformance_vectors_prove_the_mvp_backend() {
    let factory = LightrFactory::new();

    let mut run_pass = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut deferred: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();

    for def in data::vectors() {
        if def.category != Category::RunLifecycle {
            let reason = match def.category {
                Category::DeferNet => "live OCI image-content pull (no network in the macOS gate)",
                Category::RunLifecycle => unreachable!(),
            };
            deferred.entry(reason).or_default().push(def.name);
            continue;
        }
        let vector: Vector = serde_json::from_str(def.json)
            .unwrap_or_else(|e| panic!("transcribed vector {} failed to parse: {e}", def.name));
        match runner::run_vector(&factory, &vector) {
            Ok(()) => run_pass += 1,
            Err(msg) => failures.push(msg),
        }
    }

    // GREENLIST log — never a silent skip.
    eprintln!("── WP-CRI-VECTORS-2 GREENLIST ─────────────────────────────");
    eprintln!("RUN (full plane: sandbox+container+exec+image+stats+stream+log): {run_pass} passed");
    let deferred_total: usize = deferred.values().map(Vec::len).sum();
    eprintln!("DEFERRED (gated out, logged): {deferred_total}");
    for (reason, names) in &deferred {
        eprintln!("  [{}] {}: {}", names.len(), reason, names.join(", "));
    }
    eprintln!("───────────────────────────────────────────────────────────");

    if !failures.is_empty() {
        for f in &failures {
            eprintln!("FAILED: {f}");
        }
        panic!("{} RUN vector(s) failed (see above)", failures.len());
    }

    // Lock the proven count so an accidental re-classification (e.g. silently
    // dropping a vector to "deferred") is caught by the gate.
    assert_eq!(run_pass, 25, "expected 25 RunLifecycle vectors to RUN+PASS");
    assert_eq!(
        deferred_total, 4,
        "expected 4 DeferNet vectors (live OCI pull), logged"
    );
}
