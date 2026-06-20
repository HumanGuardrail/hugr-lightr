//! name→id registry — a fail-closed, daemonless file map from a user-supplied
//! container NAME to a run id, matching Docker's name semantics.
//!
//! Storage: `<home>/run/names/<name>` — a tiny file whose contents are the run
//! id the name maps to. Following the house convention (see `network::registry`),
//! the `home` root is INJECTED by the caller (which passes `paths::lightr_home()`),
//! never read from the global env inside this module — so the unit tests pass a
//! private tempdir and run safely in parallel (CI uses `cargo test --workspace`,
//! which is multi-threaded).
//!
//! This is a self-contained PRIMITIVE (WP-LIFE-01). Wiring it into `run --name`
//! and the lifecycle verbs is a later WP — nothing here touches the CLI or the
//! verb handlers.

use std::path::{Path, PathBuf};

use lightr_core::{LightrError, Result};

/// Directory holding the name→id mapping files: `<home>/run/names`.
fn names_dir(home: &Path) -> PathBuf {
    home.join("run").join("names")
}

/// The mapping file for a single name: `<home>/run/names/<name>`.
fn name_path(home: &Path, name: &str) -> PathBuf {
    names_dir(home).join(name)
}

/// The run-id directory root: `<home>/run`.
///
/// Run ids are the directory entries directly under it (the `names`
/// sub-directory is excluded — it is the registry's own storage, not a run).
fn run_root(home: &Path) -> PathBuf {
    home.join("run")
}

/// Validate a user-supplied container name against Docker's rule:
/// `[a-zA-Z0-9][a-zA-Z0-9_.-]*` — non-empty, first char alphanumeric, the rest
/// alphanumeric or one of `_ . -`. Fail-closed: anything else is rejected with
/// an honest error.
pub fn name_validate(name: &str) -> Result<()> {
    let mut chars = name.chars();
    match chars.next() {
        None => return Err(LightrError::InvalidRef("empty name".to_string())),
        Some(c) if c.is_ascii_alphanumeric() => {}
        Some(_) => {
            return Err(LightrError::InvalidRef(format!(
                "invalid name '{name}': must start with [a-zA-Z0-9]"
            )));
        }
    }
    for c in chars {
        let ok = c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
        if !ok {
            return Err(LightrError::InvalidRef(format!(
                "invalid name '{name}': only [a-zA-Z0-9_.-] allowed"
            )));
        }
    }
    Ok(())
}

/// Atomically create the name→id mapping under `home`. If the name is already in
/// use the claim fails (Docker: "name already in use"). The atomicity comes from
/// `create_new` (O_EXCL): two concurrent claims can never both win.
pub fn claim(home: &Path, name: &str, id: &str) -> Result<()> {
    name_validate(name)?;

    let dir = names_dir(home);
    std::fs::create_dir_all(&dir).map_err(LightrError::Io)?;

    let path = name_path(home, name);
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(LightrError::InvalidRef(format!(
                "name already in use: {name}"
            )));
        }
        Err(e) => return Err(LightrError::Io(e)),
    };
    use std::io::Write;
    file.write_all(id.as_bytes()).map_err(LightrError::Io)?;
    Ok(())
}

/// Remove the name→id mapping under `home`. Absent name is NOT an error
/// (idempotent release): only a real I/O failure surfaces.
pub fn release(home: &Path, name: &str) -> Result<()> {
    let path = name_path(home, name);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LightrError::Io(e)),
    }
}

/// Read the run id a name maps to under `home`, if the name exists.
fn lookup_name(home: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(name_path(home, name))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Enumerate existing run ids: the directory entries directly under
/// `<home>/run`, excluding the `names` registry sub-directory. Mirrors the
/// listing pattern in `ps` (no standalone helper exists to reuse).
fn list_run_ids(home: &Path) -> Result<Vec<String>> {
    let root = run_root(home);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = match path.file_name().map(|s| s.to_string_lossy().into_owned()) {
            Some(n) => n,
            None => continue,
        };
        if id == "names" {
            continue;
        }
        ids.push(id);
    }
    Ok(ids)
}

/// Resolve a user token to a run id under `home`, in Docker's precedence:
///   1. exact id          (the token IS a run-id directory)
///   2. exact name        (the token is a claimed name)
///   3. unambiguous id-PREFIX (≥3 chars; one match wins, many = ambiguous)
///
/// None of the above = not-found. Fail-closed throughout.
pub fn resolve(home: &Path, token: &str) -> Result<String> {
    if token.is_empty() {
        return Err(LightrError::InvalidRef("empty token".to_string()));
    }

    let ids = list_run_ids(home)?;

    // 1. exact id
    if ids.iter().any(|i| i == token) {
        return Ok(token.to_string());
    }

    // 2. exact name
    if let Some(id) = lookup_name(home, token) {
        return Ok(id);
    }

    // 3. unambiguous id-prefix (Docker requires ≥3 chars for a short id)
    if token.len() >= 3 {
        let matches: Vec<&String> = ids.iter().filter(|i| i.starts_with(token)).collect();
        match matches.len() {
            1 => return Ok(matches[0].clone()),
            0 => {}
            _ => {
                return Err(LightrError::InvalidRef(format!(
                    "ambiguous id prefix: {token}"
                )));
            }
        }
    }

    Err(LightrError::RefNotFound(token.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A private tempdir used as the registry `home` root — passed explicitly to
    /// every fn (no global env mutation), so tests run safely in parallel
    /// (matching how CI invokes `cargo test --workspace`). Removed on drop. The
    /// atomic counter + nanos guarantee a unique dir even under concurrent tests.
    struct TmpHome {
        dir: PathBuf,
    }

    impl TmpHome {
        fn new(tag: &str) -> Self {
            static CTR: AtomicU64 = AtomicU64::new(0);
            let n = CTR.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!("lightr-reg-{tag}-{nanos}-{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            TmpHome { dir }
        }
        fn path(&self) -> &Path {
            &self.dir
        }
    }

    impl Drop for TmpHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Materialize a run-id directory under `<home>/run/<id>`.
    fn make_run(home: &Path, id: &str) {
        std::fs::create_dir_all(run_root(home).join(id)).unwrap();
    }

    #[test]
    fn validate_accepts_docker_legal_names() {
        for n in ["a", "A", "0", "web", "my_app", "db-1", "v1.2.3", "X_y-z.0"] {
            assert!(name_validate(n).is_ok(), "should accept {n}");
        }
    }

    #[test]
    fn validate_rejects_illegal_names() {
        for n in [
            "",
            "_leading",
            ".dot",
            "-dash",
            "has space",
            "slash/x",
            "uni\u{e9}",
        ] {
            assert!(name_validate(n).is_err(), "should reject {n:?}");
        }
    }

    #[test]
    fn claim_then_resolve_by_name_roundtrips() {
        let h = TmpHome::new("roundtrip");
        let home = h.path();
        make_run(home, "abc123def456");
        claim(home, "web", "abc123def456").unwrap();
        assert_eq!(resolve(home, "web").unwrap(), "abc123def456");
    }

    #[test]
    fn duplicate_claim_errors() {
        let h = TmpHome::new("dup");
        let home = h.path();
        claim(home, "web", "id-one").unwrap();
        let err = claim(home, "web", "id-two").unwrap_err();
        match err {
            LightrError::InvalidRef(m) => assert!(m.contains("already in use"), "{m}"),
            other => panic!("expected InvalidRef, got {other:?}"),
        }
        // First mapping must be untouched.
        assert_eq!(resolve(home, "web").unwrap(), "id-one");
    }

    #[test]
    fn resolve_exact_id_beats_everything() {
        let h = TmpHome::new("exact-id");
        let home = h.path();
        make_run(home, "deadbeefcafe");
        // Exact id wins immediately.
        assert_eq!(resolve(home, "deadbeefcafe").unwrap(), "deadbeefcafe");
    }

    #[test]
    fn resolve_prefix_unique() {
        let h = TmpHome::new("prefix-uniq");
        let home = h.path();
        make_run(home, "aaa111");
        make_run(home, "bbb222");
        assert_eq!(resolve(home, "aaa").unwrap(), "aaa111");
    }

    #[test]
    fn resolve_prefix_ambiguous() {
        let h = TmpHome::new("prefix-amb");
        let home = h.path();
        make_run(home, "abc111");
        make_run(home, "abc222");
        let err = resolve(home, "abc").unwrap_err();
        match err {
            LightrError::InvalidRef(m) => assert!(m.contains("ambiguous"), "{m}"),
            other => panic!("expected InvalidRef, got {other:?}"),
        }
    }

    #[test]
    fn resolve_prefix_too_short_is_not_found() {
        let h = TmpHome::new("prefix-short");
        let home = h.path();
        make_run(home, "abcdef");
        // <3 chars never resolves as a prefix.
        let err = resolve(home, "ab").unwrap_err();
        assert!(matches!(err, LightrError::RefNotFound(_)));
    }

    #[test]
    fn resolve_none_is_not_found() {
        let h = TmpHome::new("none");
        let home = h.path();
        make_run(home, "xyz999");
        let err = resolve(home, "nomatch").unwrap_err();
        assert!(matches!(err, LightrError::RefNotFound(_)));
    }

    #[test]
    fn release_is_idempotent() {
        let h = TmpHome::new("release");
        let home = h.path();
        claim(home, "web", "id-1").unwrap();
        release(home, "web").unwrap();
        // Absent name: still Ok.
        release(home, "web").unwrap();
        // After release the name no longer resolves.
        let err = resolve(home, "web").unwrap_err();
        assert!(matches!(err, LightrError::RefNotFound(_)));
        // And the name can be re-claimed.
        claim(home, "web", "id-2").unwrap();
        assert_eq!(resolve(home, "web").unwrap(), "id-2");
    }
}
