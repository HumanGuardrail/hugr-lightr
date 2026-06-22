//! WP-FIX73 (#73): `step_key` must INTERPOLATE a COPY/ADD source token before
//! resolving + hashing its content, mirroring the executor
//! (`exec_instr::copy` → `resolve_sources_in`, which `interp_vec`s `src` before
//! joining to the context). Sibling test file (godfile cap on `memo.rs`).
//!
//! ## The bug these tests pin (negative control)
//!
//! Before the fix, the loop hashed `context_dir.join(s)` with the RAW token `s`
//! (e.g. the literal `"${DIR}"`), joining to a nonexistent
//! `context_dir/${DIR}` ⇒ `hash_copy_source_filtered` folded the
//! `\x00missing\x00` sentinel — a CONSTANT for EVERY value of `${DIR}` and for
//! every file content under the resolved dir. So a `COPY ${DIR}/ /out` built
//! twice with the SAME `DIR` ARG but DIFFERENT files under that dir produced the
//! IDENTICAL key ⇒ a FALSE cache hit ⇒ the 2nd build reused the 1st's layer
//! (wrong output). `interp_src_change_changes_key_no_false_hit` below is RED
//! under that old code (the keys were equal) and GREEN after the fix.
use super::*;
use tempfile::TempDir;

// A scope from (arg, env) pairs (mirrors memo_tests.rs::scope).
fn scope(args: &[(&str, &str)], envs: &[(&str, &str)]) -> VarScope {
    VarScope {
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        env: envs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

fn dsh() -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string()]
}

fn di() -> &'static DockerIgnore {
    Box::leak(Box::<DockerIgnore>::default())
}

fn ck(dir: &std::path::Path) -> ContextKey<'_> {
    ContextKey {
        context_dir: dir,
        ignore: di(),
    }
}

/// A COPY step whose single source is `src_tok` (e.g. `"${DIR}"`), keeping the
/// raw text in sync so `canonical_step_text` is realistic.
fn copy_step(src_tok: &str) -> BuildStep {
    BuildStep {
        instr: Instr::Copy {
            src: vec![src_tok.to_string()],
            dest: "/out".to_string(),
            from: None,
            chown: None,
            chmod: None,
        },
        raw: format!("COPY {src_tok} /out"),
    }
}

#[test]
fn interp_src_change_changes_key_no_false_hit() {
    // THE #73 PROOF. `COPY ${DIR} /out` under a FIXED scope (DIR=mydir), keyed
    // twice against a context where mydir/'s FILES differ between runs, MUST
    // yield DIFFERENT keys — the source content differs, so the layer differs.
    //
    // Negative control: with the OLD raw-token code both keys were EQUAL (the
    // hashed path was the nonexistent `context_dir/${DIR}` ⇒ constant
    // `\x00missing\x00` sentinel regardless of the real files) — a FALSE memo
    // hit. This assertion is RED-without-fix / GREEN-with-fix.
    let ctx = TempDir::new().unwrap();
    std::fs::create_dir_all(ctx.path().join("mydir")).unwrap();
    std::fs::write(ctx.path().join("mydir/a.txt"), b"one").unwrap();

    let step = copy_step("${DIR}");
    let s = scope(&[("DIR", "mydir")], &[]);

    let k1 = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), None, "").unwrap();

    // Same DIR value, but the FILES under mydir/ change between builds.
    std::fs::write(ctx.path().join("mydir/a.txt"), b"two").unwrap();
    let k2 = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), None, "").unwrap();

    assert_ne!(
        k1.0, k2.0,
        "interpolated COPY source content change must change the key (no false cache hit, #73)"
    );

    // And restoring the exact bytes restores the key (determinism — proves it is
    // really hashing the resolved content, not nondeterministic noise).
    std::fs::write(ctx.path().join("mydir/a.txt"), b"one").unwrap();
    let k3 = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), None, "").unwrap();
    assert_eq!(k1.0, k3.0, "restoring content must restore the key");
}

#[test]
fn interp_src_resolves_to_real_path_matches_literal_key() {
    // Positive proof the fix hashes the RESOLVED path: `COPY ${DIR} /out` with
    // DIR=mydir must key to the SAME source-content fold as the equivalent
    // literal `COPY mydir /out`. (Keys differ in CANONICAL TEXT — `${DIR}` vs
    // `mydir` interpolate identically to `mydir`, so the whole keys match.)
    let ctx = TempDir::new().unwrap();
    std::fs::create_dir_all(ctx.path().join("mydir")).unwrap();
    std::fs::write(ctx.path().join("mydir/a.txt"), b"payload").unwrap();

    let interp_step = copy_step("${DIR}");
    let s = scope(&[("DIR", "mydir")], &[]);
    let k_interp = step_key(
        None,
        &interp_step,
        ck(ctx.path()),
        &s,
        true,
        &dsh(),
        None,
        "",
    )
    .unwrap();

    // A literal `COPY mydir /out` whose raw text ALSO interpolates to `mydir`.
    let literal_step = BuildStep {
        instr: Instr::Copy {
            src: vec!["mydir".to_string()],
            dest: "/out".to_string(),
            from: None,
            chown: None,
            chmod: None,
        },
        raw: "COPY mydir /out".to_string(),
    };
    let k_literal = step_key(
        None,
        &literal_step,
        ck(ctx.path()),
        &s,
        true,
        &dsh(),
        None,
        "",
    )
    .unwrap();

    assert_eq!(
        k_interp.0, k_literal.0,
        "an interpolated ${{DIR}} (=mydir) source must key identically to the literal `mydir`"
    );
}

#[test]
fn literal_source_key_unchanged_no_regression() {
    // No cache-bust of existing CORRECT keys: a plain `COPY foo /out` (no
    // `${VAR}`) interpolates to itself verbatim, so its key is byte-IDENTICAL
    // to the pre-fix key. We pin the exact bytes so any future drift trips.
    let ctx = TempDir::new().unwrap();
    std::fs::write(ctx.path().join("foo"), b"literal-bytes").unwrap();

    let step = BuildStep {
        instr: Instr::Copy {
            src: vec!["foo".to_string()],
            dest: "/out".to_string(),
            from: None,
            chown: None,
            chmod: None,
        },
        raw: "COPY foo /out".to_string(),
    };
    // A populated scope must NOT affect a no-`${VAR}` literal source key.
    let s = scope(&[("DIR", "mydir")], &[("X", "y")]);

    let k = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), None, "").unwrap();

    // Golden bytes: this key is produced by interpolating `foo` -> `foo`
    // (identity) and hashing the file content of `context_dir/foo`. The fix is a
    // no-op for literal sources, so these bytes equal the pre-fix bytes. (Stable
    // because: empty prev-root, empty platform, identity-interpolated text +
    // source, no chown/chmod/from-stage.) Computed once and locked.
    let golden = "b80ff068f2b5683e5348c2b217d84063f5460c28dc77e42a9fe1f21929789e3b";
    assert_eq!(
        hex32(&k.0),
        golden,
        "literal `COPY foo /out` key must be byte-identical to the pre-fix key (no regression)"
    );
}

/// Hex-encode a 32-byte digest (test-local; no external dep).
fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}
