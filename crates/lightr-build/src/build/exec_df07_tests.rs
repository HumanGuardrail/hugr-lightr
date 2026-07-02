//! WP-DF-07 end-to-end tests: ADD parity, exercised through the full `build()`
//! loop + a hydrate of the final tree. Split out of `exec_tests.rs` for the
//! godfile cap.
//!
//! Coverage:
//! - Local file/dir/multi-src/glob/--chmod/--chown ADD == COPY behaviour
//!   (reuses DF-06's CopyMeta/copy infra).
//! - Tar auto-extract: a `.tar` and a `.tar.gz` EXTRACT their entries into dest
//!   (Docker semantics) — the archive FILE itself never lands.
//! - `.tar.bz2` / `.tar.xz` EXTRACT in-process (WP-G); a corrupt stream fails
//!   closed (never a silent copy).
//! - `ADD <url>` FETCHES (WP-G): the fetch path is wired and fails closed when
//!   the URL is unreachable/4xx (no silent success, never auto-extracted).
//! - MEMO no-false-hit: a differing --chmod busts the cache; identical ADD hits.
//!
//! Each test owns its tempdirs + store and never MUTATES process-global state,
//! but `build()`/`hydrate` READ the process-global `LIGHTR_HOME`, so the
//! `build_df`/`hydrate` helpers hold the crate-wide shared read lock
//! (`build::LIGHTR_HOME_ENV_LOCK`) to exclude the setter tests
//! (exec_tests/up_tests) while they run. Readers still parallelize; the work dir
//! is a nanos-unique temp. `--chown` is exercised with a NO-OP chown to the
//! current uid:gid (unprivileged-safe); the cache-bust guarantee is proven at the
//! key layer (memo_tests.rs) and here via re-run.
use super::*;
use std::io::Write;
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

fn build_err(f: &Fix, name: &str, df_body: &str) -> String {
    match build_df(f, name, df_body) {
        Ok(_) => panic!("expected build to fail, but it succeeded"),
        Err(e) => format!("{e}"),
    }
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

/// Write a 2-entry uncompressed tar (`a.txt`, `dir/b.txt`) at `path`.
fn write_tar(path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut ar = tar::Builder::new(file);
    append_bytes(&mut ar, "a.txt", b"alpha");
    append_bytes(&mut ar, "dir/b.txt", b"beta");
    ar.finish().unwrap();
}

/// Write the same 2-entry archive, gzip-compressed, at `path`.
fn write_tar_gz(path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut ar = tar::Builder::new(enc);
    append_bytes(&mut ar, "a.txt", b"alpha");
    append_bytes(&mut ar, "dir/b.txt", b"beta");
    ar.into_inner().unwrap().finish().unwrap();
}

// WP-G: ADD `.tar.bz2`/`.tar.xz` extraction + ADD URL fetch tests live in a
// sibling module (godfile cap <400 LOC/file). Declared as a `#[path]` child of
// THIS test module so the moved tests reach the shared `fix`/`build_df`/
// `build_err`/`hydrate`/`append_bytes` helpers via `super::`.
#[path = "exec_wpg_tests.rs"]
mod wpg_tests;

fn append_bytes<W: Write>(ar: &mut tar::Builder<W>, name: &str, data: &[u8]) {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    ar.append_data(&mut h, name, data).unwrap();
}

// ── Local ADD == COPY behaviour ──────────────────────────────────────────────

#[test]
fn add_local_file_is_copy() {
    // A plain local file ADD lands at dest, byte-for-byte (COPY semantics).
    let f = fix();
    std::fs::write(f.ctx_path.join("src.txt"), b"content").unwrap();
    let r = build_df(&f, "df07-file", "FROM scratch\nADD src.txt /src.txt\n").unwrap();
    assert_eq!(r.cached_steps, 0, "cold build");
    let dest = hydrate(&f, "df07-file", "file");
    assert_eq!(
        std::fs::read_to_string(dest.join("src.txt")).unwrap(),
        "content"
    );
}

#[test]
fn add_multi_src_into_dir() {
    let f = fix();
    for n in ["a.txt", "b.txt"] {
        std::fs::write(f.ctx_path.join(n), n.as_bytes()).unwrap();
    }
    build_df(&f, "df07-multi", "FROM scratch\nADD a.txt b.txt /dst/\n").unwrap();
    let dest = hydrate(&f, "df07-multi", "multi");
    for n in ["a.txt", "b.txt"] {
        assert_eq!(
            std::fs::read_to_string(dest.join("dst").join(n)).unwrap(),
            n
        );
    }
}

#[test]
fn add_glob_expands_against_context() {
    let f = fix();
    std::fs::write(f.ctx_path.join("one.txt"), b"1").unwrap();
    std::fs::write(f.ctx_path.join("two.txt"), b"2").unwrap();
    std::fs::write(f.ctx_path.join("skip.md"), b"m").unwrap();
    build_df(&f, "df07-glob", "FROM scratch\nADD *.txt /app/\n").unwrap();
    let d1 = hydrate(&f, "df07-glob", "glob");
    assert_eq!(
        std::fs::read_to_string(d1.join("app/one.txt")).unwrap(),
        "1"
    );
    assert_eq!(
        std::fs::read_to_string(d1.join("app/two.txt")).unwrap(),
        "2"
    );
    assert!(
        !d1.join("app/skip.md").exists(),
        "non-matching file must NOT be added by *.txt"
    );
}

#[test]
fn add_dir_copies_contents_not_dir() {
    // Docker dir semantics: `ADD srcdir /app/` lands srcdir's CONTENTS under /app.
    let f = fix();
    std::fs::create_dir_all(f.ctx_path.join("srcdir/nested")).unwrap();
    std::fs::write(f.ctx_path.join("srcdir/inner.txt"), b"i").unwrap();
    std::fs::write(f.ctx_path.join("srcdir/nested/deep.txt"), b"d").unwrap();
    build_df(&f, "df07-dir", "FROM scratch\nADD srcdir /app/\n").unwrap();
    let d2 = hydrate(&f, "df07-dir", "dir");
    assert_eq!(
        std::fs::read_to_string(d2.join("app/inner.txt")).unwrap(),
        "i",
        "dir CONTENTS land directly under dest (not under dest/srcdir)"
    );
    assert_eq!(
        std::fs::read_to_string(d2.join("app/nested/deep.txt")).unwrap(),
        "d"
    );
    assert!(!d2.join("app/srcdir").exists());
}

#[test]
fn add_chmod_applies_octal_mode() {
    let f = fix();
    std::fs::write(f.ctx_path.join("s.txt"), b"x").unwrap();
    build_df(
        &f,
        "df07-chmod",
        "FROM scratch\nADD --chmod=0600 s.txt /s.txt\n",
    )
    .unwrap();
    let dest = hydrate(&f, "df07-chmod", "chmod");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.join("s.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o600, "--chmod=0600 must set mode 0o600");
    }
    #[cfg(not(unix))]
    let _ = dest;
}

#[test]
fn add_chown_numeric_noop_succeeds() {
    let f = fix();
    std::fs::write(f.ctx_path.join("f.txt"), b"data").unwrap();
    #[cfg(unix)]
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    #[cfg(not(unix))]
    let (uid, gid) = (0u32, 0u32);
    let df = format!("FROM scratch\nADD --chown={uid}:{gid} f.txt /f.txt\n");
    build_df(&f, "df07-chown", &df).unwrap();
    let dest = hydrate(&f, "df07-chown", "chown");
    assert_eq!(std::fs::read_to_string(dest.join("f.txt")).unwrap(), "data");
}

// ── Tar auto-extract (the ADD-specific feature) ──────────────────────────────

#[test]
fn add_tar_auto_extracts_entries_not_the_archive() {
    // A `.tar` src EXTRACTS its entries into dest (Docker), and the archive FILE
    // itself must NOT appear in the layer.
    let f = fix();
    write_tar(&f.ctx_path.join("bundle.tar"));
    build_df(&f, "df07-tar", "FROM scratch\nADD bundle.tar /opt/\n").unwrap();
    let dest = hydrate(&f, "df07-tar", "tar");
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/a.txt")).unwrap(),
        "alpha",
        "tar entry a.txt must be extracted into dest"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/dir/b.txt")).unwrap(),
        "beta",
        "nested tar entry must be extracted"
    );
    assert!(
        !dest.join("opt/bundle.tar").exists(),
        "the archive FILE must NOT land — ADD extracts, it does not copy the tar"
    );
}

#[test]
fn add_tar_gz_auto_extracts() {
    let f = fix();
    write_tar_gz(&f.ctx_path.join("bundle.tar.gz"));
    build_df(&f, "df07-tgz", "FROM scratch\nADD bundle.tar.gz /opt/\n").unwrap();
    let dest = hydrate(&f, "df07-tgz", "tgz");
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/a.txt")).unwrap(),
        "alpha"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/dir/b.txt")).unwrap(),
        "beta"
    );
    assert!(!dest.join("opt/bundle.tar.gz").exists());
}

// ── MEMO no-false-hit (chown/chmod) ──────────────────────────────────────────

#[test]
fn add_differing_chmod_busts_cache_no_false_hit() {
    // The SAME ADD of the SAME bytes, rebuilt with a different --chmod against the
    // SAME store, must NOT reuse the first layer — the mode is part of the output.
    let f = fix();
    std::fs::write(f.ctx_path.join("f.txt"), b"x").unwrap();
    build_df(
        &f,
        "df07-bust",
        "FROM scratch\nADD --chmod=0600 f.txt /f.txt\n",
    )
    .unwrap();
    let r2 = build_df(
        &f,
        "df07-bust",
        "FROM scratch\nADD --chmod=0640 f.txt /f.txt\n",
    )
    .unwrap();
    assert!(
        r2.cached_steps < r2.steps,
        "a different --chmod must NOT be a full cache hit"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dest = hydrate(&f, "df07-bust", "bust");
        let mode = std::fs::metadata(dest.join("f.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o640, "the second --chmod must win (no false hit)");
    }
}

#[test]
fn add_identical_with_chmod_is_memo_hit() {
    let f = fix();
    std::fs::write(f.ctx_path.join("f.txt"), b"x").unwrap();
    let df = "FROM scratch\nADD --chmod=0644 f.txt /f.txt\n";
    let r1 = build_df(&f, "df07-hit", df).unwrap();
    assert_eq!(r1.cached_steps, 0, "cold build");
    let r2 = build_df(&f, "df07-hit", df).unwrap();
    assert_eq!(
        r2.cached_steps, r2.steps,
        "identical ADD + --chmod ⇒ every step is a memo hit"
    );
}

#[test]
fn add_tar_content_change_busts_cache() {
    // The extracted archive's CONTENT is keyed (via hash_copy_source on the .tar
    // bytes): editing the archive busts the cache, so the new entries appear.
    let f = fix();
    write_tar(&f.ctx_path.join("bundle.tar"));
    let r1 = build_df(&f, "df07-tarbust", "FROM scratch\nADD bundle.tar /opt/\n").unwrap();
    assert_eq!(r1.cached_steps, 0);
    // Repack with different content under the same name.
    {
        let file = std::fs::File::create(f.ctx_path.join("bundle.tar")).unwrap();
        let mut ar = tar::Builder::new(file);
        append_bytes(&mut ar, "a.txt", b"GAMMA");
        ar.finish().unwrap();
    }
    let r2 = build_df(&f, "df07-tarbust", "FROM scratch\nADD bundle.tar /opt/\n").unwrap();
    assert!(
        r2.cached_steps < r2.steps,
        "changed archive must bust the cache"
    );
    let dest = hydrate(&f, "df07-tarbust", "tarbust");
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/a.txt")).unwrap(),
        "GAMMA"
    );
}
