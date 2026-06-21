//! WP-DF-06 KEY-layer tests (split from `memo_tests.rs` for the godfile cap):
//! `--chown`/`--chmod` are folded into the COPY key, so the SAME bytes under
//! different flags key DIFFERENTLY (no false memo hit) and identical flags key
//! identically (a deterministic hit). Sibling `#[path]` module of `memo`.
use super::*;
use tempfile::TempDir;

/// A `ContextKey` over `dir` with an empty `.dockerignore` matcher (WP-DF-IGNORE)
/// — these key tests are unaffected by ignore semantics (byte-identical keys).
fn ck(dir: &std::path::Path) -> ContextKey<'_> {
    ContextKey {
        context_dir: dir,
        ignore: Box::leak(Box::<DockerIgnore>::default()),
    }
}

/// The default active SHELL for keying tests (Docker's `["/bin/sh","-c"]`).
fn dsh() -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string()]
}

/// A COPY step over a single context file `f.txt`, with the given flags.
fn copy_step(chown: Option<&str>, chmod: Option<&str>) -> BuildStep {
    let mut raw = String::from("COPY ");
    if let Some(c) = chown {
        raw.push_str(&format!("--chown={c} "));
    }
    if let Some(c) = chmod {
        raw.push_str(&format!("--chmod={c} "));
    }
    raw.push_str("f.txt /f.txt");
    BuildStep {
        instr: Instr::Copy {
            src: vec!["f.txt".to_string()],
            dest: "/f.txt".to_string(),
            from: None,
            chown: chown.map(str::to_string),
            chmod: chmod.map(str::to_string),
        },
        raw,
    }
}

#[test]
fn copy_different_chmod_differs_key_no_false_hit() {
    // CORE WP-DF-06 invariant: the SAME COPY of the SAME bytes keyed under
    // --chmod=0644 vs 0600 must produce DIFFERENT keys — else the 0600 build
    // would reuse the 0644 layer (FALSE memo hit, wrong file mode).
    let ctx = TempDir::new().unwrap();
    std::fs::write(ctx.path().join("f.txt"), b"data").unwrap();
    let s = VarScope::default();
    let dsh = dsh();
    let k644 = step_key(
        None,
        &copy_step(None, Some("0644")),
        ck(ctx.path()),
        &s,
        true,
        &dsh,
        None,
    )
    .unwrap();
    let k600 = step_key(
        None,
        &copy_step(None, Some("0600")),
        ck(ctx.path()),
        &s,
        true,
        &dsh,
        None,
    )
    .unwrap();
    assert_ne!(
        k644.0, k600.0,
        "different --chmod must yield a different COPY key (no false hit)"
    );
}

#[test]
fn copy_different_chown_differs_key_no_false_hit() {
    // The SAME COPY of the SAME bytes keyed under --chown=0:0 vs 1000:1000 must
    // differ — different ownership is a different output layer.
    let ctx = TempDir::new().unwrap();
    std::fs::write(ctx.path().join("f.txt"), b"data").unwrap();
    let s = VarScope::default();
    let dsh = dsh();
    let k_root = step_key(
        None,
        &copy_step(Some("0:0"), None),
        ck(ctx.path()),
        &s,
        true,
        &dsh,
        None,
    )
    .unwrap();
    let k_user = step_key(
        None,
        &copy_step(Some("1000:1000"), None),
        ck(ctx.path()),
        &s,
        true,
        &dsh,
        None,
    )
    .unwrap();
    assert_ne!(
        k_root.0, k_user.0,
        "different --chown must yield a different COPY key (no false hit)"
    );
}

#[test]
fn copy_same_flags_same_key_memo_hit() {
    // Identical COPY + identical flags ⇒ identical key ⇒ memo HIT (deterministic).
    let ctx = TempDir::new().unwrap();
    std::fs::write(ctx.path().join("f.txt"), b"data").unwrap();
    let s = VarScope::default();
    let dsh = dsh();
    let k1 = step_key(
        None,
        &copy_step(Some("1000:1000"), Some("0640")),
        ck(ctx.path()),
        &s,
        true,
        &dsh,
        None,
    )
    .unwrap();
    let k2 = step_key(
        None,
        &copy_step(Some("1000:1000"), Some("0640")),
        ck(ctx.path()),
        &s,
        true,
        &dsh,
        None,
    )
    .unwrap();
    assert_eq!(
        k1.0, k2.0,
        "identical COPY flags must yield an identical key"
    );
}

#[test]
fn copy_adding_chmod_differs_from_flagless() {
    // Adding a --chmod where there was none must change the key (the flagless
    // layer has no enforced mode; the flagged one does — different output).
    let ctx = TempDir::new().unwrap();
    std::fs::write(ctx.path().join("f.txt"), b"data").unwrap();
    let s = VarScope::default();
    let dsh = dsh();
    let k_plain = step_key(
        None,
        &copy_step(None, None),
        ck(ctx.path()),
        &s,
        true,
        &dsh,
        None,
    )
    .unwrap();
    let k_mode = step_key(
        None,
        &copy_step(None, Some("0644")),
        ck(ctx.path()),
        &s,
        true,
        &dsh,
        None,
    )
    .unwrap();
    assert_ne!(
        k_plain.0, k_mode.0,
        "adding --chmod must change the COPY key vs a flagless COPY"
    );
}
