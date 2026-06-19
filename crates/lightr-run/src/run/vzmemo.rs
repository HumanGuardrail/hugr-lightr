//! vz-memo — memoize container runs (build-spec-prod.md, the product's moat).
//!
//! A `vz` container run (`lightr run --engine vz --rootfs <ref> -- <cmd>`) is
//! memoized EXACTLY like the native path: the 1st run boots the VM + captures
//! {exit, stdout, stderr}; an identical 2nd run is a HIT that replays them from
//! the Action Cache with NO VM boot. The hit/miss flow mirrors
//! `run_memoized_with` byte-for-byte — the only difference is that the "run" is a
//! caller-supplied closure (boot the VM, read the guest's capture files) instead
//! of a native `Command`. Caching law is identical: store ONLY when `exit == 0`
//! AND both streams are within `OUTPUT_CAP_BYTES`; replay is byte-exact.
//!
//! The memo key is a SEPARATE, domain-separated key (`b"lightr-vz-memo-v1"`) — a
//! vz run keys on (command, rootfs image digest, env), NOT on a cwd scan, so it
//! never collides with a native `run/v1` key.

use lightr_core::{Digest, Result, OUTPUT_CAP_BYTES};
use lightr_store::Store;

use super::ac::{decode_ac_record, encode_ac_record};
use super::types::{RunOutcome, VzMemoKey};

/// Compute the memo key for a `vz` container run. blake3 over a
/// DOMAIN-SEPARATED, length-prefixed encoding so the layout is unambiguous and
/// no field boundary can be forged by concatenation:
///   update(b"lightr-vz-memo-v1")            // fixed domain tag
///   update(rootfs_digest.0)                 // 32B image content digest
///   for arg in command:  len(u64 LE) + arg  // length-prefixed, ordered
///   for (k, v) in env:   len(k=v, u64 LE) + "k=v"  // length-prefixed, ordered
///   update(OS) update("-") update(ARCH)     // target triple
///
/// Determinism is essential: identical inputs ⇒ identical key (see
/// `vz_memo_key_is_deterministic`); any field change ⇒ a different key (see
/// `vz_memo_key_is_sensitive_to_every_field`).
pub fn vz_memo_key(k: &VzMemoKey) -> lightr_core::Digest {
    let mut hasher = blake3::Hasher::new();

    // Fixed domain tag — separates this key space from `run/v1` etc.
    hasher.update(b"lightr-vz-memo-v1");

    // Rootfs image content digest (32 bytes, fixed width — no length prefix
    // needed; it is always exactly 32 bytes).
    hasher.update(&k.rootfs_digest.0);

    // Command args — length-prefixed, in order.
    for arg in &k.command {
        let len = arg.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(arg.as_bytes());
    }

    // Env — length-prefixed `key=value`, in order. The encoded `k=v` is
    // length-prefixed as a whole so an env split (e.g. `A`,`B=C` vs `A=B`,`C`)
    // can never collide.
    for (key, val) in &k.env {
        let mut kv = Vec::with_capacity(key.len() + 1 + val.len());
        kv.extend_from_slice(key.as_bytes());
        kv.push(b'=');
        kv.extend_from_slice(val.as_bytes());
        let len = kv.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(&kv);
    }

    // Target triple: os/arch (a cached Linux-guest result is host-arch specific).
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update(b"-");
    hasher.update(std::env::consts::ARCH.as_bytes());

    Digest(*hasher.finalize().as_bytes())
}

/// Memoize a `vz` container run. Mirrors `run_memoized_with`'s hit/miss flow
/// EXACTLY, with the "run" supplied as a closure returning
/// `(exit_code, stdout, stderr)`:
///
/// * HIT  — `ac_get(key)` → `decode_ac_record` → `get_bytes` both streams ⇒
///   `RunOutcome { hit: true, .. }`. The closure is NEVER invoked (no VM boot).
/// * MISS — invoke `run()`, then store ONLY when `exit == 0` AND both streams
///   are `<= OUTPUT_CAP_BYTES` (`put_bytes` × 2 + `encode_ac_record` +
///   `ac_put`) ⇒ `RunOutcome { hit: false, .. }`.
///
/// Caching law is identical to the native memo: a non-zero exit or an oversized
/// stream is environmental/unbounded and is never cached, so it always re-runs.
pub fn run_vz_memoized(
    k: &VzMemoKey,
    store: &Store,
    run: impl FnOnce() -> Result<(i32, Vec<u8>, Vec<u8>)>,
) -> Result<RunOutcome> {
    let key = vz_memo_key(k);

    // --- Hit path (mirror of run_memoized_with) ---
    if let Ok(Some(record_bytes)) = store.ac_get(&key) {
        if let Some((exit_code, stdout_d, stderr_d)) = decode_ac_record(&record_bytes) {
            let stdout_res = store.get_bytes(&stdout_d);
            let stderr_res = store.get_bytes(&stderr_d);
            if let (Ok(stdout), Ok(stderr)) = (stdout_res, stderr_res) {
                return Ok(RunOutcome {
                    key,
                    hit: true,
                    exit_code,
                    stdout,
                    stderr,
                });
            }
        }
    }

    // --- Miss path: run the closure (boots the VM) and capture the result ---
    let (exit_code, stdout, stderr) = run()?;

    if exit_code == 0 && stdout.len() <= OUTPUT_CAP_BYTES && stderr.len() <= OUTPUT_CAP_BYTES {
        let stdout_d = store.put_bytes(&stdout)?;
        let stderr_d = store.put_bytes(&stderr)?;
        let record = encode_ac_record(exit_code, &stdout_d, &stderr_d);
        store.ac_put(&key, &record)?;
    }

    Ok(RunOutcome {
        key,
        hit: false,
        exit_code,
        stdout,
        stderr,
    })
}
