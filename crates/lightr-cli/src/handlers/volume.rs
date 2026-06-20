//! `lightr volume` handlers — named-volume management (docker volume parity).
//!
//! Wires the five frozen sub-verbs (create/ls/rm/inspect/prune) to the
//! daemonless on-disk volume registry in `lightr-store` (WP-VOL-4). The store
//! ROOT is resolved once here and INJECTED into every registry call (house
//! convention — the registry never reads the global env itself).
//!
//! In-use ref-counting against running containers is WP-VOL-5; nothing mounts
//! named volumes yet (mount-wiring is VOL-8/9/10), so `rm` passes `in_use=false`
//! and `prune` removes every volume. We do NOT fake an in-use signal.

use std::path::Path;

use lightr_store::{volume, Store, VolumeInfo};

use crate::cli::cmd::VolumeCmd;
use crate::exit::die_lightr;

pub fn run(subcmd: VolumeCmd) -> i32 {
    let root = Store::default_root();
    match subcmd {
        VolumeCmd::Create { name } => create(&root, name),
        VolumeCmd::Ls { json } => ls(&root, json),
        VolumeCmd::Rm { targets, force } => rm(&root, &targets, force),
        VolumeCmd::Inspect { target, json } => inspect(&root, &target, json),
        VolumeCmd::Prune { force } => prune(&root, force),
    }
}

// ── create ────────────────────────────────────────────────────────────────────

/// `docker volume create [name]`. A missing name gets a docker-style anonymous
/// 64-hex id. The CLI surface carries no `--label` flag (frozen), so labels are
/// empty. Prints the volume name on success (docker shape).
fn create(root: &Path, name: Option<String>) -> i32 {
    let name = name.unwrap_or_else(anon_name);
    match volume::create(root, &name, &[]) {
        Ok(info) => {
            println!("{}", info.name);
            0
        }
        Err(e) => die_lightr(&e),
    }
}

// ── ls ──────────────────────────────────────────────────────────────────────

/// `docker volume ls`. Columns: DRIVER, VOLUME NAME. `--json` emits one JSON
/// object per line (docker's `--format json` line-delimited shape).
fn ls(root: &Path, json: bool) -> i32 {
    let vols = match volume::list(root) {
        Ok(v) => v,
        Err(e) => return die_lightr(&e),
    };
    if json {
        for v in &vols {
            println!("{}", v.to_json());
        }
    } else {
        println!("{:<14}VOLUME NAME", "DRIVER");
        for v in &vols {
            println!("{:<14}{}", v.driver, v.name);
        }
    }
    0
}

// ── rm ──────────────────────────────────────────────────────────────────────

/// `docker volume rm <name>...`. Removes each named volume. `-f/--force`
/// ignores a missing volume (docker's `--force` semantics). In-use is always
/// false today (WP-VOL-5). Any non-ignored error fails the whole command with
/// that error's exit code, after attempting the rest.
fn rm(root: &Path, targets: &[String], force: bool) -> i32 {
    if targets.is_empty() {
        eprintln!("lightr: volume rm: requires at least one volume name");
        return 2;
    }
    let mut code = 0;
    for name in targets {
        // WP-VOL-5: real refcount — pass the live in-use signal; today false.
        match volume::remove(root, name, false) {
            Ok(()) => println!("{name}"),
            Err(lightr_core::LightrError::RefNotFound(_)) if force => {
                // --force: a missing volume is not an error.
            }
            Err(e) => code = die_lightr(&e),
        }
    }
    code
}

// ── inspect ───────────────────────────────────────────────────────────────────

/// `docker volume inspect <name>`. Always prints JSON (docker inspect is
/// JSON-only); the `--json` flag is accepted for surface parity and is a no-op.
fn inspect(root: &Path, target: &str, _json: bool) -> i32 {
    match volume::inspect(root, target) {
        Ok(info) => {
            print_inspect(&info);
            0
        }
        Err(e) => die_lightr(&e),
    }
}

/// Docker prints `inspect` as a JSON array of objects; mirror that shape.
fn print_inspect(info: &VolumeInfo) {
    println!("[{}]", info.to_json());
}

// ── prune ─────────────────────────────────────────────────────────────────────

/// `docker volume prune`. Removes all dangling (not-in-use) volumes. Today no
/// volume is in use (WP-VOL-5 owns ref-counting), so every volume is dangling.
/// `-f/--force` skips the interactive prompt — we never prompt (daemonless,
/// non-interactive), so the flag is accepted and is a no-op.
fn prune(root: &Path, _force: bool) -> i32 {
    match volume::prune(root) {
        Ok(removed) => {
            if !removed.is_empty() {
                println!("Deleted Volumes:");
                for name in &removed {
                    println!("{name}");
                }
            }
            0
        }
        Err(e) => die_lightr(&e),
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// A docker-style anonymous-volume name: 64 lowercase hex chars derived from a
/// content-addressed digest of the wall-clock nanos + a process-unique nonce.
/// Reuses `lightr_core::Digest` so we add no new dependency.
fn anon_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = NONCE.fetch_add(1, Ordering::Relaxed);
    let seed = format!("{}:{nanos}:{n}", std::process::id());
    lightr_core::Digest::of_bytes(seed.as_bytes()).to_hex()
}
