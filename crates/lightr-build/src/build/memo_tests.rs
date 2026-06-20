use super::*;
use tempfile::TempDir;

// A scope from (arg, env) pairs, for keying tests.
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

fn run_step(raw: &str) -> BuildStep {
    // A SHELL-form RUN step (the key folds the active shell for these);
    // self-contained for the keying tests.
    BuildStep {
        instr: Instr::Run {
            argv: vec!["/bin/sh".into(), "-c".into(), raw.into()],
            form: CmdForm::Shell(raw.into()),
        },
        raw: raw.to_string(),
    }
}

fn run_step_exec(raw: &str, argv: &[&str]) -> BuildStep {
    // An EXEC-form RUN step (the key does NOT fold the active shell).
    BuildStep {
        instr: Instr::Run {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            form: CmdForm::Exec(argv.iter().map(|s| s.to_string()).collect()),
        },
        raw: raw.to_string(),
    }
}

/// The default active SHELL for keying tests (Docker's `["/bin/sh","-c"]`).
fn dsh() -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string()]
}

#[test]
fn step_key_dir_copy_changes_when_contained_file_changes() {
    // `COPY src/ /app` must invalidate the cache when a file INSIDE src/
    // changes -- not just top-level files.
    // NOTE: step_key now takes (scope, escape) — WP-DF-BUILDKEY. An empty
    // scope leaves the COPY text verbatim, so this content-fingerprint
    // assertion is unchanged in meaning (only the v2 tag changed the bytes).
    let ctx = TempDir::new().unwrap();
    std::fs::create_dir_all(ctx.path().join("src/nested")).unwrap();
    std::fs::write(ctx.path().join("src/a.txt"), b"one").unwrap();
    std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-one").unwrap();

    let step = BuildStep {
        instr: Instr::Copy {
            src: vec!["src".to_string()],
            dest: "/app".to_string(),
            from: None,
            chown: None,
            chmod: None,
        },
        raw: "COPY src /app".to_string(),
    };
    let s = VarScope::default();

    let k1 = step_key(None, &step, ctx.path(), &s, true, &dsh()).unwrap();

    // change a NESTED file
    std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-two").unwrap();
    let k2 = step_key(None, &step, ctx.path(), &s, true, &dsh()).unwrap();
    assert_ne!(
        k1.0, k2.0,
        "nested file change must change the COPY step key"
    );

    // adding a file changes the key too
    std::fs::write(ctx.path().join("src/c.txt"), b"new").unwrap();
    let k3 = step_key(None, &step, ctx.path(), &s, true, &dsh()).unwrap();
    assert_ne!(k2.0, k3.0, "adding a file must change the COPY step key");

    // identical content => identical key (determinism)
    std::fs::remove_file(ctx.path().join("src/c.txt")).unwrap();
    std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-one").unwrap();
    let k4 = step_key(None, &step, ctx.path(), &s, true, &dsh()).unwrap();
    assert_eq!(k1.0, k4.0, "restoring content must restore the key");
}

// ---- WP-DF-BUILDKEY: MEMO-CORRECTNESS at the key layer ----

#[test]
fn interp_var_value_change_changes_key_no_false_hit() {
    // The SAME raw instruction `RUN echo ${X}` keyed under X=A vs X=B must
    // produce DIFFERENT keys — else B would reuse A's cached layer (silent
    // WRONG build). This is the core memoization-correctness invariant.
    let ctx = TempDir::new().unwrap();
    let step = run_step("RUN echo ${X}");

    let sa = scope(&[], &[("X", "alpha")]);
    let sb = scope(&[], &[("X", "beta")]);

    let ka = step_key(None, &step, ctx.path(), &sa, true, &dsh()).unwrap();
    let kb = step_key(None, &step, ctx.path(), &sb, true, &dsh()).unwrap();
    assert_ne!(
        ka.0, kb.0,
        "differing ${{X}} values must yield differing keys (no false memo hit)"
    );
}

#[test]
fn interp_same_inputs_same_key_memo_hit() {
    // Identical (instruction, scope) ⇒ identical key ⇒ memo HIT.
    let ctx = TempDir::new().unwrap();
    let step = run_step("RUN echo ${X}-${Y}");
    let s = scope(&[("Y", "two")], &[("X", "one")]);

    let k1 = step_key(None, &step, ctx.path(), &s, true, &dsh()).unwrap();
    let k2 = step_key(None, &step, ctx.path(), &s, true, &dsh()).unwrap();
    assert_eq!(k1.0, k2.0, "identical inputs must yield an identical key");
}

#[test]
fn no_var_dockerfile_key_is_stable() {
    // A line with no `${VAR}` keys identically regardless of scope, and is
    // stable across runs (v2). Behavior-preserving modulo the v1→v2 bump.
    let ctx = TempDir::new().unwrap();
    let step = run_step("RUN echo hello");
    let empty = VarScope::default();
    let populated = scope(&[("X", "v")], &[("Y", "w")]);

    let k1 = step_key(None, &step, ctx.path(), &empty, true, &dsh()).unwrap();
    let k2 = step_key(None, &step, ctx.path(), &empty, true, &dsh()).unwrap();
    let k3 = step_key(None, &step, ctx.path(), &populated, true, &dsh()).unwrap();
    assert_eq!(k1.0, k2.0, "no-var key must be stable across runs");
    assert_eq!(k1.0, k3.0, "no-var key must not depend on scope contents");
}

#[test]
fn v2_domain_tag_in_key() {
    // Document + lock the one-time invalidation: the domain tag is v2.
    assert_eq!(BUILD_KEY_DOMAIN, b"lightr/build/v2");
}

// ---- WP-DF-09: the active SHELL is folded into the SHELL-form RUN key ----

#[test]
fn shell_form_run_different_shell_differs_key_no_false_hit() {
    // CORE WP-DF-09 invariant: the SAME shell-form `RUN echo hi` keyed under
    // `["/bin/sh","-c"]` vs `["/bin/bash","-c"]` must produce DIFFERENT keys —
    // else the bash build would reuse the sh layer (FALSE memo hit, wrong
    // interpreter). The RUN text is identical; only the active SHELL differs.
    let ctx = TempDir::new().unwrap();
    let step = run_step("RUN echo hi");
    let s = VarScope::default();
    let sh = vec!["/bin/sh".to_string(), "-c".to_string()];
    let bash = vec!["/bin/bash".to_string(), "-c".to_string()];

    let k_sh = step_key(None, &step, ctx.path(), &s, true, &sh).unwrap();
    let k_bash = step_key(None, &step, ctx.path(), &s, true, &bash).unwrap();
    assert_ne!(
        k_sh.0, k_bash.0,
        "different active SHELL must yield a different shell-form RUN key (no false hit)"
    );
}

#[test]
fn shell_form_run_same_shell_same_key_memo_hit() {
    // Identical SHELL + identical RUN text ⇒ identical key ⇒ memo HIT.
    let ctx = TempDir::new().unwrap();
    let step = run_step("RUN echo hi");
    let s = VarScope::default();
    let bash = vec!["/bin/bash".to_string(), "-c".to_string()];

    let k1 = step_key(None, &step, ctx.path(), &s, true, &bash).unwrap();
    let k2 = step_key(None, &step, ctx.path(), &s, true, &bash).unwrap();
    assert_eq!(k1.0, k2.0, "same SHELL + same RUN must be a memo hit");
}

#[test]
fn exec_form_run_ignores_active_shell_in_key() {
    // Docker: exec-form `RUN ["echo","hi"]` is NOT wrapped by SHELL, so the
    // active SHELL must NOT enter its key (different SHELL ⇒ SAME key — no
    // needless cache bust). Behavior-faithful + cache-efficient.
    let ctx = TempDir::new().unwrap();
    let step = run_step_exec("RUN [\"echo\",\"hi\"]", &["echo", "hi"]);
    let s = VarScope::default();
    let sh = vec!["/bin/sh".to_string(), "-c".to_string()];
    let bash = vec!["/bin/bash".to_string(), "-c".to_string()];

    let k_sh = step_key(None, &step, ctx.path(), &s, true, &sh).unwrap();
    let k_bash = step_key(None, &step, ctx.path(), &s, true, &bash).unwrap();
    assert_eq!(
        k_sh.0, k_bash.0,
        "exec-form RUN key must NOT depend on the active SHELL"
    );
}

#[test]
fn non_run_instruction_key_is_unchanged_by_shell() {
    // Behavior-preserving: a non-RUN instruction (COPY here) never folds the
    // active SHELL, so its key is byte-identical regardless of current_shell —
    // proving the WP touches ONLY the RUN-key contribution.
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
    let sh = vec!["/bin/sh".to_string(), "-c".to_string()];
    let bash = vec!["/bin/bash".to_string(), "-c".to_string()];

    let k_sh = step_key(None, &step, ctx.path(), &s, true, &sh).unwrap();
    let k_bash = step_key(None, &step, ctx.path(), &s, true, &bash).unwrap();
    assert_eq!(
        k_sh.0, k_bash.0,
        "non-RUN instruction key must not depend on the active SHELL"
    );
}
