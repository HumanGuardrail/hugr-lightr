//! WP-DF-06 end-to-end tests: COPY parity (--chown/--chmod/multi-src/dir-contents/
//! glob; --from honest-unsupported), exercised through the full `build()` loop +
//! a hydrate of the final tree. Split out of `exec_tests.rs` for the godfile cap.
//!
//! Each test owns its tempdirs + store and never MUTATES process-global state,
//! but `build()`/`hydrate` READ the process-global `LIGHTR_HOME`, so the
//! `build_df`/`hydrate` helpers hold the crate-wide shared read lock
//! (`build::LIGHTR_HOME_ENV_LOCK`) to exclude the setter tests
//! (exec_tests/up_tests) while they run. Readers still parallelize; each test
//! uses a nanos-unique temp work dir and hydrates into an owned dir.
//!
//! `--chmod` is asserted on the hydrated tree (snapshot↔hydrate roundtrips the
//! full permission bits — proven in lightr-index). `--chown` to an *arbitrary*
//! owner needs root, so the executor path is exercised with a NO-OP chown (the
//! current uid:gid) that succeeds unprivileged; the cache-busting guarantee is
//! proven deterministically at the key layer (memo_tests.rs) + here via re-run.
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

/// Build expecting FAILURE; return the error's display string. (Avoids requiring
/// `BuildReport: Debug` for `Result::unwrap_err`.)
fn build_err(f: &Fix, name: &str, df_body: &str) -> String {
    match build_df(f, name, df_body) {
        Ok(_) => panic!("expected build to fail, but it succeeded"),
        Err(e) => format!("{e}"),
    }
}

/// Hydrate the named built image into a fresh dir and return that dir.
fn hydrate(f: &Fix, name: &str, tag: &str) -> std::path::PathBuf {
    let dest = f.store_tmp_path.join(format!("hydrated-{tag}"));
    // hydrate READs the process-global LIGHTR_HOME; hold the shared read lock.
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
    lightr_index::hydrate(&dest, &f.store, name).unwrap();
    dest
}

#[test]
fn copy_chmod_applies_octal_mode() {
    // `COPY --chmod=0600` must set the copied file's mode to 0o600 in the layer.
    let f = fix();
    std::fs::write(f.ctx_path.join("secret.txt"), b"shh").unwrap();
    build_df(
        &f,
        "df06-chmod",
        "FROM scratch\nCOPY --chmod=0600 secret.txt /secret.txt\n",
    )
    .unwrap();
    let dest = hydrate(&f, "df06-chmod", "chmod");
    let p = dest.join("secret.txt");
    assert!(p.exists(), "copied file must exist");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "shh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600, "--chmod=0600 must set mode 0o600");
    }
}

#[test]
fn copy_chmod_invalid_octal_is_honest_error() {
    // Fail-closed: a non-octal --chmod is an explicit error, never a silent skip.
    let f = fix();
    std::fs::write(f.ctx_path.join("f.txt"), b"x").unwrap();
    let err = build_err(
        &f,
        "df06-badchmod",
        "FROM scratch\nCOPY --chmod=notoctal f.txt /f.txt\n",
    );
    assert!(
        err.contains("chmod"),
        "invalid --chmod must surface an honest error, got: {err}"
    );
}

#[test]
fn copy_chown_numeric_noop_succeeds() {
    // A numeric --chown to the CURRENT uid:gid is a successful no-op on any box
    // (no root needed): it exercises the chown application path without changing
    // ownership. Proves --chown=uid:gid is parsed + applied without error.
    let f = fix();
    std::fs::write(f.ctx_path.join("f.txt"), b"data").unwrap();
    #[cfg(unix)]
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    #[cfg(not(unix))]
    let (uid, gid) = (0u32, 0u32);
    let df = format!("FROM scratch\nCOPY --chown={uid}:{gid} f.txt /f.txt\n");
    build_df(&f, "df06-chown", &df).unwrap();
    let dest = hydrate(&f, "df06-chown", "chown");
    assert_eq!(std::fs::read_to_string(dest.join("f.txt")).unwrap(), "data");
}

#[test]
fn copy_chown_named_is_best_effort_noop() {
    // A NAMED --chown cannot be resolved without the image's /etc/passwd; it is an
    // honest best-effort no-op (no uid/gid resolved ⇒ chown left unchanged) and
    // the build still succeeds. (Docker resolves names; we are honest we don't.)
    let f = fix();
    std::fs::write(f.ctx_path.join("f.txt"), b"data").unwrap();
    build_df(
        &f,
        "df06-named",
        "FROM scratch\nCOPY --chown=appuser:appgrp f.txt /f.txt\n",
    )
    .unwrap();
    let dest = hydrate(&f, "df06-named", "named");
    assert_eq!(std::fs::read_to_string(dest.join("f.txt")).unwrap(), "data");
}

#[test]
fn copy_multi_src_into_dir() {
    // `COPY a b c /dst/` lands all three under /dst (dest is a dir when >1 src).
    let f = fix();
    for n in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(f.ctx_path.join(n), n.as_bytes()).unwrap();
    }
    build_df(
        &f,
        "df06-multi",
        "FROM scratch\nCOPY a.txt b.txt c.txt /dst/\n",
    )
    .unwrap();
    let dest = hydrate(&f, "df06-multi", "multi");
    for n in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(
            std::fs::read_to_string(dest.join("dst").join(n)).unwrap(),
            n,
            "{n} must land under /dst"
        );
    }
}

#[test]
fn copy_dir_copies_contents_not_dir() {
    // Docker dir semantics: `COPY srcdir /app/` copies the CONTENTS of srcdir into
    // /app (so /app/inner.txt), NOT /app/srcdir/inner.txt.
    let f = fix();
    std::fs::create_dir_all(f.ctx_path.join("srcdir/nested")).unwrap();
    std::fs::write(f.ctx_path.join("srcdir/inner.txt"), b"i").unwrap();
    std::fs::write(f.ctx_path.join("srcdir/nested/deep.txt"), b"d").unwrap();
    build_df(&f, "df06-dir", "FROM scratch\nCOPY srcdir /app/\n").unwrap();
    let dest = hydrate(&f, "df06-dir", "dir");
    assert_eq!(
        std::fs::read_to_string(dest.join("app/inner.txt")).unwrap(),
        "i",
        "dir CONTENTS land directly under dest (not under dest/srcdir)"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("app/nested/deep.txt")).unwrap(),
        "d",
        "nested contents preserved"
    );
    assert!(
        !dest.join("app/srcdir").exists(),
        "the source dir name must NOT appear under dest (contents, not dir)"
    );
}

#[test]
fn copy_glob_expands_against_context() {
    // `COPY *.txt /app/` copies every matching file; non-matching files are not.
    let f = fix();
    std::fs::write(f.ctx_path.join("one.txt"), b"1").unwrap();
    std::fs::write(f.ctx_path.join("two.txt"), b"2").unwrap();
    std::fs::write(f.ctx_path.join("skip.md"), b"m").unwrap();
    build_df(&f, "df06-glob", "FROM scratch\nCOPY *.txt /app/\n").unwrap();
    let dest = hydrate(&f, "df06-glob", "glob");
    assert_eq!(
        std::fs::read_to_string(dest.join("app/one.txt")).unwrap(),
        "1"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("app/two.txt")).unwrap(),
        "2"
    );
    assert!(
        !dest.join("app/skip.md").exists(),
        "non-matching file must NOT be copied by *.txt"
    );
}

#[test]
fn copy_glob_no_match_is_honest_error() {
    // A glob matching nothing is an explicit error (Docker: no source files).
    let f = fix();
    let err = build_err(&f, "df06-noglob", "FROM scratch\nCOPY *.nope /app/\n");
    assert!(
        err.to_lowercase().contains("no source"),
        "an empty glob must surface an honest error, got: {err}"
    );
}

#[test]
fn copy_from_unknown_stage_is_honest_error() {
    // WP-DF-03 landed multi-stage: `COPY --from=<stage>` now RESOLVES against the
    // stage table. A ref to a NON-EXISTENT stage (here `builder`, never declared)
    // is an honest fail-closed error, NOT a half-copy. (DF-03 supersedes DF-06's
    // prior "unsupported until DF-03" placeholder for this single-stage case.)
    let f = fix();
    let msg = build_err(
        &f,
        "df06-from",
        "FROM scratch\nCOPY --from=builder /src /dst\n",
    );
    assert!(
        msg.contains("--from") && msg.to_lowercase().contains("unknown stage"),
        "--from to an unknown stage must be an honest error, got: {msg}"
    );
}

#[test]
fn plain_copy_is_behavior_preserving() {
    // A flagless single-file `COPY src dest` is unchanged: file lands at dest.
    let f = fix();
    std::fs::write(f.ctx_path.join("src.txt"), b"content").unwrap();
    let r = build_df(&f, "df06-plain", "FROM scratch\nCOPY src.txt /src.txt\n").unwrap();
    assert_eq!(r.cached_steps, 0, "cold build");
    let dest = hydrate(&f, "df06-plain", "plain");
    assert_eq!(
        std::fs::read_to_string(dest.join("src.txt")).unwrap(),
        "content"
    );
}

#[test]
fn differing_chmod_busts_cache_no_false_hit() {
    // END-TO-END no-false-hit: the SAME COPY of the SAME bytes, rebuilt with a
    // different --chmod against the SAME store, must NOT reuse the first layer —
    // the mode is part of the output, so the key differs and the COPY re-runs.
    // Proven via the resulting mode (a false hit would keep the first mode).
    let f = fix();
    std::fs::write(f.ctx_path.join("f.txt"), b"x").unwrap();
    build_df(
        &f,
        "df06-bust",
        "FROM scratch\nCOPY --chmod=0600 f.txt /f.txt\n",
    )
    .unwrap();
    let r2 = build_df(
        &f,
        "df06-bust",
        "FROM scratch\nCOPY --chmod=0640 f.txt /f.txt\n",
    )
    .unwrap();
    assert!(
        r2.cached_steps < r2.steps,
        "a different --chmod must NOT be a full cache hit"
    );
    #[cfg(unix)]
    let dest = hydrate(&f, "df06-bust", "bust");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.join("f.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            mode, 0o640,
            "the second --chmod must win (no false hit on the first layer)"
        );
    }
}

#[test]
fn identical_copy_with_chmod_is_memo_hit() {
    // Determinism: identical COPY + identical --chmod rebuilt against the same
    // store ⇒ full memo hit (the fold is deterministic, not a blanket buster).
    let f = fix();
    std::fs::write(f.ctx_path.join("f.txt"), b"x").unwrap();
    let df = "FROM scratch\nCOPY --chmod=0644 f.txt /f.txt\n";
    let r1 = build_df(&f, "df06-hit", df).unwrap();
    assert_eq!(r1.cached_steps, 0, "cold build");
    let r2 = build_df(&f, "df06-hit", df).unwrap();
    assert_eq!(
        r2.cached_steps, r2.steps,
        "identical COPY + --chmod ⇒ every step is a memo hit"
    );
}
