//! Tests for vz-memo: key determinism/sensitivity + HIT/MISS flow.
#![cfg(test)]

use crate::run::ac::decode_ac_record;
use crate::run::types::VzMemoKey;
use crate::run::vzmemo::{run_vz_memoized, vz_memo_key};
use lightr_core::OUTPUT_CAP_BYTES;
use lightr_store::Store;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("LIGHTR_HOME", home.path());
    (home, guard)
}

fn make_store(dir: &std::path::Path) -> Store {
    Store::open(dir.join("store")).expect("store open")
}

fn vz_key(command: Vec<&str>, rootfs: [u8; 32], env: Vec<(&str, &str)>) -> VzMemoKey {
    VzMemoKey {
        command: command.into_iter().map(|s| s.to_string()).collect(),
        rootfs_digest: lightr_core::Digest(rootfs),
        env: env
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

// -----------------------------------------------------------------------
// vz_memo_key_is_deterministic: identical inputs ⇒ identical key.
// -----------------------------------------------------------------------
#[test]
fn vz_memo_key_is_deterministic() {
    let k1 = vz_key(
        vec!["/bin/echo", "hi"],
        [7u8; 32],
        vec![("PATH", "/usr/bin")],
    );
    let k2 = vz_key(
        vec!["/bin/echo", "hi"],
        [7u8; 32],
        vec![("PATH", "/usr/bin")],
    );
    assert_eq!(
        vz_memo_key(&k1).0,
        vz_memo_key(&k2).0,
        "same inputs must produce the same vz memo key"
    );
}

// -----------------------------------------------------------------------
// vz_memo_key_is_sensitive_to_every_field: any field change ⇒ a new key.
// Covers command, rootfs_digest, and env (the three key inputs), plus
// env-split ambiguity (the length-prefix must defeat it).
// -----------------------------------------------------------------------
#[test]
fn vz_memo_key_is_sensitive_to_every_field() {
    let base = vz_key(
        vec!["/bin/echo", "hi"],
        [7u8; 32],
        vec![("PATH", "/usr/bin")],
    );
    let base_key = vz_memo_key(&base).0;

    // (a) command arg change
    let diff_cmd = vz_key(
        vec!["/bin/echo", "bye"],
        [7u8; 32],
        vec![("PATH", "/usr/bin")],
    );
    assert_ne!(
        base_key,
        vz_memo_key(&diff_cmd).0,
        "a command change must change the key"
    );

    // (b) command arity change (one arg vs two — length-prefix defeats
    //     "echo"+"hi" colliding with "echohi").
    let diff_arity = vz_key(vec!["/bin/echohi"], [7u8; 32], vec![("PATH", "/usr/bin")]);
    assert_ne!(
        base_key,
        vz_memo_key(&diff_arity).0,
        "argument boundaries must matter (length-prefixed)"
    );

    // (c) rootfs digest change (a different image ⇒ a different run)
    let diff_rootfs = vz_key(
        vec!["/bin/echo", "hi"],
        [8u8; 32],
        vec![("PATH", "/usr/bin")],
    );
    assert_ne!(
        base_key,
        vz_memo_key(&diff_rootfs).0,
        "a rootfs image change must change the key"
    );

    // (d) env value change
    let diff_env_val = vz_key(vec!["/bin/echo", "hi"], [7u8; 32], vec![("PATH", "/bin")]);
    assert_ne!(
        base_key,
        vz_memo_key(&diff_env_val).0,
        "an env value change must change the key"
    );

    // (e) env split ambiguity: ["A=B", "C"] vs ["A", "B=C"] must differ.
    let split1 = vz_key(vec!["/bin/x"], [7u8; 32], vec![("A", "B"), ("CKEY", "V")]);
    let split2 = vz_key(vec!["/bin/x"], [7u8; 32], vec![("A", "BCKEY"), ("", "V")]);
    assert_ne!(
        vz_memo_key(&split1).0,
        vz_memo_key(&split2).0,
        "env entries must be unambiguously delimited (length-prefixed k=v)"
    );
}

// -----------------------------------------------------------------------
// run_vz_memoized_miss_runs_closure_and_stores: first call MISSes, invokes
// the closure, and (exit==0, bounded) caches the result.
// -----------------------------------------------------------------------
#[test]
fn run_vz_memoized_miss_runs_closure_and_stores() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());

    let key = vz_key(vec!["/bin/echo", "hi"], [1u8; 32], vec![("PATH", "/bin")]);

    let mut calls = 0u32;
    let out = run_vz_memoized(&key, &store, || {
        calls += 1;
        Ok((0, b"out-bytes".to_vec(), b"err-bytes".to_vec()))
    })
    .expect("miss run");

    assert_eq!(calls, 1, "closure must be invoked exactly once on a miss");
    assert!(!out.hit, "first run must be a miss");
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout, b"out-bytes");
    assert_eq!(out.stderr, b"err-bytes");

    // The result must now be in the Action Cache (exit==0 + bounded).
    let rec = store
        .ac_get(&vz_memo_key(&key))
        .expect("ac_get")
        .expect("record present after a cacheable miss");
    assert!(
        decode_ac_record(&rec).is_some(),
        "the stored AC record must decode"
    );
}

// -----------------------------------------------------------------------
// run_vz_memoized_hit_replays_without_closure: after a cacheable first
// call, the second call is a HIT that replays {exit, stdout, stderr} from
// the CAS and NEVER invokes the closure (proven with a counter) — NO VM
// boot. This is the "work ceases to exist" thesis.
// -----------------------------------------------------------------------
#[test]
fn run_vz_memoized_hit_replays_without_closure() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());

    let key = vz_key(vec!["/bin/echo", "hi"], [2u8; 32], vec![("PATH", "/bin")]);

    // First call seeds the AC (exit==0, bounded).
    let mut first_calls = 0u32;
    let out1 = run_vz_memoized(&key, &store, || {
        first_calls += 1;
        Ok((0, b"replay-out".to_vec(), b"replay-err".to_vec()))
    })
    .expect("seed run");
    assert_eq!(first_calls, 1);
    assert!(!out1.hit);

    // Second identical call MUST be a hit and MUST NOT invoke the closure.
    let mut second_calls = 0u32;
    let out2 = run_vz_memoized(&key, &store, || {
        second_calls += 1;
        // If this ever runs, return a DIFFERENT result so a regression is
        // loud (a real boot would also differ from the cached replay).
        Ok((123, b"SHOULD-NOT-RUN".to_vec(), b"SHOULD-NOT-RUN".to_vec()))
    })
    .expect("hit run");

    assert_eq!(
        second_calls, 0,
        "the closure must NOT run on a hit (no VM boot)"
    );
    assert!(out2.hit, "second run must be a hit");
    assert_eq!(out2.exit_code, 0, "replayed exit code");
    assert_eq!(out2.stdout, b"replay-out", "stdout replayed byte-exact");
    assert_eq!(out2.stderr, b"replay-err", "stderr replayed byte-exact");
    assert_eq!(out1.key.0, out2.key.0, "same key across the two calls");
}

// -----------------------------------------------------------------------
// run_vz_memoized_nonzero_exit_not_cached: a non-zero exit is never cached
// (mirrors the native exit_nonzero_never_memoized law) ⇒ it re-runs.
// -----------------------------------------------------------------------
#[test]
fn run_vz_memoized_nonzero_exit_not_cached() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());

    let key = vz_key(vec!["/bin/false"], [3u8; 32], vec![("PATH", "/bin")]);

    let mut calls = 0u32;
    let run = |calls: &mut u32| {
        *calls += 1;
        run_vz_memoized(&key, &store, || Ok((7, b"o".to_vec(), b"e".to_vec())))
    };

    let out1 = run(&mut calls).expect("run1");
    assert!(!out1.hit, "first run is a miss");
    assert_eq!(out1.exit_code, 7);

    let out2 = run(&mut calls).expect("run2");
    assert!(
        !out2.hit,
        "a non-zero exit must NOT be cached ⇒ the second run is also a miss"
    );
    assert_eq!(out2.exit_code, 7);

    // Nothing was ever written to the AC for this key.
    assert!(
        store.ac_get(&vz_memo_key(&key)).expect("ac_get").is_none(),
        "a non-zero exit must leave the AC empty"
    );
}

// -----------------------------------------------------------------------
// run_vz_memoized_oversized_output_not_cached: a stdout over OUTPUT_CAP
// bytes is never cached (mirrors the native output_cap_not_memoized law).
// -----------------------------------------------------------------------
#[test]
fn run_vz_memoized_oversized_output_not_cached() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());

    let key = vz_key(vec!["/bin/yes"], [4u8; 32], vec![("PATH", "/bin")]);
    let big = vec![b'x'; OUTPUT_CAP_BYTES + 1];

    let out1 = run_vz_memoized(&key, &store, {
        let big = big.clone();
        || Ok((0, big, Vec::new()))
    })
    .expect("run1");
    assert!(!out1.hit);
    assert_eq!(out1.exit_code, 0);

    // Over-cap ⇒ not cached ⇒ the next call is still a miss.
    assert!(
        store.ac_get(&vz_memo_key(&key)).expect("ac_get").is_none(),
        "an over-cap stdout must not be cached"
    );
}
