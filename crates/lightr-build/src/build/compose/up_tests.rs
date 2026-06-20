//! Tests for `up.rs` — CMP-P1-PROFILES active-set selection (behavior-
//! preserving) and WP-CMP-SECRETS-FULL top-level `file:` source ingestion.
//!
//! Parallel-safe: every test that touches the Store uses its OWN tempdir Store +
//! source files; no process-global state is mutated.
use super::*;
use crate::build::compose::model::{empty_service, DepCondition};
use std::sync::atomic::{AtomicU64, Ordering};

/// A service with the given name + profile list (no deps).
fn svc(name: &str, profiles: &[&str]) -> Service {
    let mut s = empty_service(name.to_string());
    s.profiles = profiles.iter().map(|p| p.to_string()).collect();
    s
}

/// A `Compose` with the given services and no top-level secret/config sources.
fn compose_of(services: Vec<Service>) -> Compose {
    Compose {
        services,
        secret_sources: Vec::new(),
        config_sources: Vec::new(),
    }
}

/// `active` set from a slice of profile names.
fn active<'a>(names: &[&'a str]) -> HashSet<&'a str> {
    names.iter().copied().collect()
}

fn names_of(c: &Compose, act: &HashSet<&str>) -> Vec<String> {
    let mut v: Vec<String> = active_service_names(c, act).into_iter().collect();
    v.sort();
    v
}

#[test]
fn no_profiles_no_active_all_services_active() {
    // Behavior-preserving: nothing profiled, no --profile ⇒ every service.
    let c = compose_of(vec![svc("web", &[]), svc("db", &[])]);
    assert_eq!(names_of(&c, &active(&[])), vec!["db", "web"]);
}

#[test]
fn profiled_service_excluded_when_profile_inactive() {
    let c = compose_of(vec![svc("web", &[]), svc("debug", &["dev"])]);
    // `dev` not active ⇒ `debug` excluded, `web` (no profiles) stays.
    assert_eq!(names_of(&c, &active(&[])), vec!["web"]);
}

#[test]
fn profiled_service_included_when_profile_active() {
    let c = compose_of(vec![svc("web", &[]), svc("debug", &["dev"])]);
    assert_eq!(names_of(&c, &active(&["dev"])), vec!["debug", "web"]);
}

#[test]
fn one_of_several_profiles_activates() {
    let c = compose_of(vec![svc("svc", &["a", "b"])]);
    // Any one matching profile activates the service.
    assert_eq!(names_of(&c, &active(&["b"])), vec!["svc"]);
    assert!(active_service_names(&c, &active(&["c"])).is_empty());
}

#[test]
fn no_profile_service_always_active() {
    let c = compose_of(vec![svc("web", &[])]);
    // Even with unrelated profiles active, a no-profile service stays in.
    assert_eq!(names_of(&c, &active(&["dev", "prod"])), vec!["web"]);
}

#[test]
fn active_service_pulls_in_profiled_dependency() {
    // Docker rule: an active service's depends_on target auto-activates even
    // if that target is profile-gated and its profile is not active.
    let mut web = svc("web", &[]);
    web.depends_on = vec![("db".to_string(), DepCondition::Started)];
    let db = svc("db", &["storage"]);
    let c = compose_of(vec![web, db]);
    // `storage` is NOT active, yet `db` is pulled in by `web`'s depends_on.
    assert_eq!(names_of(&c, &active(&[])), vec!["db", "web"]);
}

#[test]
fn auto_activation_is_transitive() {
    // active web -> profiled api -> profiled db: all pulled in.
    let mut web = svc("web", &[]);
    web.depends_on = vec![("api".to_string(), DepCondition::Started)];
    let mut api = svc("api", &["backend"]);
    api.depends_on = vec![("db".to_string(), DepCondition::Started)];
    let db = svc("db", &["storage"]);
    let c = compose_of(vec![web, api, db]);
    assert_eq!(names_of(&c, &active(&[])), vec!["api", "db", "web"]);
}

#[test]
fn inactive_service_does_not_pull_in_its_deps() {
    // `debug` (profile `dev`, inactive) depends_on `db` (profile `storage`).
    // Neither is active and `debug` is not selected, so nothing is pulled in.
    let mut debug = svc("debug", &["dev"]);
    debug.depends_on = vec![("db".to_string(), DepCondition::Started)];
    let db = svc("db", &["storage"]);
    let c = compose_of(vec![svc("web", &[]), debug, db]);
    assert_eq!(names_of(&c, &active(&[])), vec!["web"]);
}

// ---- WP-CMP-SECRETS-FULL: top-level `file:` source ingestion ----

static UNIQ: AtomicU64 = AtomicU64::new(0);

/// A unique tempdir (atomic counter + nanos) — parallel-safe, no shared paths.
fn uniq_dir(tag: &str) -> std::path::PathBuf {
    let n = UNIQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!("lightr-cmpsec-{tag}-{n}-{nanos}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Open a fresh Store rooted in its own tempdir.
fn fresh_store(root: &std::path::Path) -> Store {
    Store::open(root.join("store")).unwrap()
}

/// Write `content` to a host file under `dir` and return its path string.
fn write_file(dir: &std::path::Path, name: &str, content: &[u8]) -> String {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn file_source_is_ingested_and_resolvable() {
    // A top-level `file:` secret is ingested into the Store at up, and the
    // registered ref hydrates back to the original bytes at run.
    let dir = uniq_dir("ingest");
    let store = fresh_store(&dir);
    let path = write_file(&dir, "db_pw.txt", b"s3cr3t");

    let sources = vec![FileSource {
        name: "db_password".to_string(),
        kind: SourceKind::File(path),
    }];
    ingest_file_sources(&store, &sources, "secret").unwrap();

    // The ref is registered.
    assert!(
        store.ref_get("db_password").unwrap().is_some(),
        "file: source must register a store ref under its name"
    );

    // And it hydrates to the original content (single-file tree: <ref>/<name>).
    let dest = dir.join("hydrated");
    lightr_index::hydrate(&dest, &store, "db_password").unwrap();
    let got = std::fs::read(dest.join("db_password")).unwrap();
    assert_eq!(got, b"s3cr3t", "hydrated secret must equal the source file");
}

#[test]
fn external_source_is_not_ingested() {
    // `external: true` ⇒ no ingest (the ref is assumed already registered).
    let dir = uniq_dir("external");
    let store = fresh_store(&dir);
    let sources = vec![FileSource {
        name: "ext_secret".to_string(),
        kind: SourceKind::External,
    }];
    ingest_file_sources(&store, &sources, "secret").unwrap();
    assert!(
        store.ref_get("ext_secret").unwrap().is_none(),
        "external source must not be ingested"
    );
}

#[test]
fn missing_file_source_fails_closed() {
    // A `file:` path that does not exist is an honest Err — no stack spawns.
    let dir = uniq_dir("missing");
    let store = fresh_store(&dir);
    let sources = vec![FileSource {
        name: "gone".to_string(),
        kind: SourceKind::File(dir.join("nope.txt").to_string_lossy().into_owned()),
    }];
    assert!(
        ingest_file_sources(&store, &sources, "secret").is_err(),
        "missing file: source must fail closed"
    );
}

#[test]
fn other_source_is_skipped_without_error() {
    // A source that is neither file: nor external: is flagged + skipped (not a
    // hard error here — the dangling ref fails closed at run, like `lightr run`).
    let dir = uniq_dir("other");
    let store = fresh_store(&dir);
    let sources = vec![FileSource {
        name: "weird".to_string(),
        kind: SourceKind::Other,
    }];
    ingest_file_sources(&store, &sources, "config").unwrap();
    assert!(store.ref_get("weird").unwrap().is_none());
}

#[test]
fn no_sources_is_noop_behavior_preserving() {
    // No top-level sources ⇒ ingestion is a no-op (today's behavior).
    let dir = uniq_dir("noop");
    let store = fresh_store(&dir);
    ingest_file_sources(&store, &[], "secret").unwrap();
    assert!(store.list_refs().unwrap().is_empty());
}
