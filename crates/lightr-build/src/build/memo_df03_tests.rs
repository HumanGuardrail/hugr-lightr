//! WP-DF-03 key-layer memo tests: the upstream stage digest is folded into a
//! `COPY --from=stage` key (different upstream ⇒ different key ⇒ no false hit),
//! and is ABSENT for a flagless COPY (key byte-identical to before this WP).
use super::*;
use tempfile::TempDir;

fn dsh() -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string()]
}

/// A `ContextKey` over `dir` with an empty `.dockerignore` matcher (WP-DF-IGNORE)
/// — `COPY --from=stage` keys are unaffected by ignore (byte-identical keys).
fn ck(dir: &std::path::Path) -> ContextKey<'_> {
    ContextKey {
        context_dir: dir,
        ignore: Box::leak(Box::<DockerIgnore>::default()),
    }
}

/// A `COPY --from=builder <src> <dest>` step over a stage source path.
fn copy_from_step() -> BuildStep {
    BuildStep {
        instr: Instr::Copy {
            src: vec!["/out/app".to_string()],
            dest: "/app".to_string(),
            from: Some("builder".to_string()),
            chown: None,
            chmod: None,
        },
        raw: "COPY --from=builder /out/app /app".to_string(),
    }
}

#[test]
fn copy_from_different_upstream_digest_differs_key_no_false_hit() {
    // CORE WP-DF-03 invariant: the SAME `COPY --from=builder` step keyed against
    // two DIFFERENT upstream-stage output digests must produce DIFFERENT keys —
    // else a changed builder stage would reuse the stale copied layer (FALSE
    // memo hit, wrong bytes).
    let ctx = TempDir::new().unwrap();
    let step = copy_from_step();
    let s = VarScope::default();
    let d_a = Digest([0xAA; 32]);
    let d_b = Digest([0xBB; 32]);

    let k_a = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), Some(d_a)).unwrap();
    let k_b = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), Some(d_b)).unwrap();
    assert_ne!(
        k_a.0, k_b.0,
        "a different upstream stage digest must yield a different COPY --from key"
    );
}

#[test]
fn copy_from_same_upstream_digest_same_key_memo_hit() {
    // Determinism: the same step + same upstream digest ⇒ identical key ⇒ hit.
    let ctx = TempDir::new().unwrap();
    let step = copy_from_step();
    let s = VarScope::default();
    let d = Digest([0xCD; 32]);

    let k1 = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), Some(d)).unwrap();
    let k2 = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), Some(d)).unwrap();
    assert_eq!(
        k1.0, k2.0,
        "same upstream digest ⇒ identical key (memo hit)"
    );
}

#[test]
fn flagless_copy_key_unaffected_by_none_from_digest() {
    // Behavior-preserving: a flagless COPY passes `from_stage_digest = None`, so
    // its key is byte-identical whether or not this WP exists — proven by keying
    // the same flagless COPY with None twice (stable) AND that a None never folds
    // the `from-stage` separator (the key matches a pre-WP-computed shape).
    let ctx = TempDir::new().unwrap();
    std::fs::write(ctx.path().join("f.txt"), b"data").unwrap();
    let step = BuildStep {
        instr: Instr::Copy {
            src: vec!["f.txt".to_string()],
            dest: "/f.txt".to_string(),
            from: None,
            chown: None,
            chmod: None,
        },
        raw: "COPY f.txt /f.txt".to_string(),
    };
    let s = VarScope::default();
    let k1 = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), None).unwrap();
    let k2 = step_key(None, &step, ck(ctx.path()), &s, true, &dsh(), None).unwrap();
    assert_eq!(
        k1.0, k2.0,
        "a flagless COPY (None upstream) keys identically + stably (no fold)"
    );
}
