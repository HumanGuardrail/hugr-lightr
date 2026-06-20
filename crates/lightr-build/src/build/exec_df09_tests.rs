//! WP-DF-09 end-to-end tests: the SHELL instruction, exercised through the full
//! `build()` loop. Split out of `exec_tests.rs` for the 400-line godfile cap.
//!
//! Parallel-safe by construction: each test owns its tempdirs + store and never
//! mutates process-global state (no `LIGHTR_HOME`, no shared mutex) — `build()`
//! takes the store explicitly and uses a nanos-unique temp work dir.
//!
//! "The active SHELL is actually used" is proven WITHOUT relying on a specific
//! interpreter being installed: we drop an executable wrapper script into the
//! context dir and point SHELL at it. When SHELL wraps a shell-form RUN, the
//! wrapper runs and appends a marker; when it does NOT (exec form, or after a
//! FROM reset), the marker is absent. This makes the assertion deterministic on
//! any box with `/bin/sh`.
use super::*;
use tempfile::TempDir;

/// Self-contained fixture: own context dir, own store, own counter file, plus
/// the wrapper-shell path + a second counter the wrapper writes to.
struct Fix {
    _ctx: TempDir,
    _store_tmp: TempDir,
    store: Store,
    counter: std::path::PathBuf,
    marker: std::path::PathBuf,
    shell_path: std::path::PathBuf,
    ctx_path: std::path::PathBuf,
}

fn fix() -> Fix {
    let _ctx = TempDir::new().unwrap();
    let _store_tmp = TempDir::new().unwrap();
    let store = Store::open(_store_tmp.path().join("store")).unwrap();
    let counter = _store_tmp.path().join("counter.txt");
    let marker = _store_tmp.path().join("marker.txt");
    let ctx_path = _ctx.path().to_path_buf();
    // A wrapper "shell": `<shell> -c <cmd>` appends a marker, then runs <cmd>
    // through /bin/sh so the build's own side effects still happen and it exits 0.
    let shell_path = _store_tmp.path().join("myshell.sh");
    let script = format!(
        "#!/bin/sh\necho used-wrapper >> {m}\n# args: $1 = -c, $2 = the command\nexec /bin/sh \"$@\"\n",
        m = marker.to_string_lossy()
    );
    std::fs::write(&shell_path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&shell_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shell_path, perms).unwrap();
    }
    Fix {
        _ctx,
        _store_tmp,
        store,
        counter,
        marker,
        shell_path,
        ctx_path,
    }
}

/// Write `df_body` (`{CF}` → counter, `{SH}` → wrapper shell path) and build.
fn run(f: &Fix, name: &str, df_body: &str) -> BuildReport {
    let df = df_body
        .replace("{CF}", &f.counter.to_string_lossy())
        .replace("{SH}", &f.shell_path.to_string_lossy());
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(&df_path, &df).unwrap();
    build(
        &f.ctx_path,
        &df_path,
        name,
        lightr_engine::EngineKind::Native,
        &f.store,
        &[],
    )
    .unwrap()
}

fn read(p: &std::path::Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[test]
fn default_shell_runs_shell_form_run() {
    // Behavior-preserving: with NO SHELL instruction, a shell-form RUN executes
    // under the default `/bin/sh -c` and its side effect happens. The wrapper is
    // NOT involved, so its marker stays empty.
    let f = fix();
    run(&f, "df09-default", "FROM scratch\nRUN echo hi >> {CF}\n");
    assert_eq!(read(&f.counter), "hi\n", "default sh -c must run the RUN");
    assert_eq!(read(&f.marker), "", "wrapper must NOT run without SHELL");
}

#[test]
fn shell_instruction_is_used_by_subsequent_shell_form_run() {
    // SHELL sets the interpreter for subsequent shell-form RUN. The wrapper runs
    // (marker written) AND the RUN's own side effect still happens (counter).
    let f = fix();
    run(
        &f,
        "df09-set",
        "FROM scratch\nSHELL [\"{SH}\",\"-c\"]\nRUN echo hi >> {CF}\n",
    );
    assert_eq!(
        read(&f.counter),
        "hi\n",
        "RUN side effect must still happen"
    );
    assert_eq!(
        read(&f.marker),
        "used-wrapper\n",
        "the active SHELL (wrapper) must wrap the shell-form RUN"
    );
}

#[test]
fn exec_form_run_ignores_active_shell() {
    // Docker: exec-form `RUN ["a","b"]` is NOT wrapped by SHELL. Even with the
    // wrapper set as SHELL, an exec-form RUN must NOT invoke it (no marker).
    let f = fix();
    run(
        &f,
        "df09-exec",
        "FROM scratch\nSHELL [\"{SH}\",\"-c\"]\nRUN [\"/bin/sh\",\"-c\",\"echo hi >> {CF}\"]\n",
    );
    assert_eq!(read(&f.counter), "hi\n", "exec-form RUN must still run");
    assert_eq!(
        read(&f.marker),
        "",
        "exec-form RUN must NOT be wrapped by the active SHELL"
    );
}

#[test]
fn shell_resets_at_from() {
    // SHELL state is per-stage: a new FROM resets it to the default. Stage 1 sets
    // the wrapper + uses it; stage 2 (after FROM) must fall back to `/bin/sh`, so
    // the wrapper marker is written ONCE (stage 1 only), not twice.
    let f = fix();
    run(
        &f,
        "df09-reset",
        "FROM scratch\nSHELL [\"{SH}\",\"-c\"]\nRUN echo a >> {CF}\n\
         FROM scratch\nRUN echo b >> {CF}\n",
    );
    assert_eq!(read(&f.counter), "a\nb\n", "both RUNs must execute");
    assert_eq!(
        read(&f.marker),
        "used-wrapper\n",
        "wrapper must run for stage 1 only; FROM resets SHELL to /bin/sh"
    );
}

#[test]
fn different_shell_busts_memo_no_false_hit() {
    // CORE WP-DF-09 memo invariant, end-to-end: the SAME shell-form RUN text
    // built first under the default shell, then under the wrapper SHELL, must
    // NOT reuse the first layer — the active SHELL is folded into the RUN key, so
    // the RUN re-executes (proven by the counter gaining a second line).
    let f = fix();

    // Build 1: default shell, shell-form RUN.
    let r1 = run(&f, "df09-memo", "FROM scratch\nRUN echo go >> {CF}\n");
    assert_eq!(r1.cached_steps, 0, "cold build");
    assert_eq!(read(&f.counter), "go\n");

    // Build 2 against the SAME store: identical RUN text but SHELL now differs.
    // Different SHELL ⇒ different RUN key ⇒ NO false hit ⇒ RUN runs again.
    let r2 = run(
        &f,
        "df09-memo",
        "FROM scratch\nSHELL [\"{SH}\",\"-c\"]\nRUN echo go >> {CF}\n",
    );
    assert!(
        r2.cached_steps < r2.steps,
        "the RUN under a different SHELL must NOT be a full cache hit"
    );
    assert_eq!(
        read(&f.counter),
        "go\ngo\n",
        "different SHELL must re-run the identical RUN (no false memo hit)"
    );
    assert_eq!(
        read(&f.marker),
        "used-wrapper\n",
        "build 2's RUN ran under the wrapper SHELL"
    );
}

#[test]
fn identical_shell_and_run_is_memo_hit() {
    // Identical SHELL + identical RUN text rebuilt against the same store ⇒ full
    // memo hit (the SHELL fold is deterministic, not a blanket cache-buster).
    let f = fix();
    let df = "FROM scratch\nSHELL [\"{SH}\",\"-c\"]\nRUN echo once >> {CF}\n";
    let r1 = run(&f, "df09-hit", df);
    assert_eq!(r1.cached_steps, 0, "cold build");
    let r2 = run(&f, "df09-hit", df);
    assert_eq!(
        r2.cached_steps, r2.steps,
        "identical SHELL + RUN ⇒ every step is a memo hit"
    );
    assert_eq!(
        read(&f.counter),
        "once\n",
        "RUN must NOT re-run on an identical-input rebuild"
    );
}
