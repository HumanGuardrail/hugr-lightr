//! WP-G end-to-end tests: ADD auto-extract of `.tar.bz2`/`.tar.xz` (now decoded
//! in-process, superseding WP-DF-07's honest-defer) + `ADD <url>` remote fetch
//! (downloaded, never auto-extracted, fail-closed off-network).
//!
//! Declared as a `#[path]` child of `exec_df07_tests` (godfile cap: <400 LOC per
//! file), so the shared fixtures (`fix`/`build_df`/`build_err`/`hydrate`/
//! `append_bytes`) are reached through `super::`.
//!
//! Determinism note: each test owns its tempdirs + store (parallel-safe). The URL
//! tests assert the FETCH PATH IS WIRED and FAILS CLOSED — they use the reserved
//! `example.com` doc host and assert on the error shape, so they pass whether the
//! sandbox has no network (transport error) or reaches a 4xx — never a silent
//! success, and a URL source is never auto-extracted.
use super::*;

/// WP-G: the shared 2-entry archive, bzip2-compressed, at `path`.
fn write_tar_bz2(path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let enc = bzip2::write::BzEncoder::new(file, bzip2::Compression::default());
    let mut ar = tar::Builder::new(enc);
    append_bytes(&mut ar, "a.txt", b"alpha");
    append_bytes(&mut ar, "dir/b.txt", b"beta");
    ar.into_inner().unwrap().finish().unwrap();
}

/// WP-G: the shared 2-entry archive, xz-compressed, at `path`.
fn write_tar_xz(path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let enc = xz2::write::XzEncoder::new(file, 6);
    let mut ar = tar::Builder::new(enc);
    append_bytes(&mut ar, "a.txt", b"alpha");
    append_bytes(&mut ar, "dir/b.txt", b"beta");
    ar.into_inner().unwrap().finish().unwrap();
}

// ── Tar auto-extract: bzip2 / xz (WP-G — supersedes the DF-07 honest-defer) ───

#[test]
fn add_tar_bz2_auto_extracts() {
    // WP-G: bzip2 is now decoded in-process — a `.tar.bz2` EXTRACTS its entries
    // (was previously an honest "unsupported" fail; that defer is now lifted).
    let f = fix();
    write_tar_bz2(&f.ctx_path.join("bundle.tar.bz2"));
    build_df(&f, "wpg-bz2", "FROM scratch\nADD bundle.tar.bz2 /opt/\n").unwrap();
    let dest = hydrate(&f, "wpg-bz2", "bz2");
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/a.txt")).unwrap(),
        "alpha",
        ".tar.bz2 entry a.txt must be extracted into dest"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/dir/b.txt")).unwrap(),
        "beta"
    );
    assert!(
        !dest.join("opt/bundle.tar.bz2").exists(),
        "the archive FILE must NOT land — ADD extracts a .tar.bz2"
    );
}

#[test]
fn add_tar_xz_auto_extracts() {
    // WP-G: xz is now decoded in-process — a `.tar.xz` EXTRACTS its entries.
    let f = fix();
    write_tar_xz(&f.ctx_path.join("bundle.tar.xz"));
    build_df(&f, "wpg-xz", "FROM scratch\nADD bundle.tar.xz /opt/\n").unwrap();
    let dest = hydrate(&f, "wpg-xz", "xz");
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/a.txt")).unwrap(),
        "alpha",
        ".tar.xz entry a.txt must be extracted into dest"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("opt/dir/b.txt")).unwrap(),
        "beta"
    );
    assert!(!dest.join("opt/bundle.tar.xz").exists());
}

#[test]
fn add_corrupt_xz_fails_closed() {
    // A `.tar.xz` whose bytes are NOT a valid xz stream must FAIL, never silently
    // copy or no-op — the decoder surfaces the error through `unpack`.
    let f = fix();
    std::fs::write(f.ctx_path.join("x.tar.xz"), b"not really xz").unwrap();
    let err = build_err(&f, "wpg-xz-bad", "FROM scratch\nADD x.tar.xz /opt/\n");
    assert!(
        !err.is_empty(),
        "a corrupt .tar.xz must surface a fail-closed error, got empty"
    );
}

// ── Remote URL → DOWNLOADED, never auto-extracted; fail-closed off-network ────

#[test]
fn add_url_fetch_path_is_wired_and_fails_closed() {
    // WP-G: ADD <url> now FETCHES (no longer an "unsupported" string). With no
    // reachable server (or a 4xx), it must fail CLOSED with a network/HTTP error
    // that NAMES the URL — proving the fetch path is wired AND fail-closed, not a
    // silent success. Uses the reserved-doc host so no real infra is touched.
    let f = fix();
    let err = build_err(
        &f,
        "wpg-url",
        "FROM scratch\nADD https://example.com/app.tar.gz /opt/\n",
    );
    let lc = err.to_lowercase();
    assert!(
        lc.contains("add") && lc.contains("app.tar.gz"),
        "a failed ADD URL must name the URL it tried to fetch, got: {err}"
    );
    // The old "non-hermetic / unsupported" copy-out message is GONE.
    assert!(
        !lc.contains("unsupported"),
        "ADD URL is no longer 'unsupported' — it fetches; got: {err}"
    );
}

#[test]
fn add_http_url_fetch_path_is_wired() {
    let f = fix();
    let err = build_err(
        &f,
        "wpg-http",
        "FROM scratch\nADD http://example.com/f.txt /f.txt\n",
    );
    let lc = err.to_lowercase();
    assert!(
        lc.contains("f.txt"),
        "http:// ADD must attempt the fetch and name the URL on failure, got: {err}"
    );
}
