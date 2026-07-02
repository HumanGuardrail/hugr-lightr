//! WP-DF-IGNORE end-to-end tests: `.dockerignore` excludes paths from the build
//! context so `COPY . /dst` (+ globbed COPY) never sees them, `!pattern`
//! re-includes, a `dir/` pattern drops a subtree, comment/blank lines are
//! ignored, no `.dockerignore` ⇒ all files copied (unchanged), AND the memo key
//! does NOT change when an IGNORED file is added (the cache is not busted).
//!
//! Exercised through the full `build()` loop + a hydrate of the final tree.
//! Each test owns its tempdirs + store and never MUTATES process-global state,
//! but `build()`/`hydrate` READ the process-global `LIGHTR_HOME`, so the
//! `build_df`/`hydrate` helpers hold the crate-wide shared read lock
//! (`build::LIGHTR_HOME_ENV_LOCK`) to exclude the setter tests
//! (exec_tests/up_tests) while they run (mirrors `exec_df06_tests.rs`).
use super::*;
use tempfile::TempDir;

struct Fix {
    _ctx: TempDir,
    _store_tmp: TempDir,
    store: Store,
    ctx_path: std::path::PathBuf,
    store_tmp_path: std::path::PathBuf,
}

fn fix() -> Fix {
    let _ctx = TempDir::new().unwrap();
    let _store_tmp = TempDir::new().unwrap();
    let store = Store::open(_store_tmp.path().join("store")).unwrap();
    let ctx_path = _ctx.path().to_path_buf();
    let store_tmp_path = _store_tmp.path().to_path_buf();
    Fix {
        _ctx,
        _store_tmp,
        store,
        ctx_path,
        store_tmp_path,
    }
}

fn build_df(f: &Fix, name: &str, df_body: &str) -> Result<BuildReport> {
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(&df_path, df_body).unwrap();
    // build() READs the process-global LIGHTR_HOME; hold the crate-wide shared
    // read lock so a concurrent setter cannot flip the home mid-build.
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
    build(
        &f.ctx_path,
        &df_path,
        name,
        lightr_engine::EngineKind::Native,
        &f.store,
        &[],
    )
}

fn hydrate(f: &Fix, name: &str, tag: &str) -> std::path::PathBuf {
    let dest = f.store_tmp_path.join(format!("hydrated-{tag}"));
    // hydrate READs the process-global LIGHTR_HOME; hold the shared read lock.
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
    lightr_index::hydrate(&dest, &f.store, name).unwrap();
    dest
}

fn write(f: &Fix, rel: &str, body: &[u8]) {
    let p = f.ctx_path.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

#[test]
fn star_log_excluded_from_copy_dot() {
    // `*.log` in `.dockerignore` ⇒ `COPY . /app` must NOT copy any .log file.
    let f = fix();
    write(&f, ".dockerignore", b"*.log\n");
    write(&f, "app.txt", b"keep");
    write(&f, "debug.log", b"drop");
    write(&f, "error.log", b"drop");
    build_df(&f, "di-star", "FROM scratch\nCOPY . /app\n").unwrap();
    let dest = hydrate(&f, "di-star", "star");
    assert!(dest.join("app/app.txt").exists(), ".txt must be copied");
    assert!(
        !dest.join("app/debug.log").exists(),
        ".log must be excluded"
    );
    assert!(
        !dest.join("app/error.log").exists(),
        ".log must be excluded"
    );
}

#[test]
fn bang_reincludes_a_specific_file() {
    // `*.log` then `!keep.log` ⇒ keep.log survives, others are excluded.
    let f = fix();
    write(&f, ".dockerignore", b"*.log\n!keep.log\n");
    write(&f, "a.log", b"drop");
    write(&f, "keep.log", b"keep");
    build_df(&f, "di-bang", "FROM scratch\nCOPY . /app\n").unwrap();
    let dest = hydrate(&f, "di-bang", "bang");
    assert!(!dest.join("app/a.log").exists(), "a.log excluded");
    assert!(dest.join("app/keep.log").exists(), "keep.log re-included");
}

#[test]
fn dir_pattern_excludes_subtree() {
    // `node_modules/` ⇒ the whole dir + contents are excluded from COPY ..
    let f = fix();
    write(&f, ".dockerignore", b"node_modules/\n");
    write(&f, "src/main.rs", b"fn main(){}");
    write(&f, "node_modules/pkg/index.js", b"module.exports={}");
    write(&f, "node_modules/.bin/tool", b"#!/bin/sh");
    build_df(&f, "di-dir", "FROM scratch\nCOPY . /app\n").unwrap();
    let dest = hydrate(&f, "di-dir", "dir");
    assert!(dest.join("app/src/main.rs").exists(), "src kept");
    assert!(
        !dest.join("app/node_modules").exists(),
        "node_modules dir excluded entirely"
    );
}

#[test]
fn comments_and_blanks_ignored() {
    // `#` comments + blank lines parse to nothing; only `*.tmp` is active.
    let f = fix();
    write(&f, ".dockerignore", b"# a comment\n\n   \n*.tmp\n");
    write(&f, "keep.txt", b"keep");
    write(&f, "scratch.tmp", b"drop");
    build_df(&f, "di-comment", "FROM scratch\nCOPY . /app\n").unwrap();
    let dest = hydrate(&f, "di-comment", "comment");
    assert!(dest.join("app/keep.txt").exists());
    assert!(!dest.join("app/scratch.tmp").exists());
}

#[test]
fn no_dockerignore_copies_everything() {
    // No `.dockerignore` ⇒ behavior-preserving: every file (incl .log) is copied.
    let f = fix();
    write(&f, "app.txt", b"keep");
    write(&f, "debug.log", b"also-keep");
    build_df(&f, "di-none", "FROM scratch\nCOPY . /app\n").unwrap();
    let dest = hydrate(&f, "di-none", "none");
    assert!(dest.join("app/app.txt").exists());
    assert!(
        dest.join("app/debug.log").exists(),
        "without .dockerignore, .log is copied unchanged"
    );
}

#[test]
fn dockerfile_and_dockerignore_self_excluded_from_copy_dot() {
    // Docker's always-out rule: `COPY . /app` never copies the Dockerfile or the
    // `.dockerignore` itself (even with no user patterns).
    let f = fix();
    write(&f, ".dockerignore", b"\n");
    write(&f, "real.txt", b"keep");
    build_df(&f, "di-self", "FROM scratch\nCOPY . /app\n").unwrap();
    let dest = hydrate(&f, "di-self", "self");
    assert!(dest.join("app/real.txt").exists());
    assert!(
        !dest.join("app/Dockerfile").exists(),
        "Dockerfile is never copied by COPY ."
    );
    assert!(
        !dest.join("app/.dockerignore").exists(),
        ".dockerignore is never copied by COPY ."
    );
}

#[test]
fn glob_copy_excludes_ignored_match() {
    // `COPY *.log /logs/` after `debug.log` is ignored ⇒ only error.log copies.
    let f = fix();
    write(&f, ".dockerignore", b"debug.log\n");
    write(&f, "debug.log", b"drop");
    write(&f, "error.log", b"keep");
    build_df(&f, "di-glob", "FROM scratch\nCOPY *.log /logs/\n").unwrap();
    let dest = hydrate(&f, "di-glob", "glob");
    assert!(dest.join("logs/error.log").exists());
    assert!(!dest.join("logs/debug.log").exists());
}

#[test]
fn adding_ignored_file_does_not_bust_cache() {
    // THE memo-key invariant: a build, then add an IGNORED file, then rebuild —
    // the COPY step must be a CACHE HIT (the ignored file is not hashed). We
    // measure via BuildReport.cached_steps: rebuild #2 caches the COPY step.
    let f = fix();
    write(&f, ".dockerignore", b"*.log\n");
    write(&f, "app.txt", b"keep");
    let r1 = build_df(&f, "di-cache", "FROM scratch\nCOPY . /app\n").unwrap();
    assert_eq!(r1.cached_steps, 0, "first build: nothing cached");

    // Add an IGNORED file; the COPY context (minus ignored) is unchanged.
    write(&f, "noise.log", b"this is ignored");
    let r2 = build_df(&f, "di-cache", "FROM scratch\nCOPY . /app\n").unwrap();
    assert_eq!(
        r2.cached_steps, r2.steps,
        "adding an IGNORED file must NOT bust the cache (all steps cached)"
    );
}

#[test]
fn adding_non_ignored_file_busts_cache() {
    // The converse guard: a NON-ignored new file DOES change the key (the COPY
    // step is re-executed, not a false hit).
    let f = fix();
    write(&f, ".dockerignore", b"*.log\n");
    write(&f, "app.txt", b"keep");
    let r1 = build_df(&f, "di-bust", "FROM scratch\nCOPY . /app\n").unwrap();
    assert_eq!(r1.cached_steps, 0);

    write(&f, "extra.txt", b"new real file");
    let r2 = build_df(&f, "di-bust", "FROM scratch\nCOPY . /app\n").unwrap();
    assert!(
        r2.cached_steps < r2.steps,
        "a NON-ignored new file MUST bust the COPY cache (re-executed)"
    );
    // And the new file is actually in the layer.
    let dest = hydrate(&f, "di-bust", "bust");
    assert!(dest.join("app/extra.txt").exists());
}
