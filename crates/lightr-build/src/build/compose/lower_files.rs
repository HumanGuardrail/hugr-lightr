//! SKELETON-FREEZE (per-aspect, files/process-shape group): lowering for the
//! compose service fields that reference the top-level `secrets:`/`configs:`
//! blocks (full compose-spec form) plus the process-shape aspects `entrypoint`
//! and `stop_grace_period`.
//!
//! Every aspect here is an honest no-op stub: each field is frozen in the model
//! but the runtime `Service` carries no slot for it yet (the Lightr `name=ref`
//! extension for secrets/configs is lowered separately in `lower.rs`). A future
//! compose-feature WP fills EXACTLY ONE stub body (and widens `model.rs` for its
//! target field), touching no sibling aspect. See `lower_stubs.rs` for the group
//! facade and the stub-filling convention; the `_` bindings document an
//! intentionally-unconsumed source field (no `#[allow(unused)]`, no debt).
use super::model::Service;
use super::spec::{ServiceDef, ServiceFileRef, ServiceFileRefLong, StringOrList};
use indexmap::IndexMap;
use serde_yaml::Value;

/// WP-CMP-SECRETS-FULL: a lowered top-level `secrets:`/`configs:` source — the
/// resolved kind of one named entry under the compose file's top-level block.
///
/// The up-path (`up.rs`) ingests a [`SourceKind::File`] into the Store under the
/// source NAME as the ref so a service's `(name, source)` `StoreFile` resolves at
/// run; a [`SourceKind::External`] is assumed to be an already-registered ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSource {
    /// The source name (the key under the top-level `secrets:`/`configs:` map).
    pub name: String,
    /// Where the content comes from.
    pub kind: SourceKind,
}

/// WP-CMP-SECRETS-FULL: the resolved kind of a top-level source entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// `file: <path>` — host file ingested into the Store at up.
    File(String),
    /// `external: true` — the source is an already-registered store ref.
    External,
    /// Neither `file:` nor `external:` — carried so the up-path can flag it.
    Other,
}

/// WP-CMP-SECRETS-FULL: lower a top-level `secrets:`/`configs:` map (raw `Value`
/// per entry, as frozen in `ComposeSpec`) into typed [`FileSource`]s.
///
/// `{file: <path>}` ⇒ [`SourceKind::File`]; `{external: true}` (or
/// `{external: {...}}`) ⇒ [`SourceKind::External`]; anything else ⇒
/// [`SourceKind::Other`] (carried, never silently dropped). Declaration order is
/// preserved (the source map is an `IndexMap`).
pub fn lower_top_sources(map: &IndexMap<String, Value>) -> Vec<FileSource> {
    map.iter()
        .map(|(name, v)| FileSource {
            name: name.clone(),
            kind: classify_source(v),
        })
        .collect()
}

/// Classify one top-level source `Value` into a [`SourceKind`].
fn classify_source(v: &Value) -> SourceKind {
    if let Value::Mapping(m) = v {
        if let Some(Value::String(p)) = m.get(Value::String("file".into())) {
            return SourceKind::File(p.clone());
        }
        match m.get(Value::String("external".into())) {
            Some(Value::Bool(true)) | Some(Value::Mapping(_)) => return SourceKind::External,
            _ => {}
        }
    }
    SourceKind::Other
}

/// WP-CMP-SECRETS-FULL: lower a service's `secrets:`/`configs:` refs into the
/// run's store-backed channel as `(name, ref)` pairs (`svc.secrets`/
/// `svc.configs` → `ServiceSpec` → `RunSpec.secrets/configs: Vec<StoreFile>`,
/// hydrated at run via `lightr_index::hydrate`).
///
/// Three shapes resolve (`kind` is `"secret"`/`"config"` for diagnostics):
///  * legacy Lightr SHORT `name=ref` ⇒ `(name, ref)` verbatim (unchanged from
///    the old `lower_pairs` — behavior-preserving).
///  * compose SHORT (a bare source name) ⇒ `(name, name)` — the source name is
///    BOTH the file name presented at run AND the store ref. The top-level
///    `file:`/`external` source under that name is resolved/ingested by `up.rs`.
///  * compose LONG `{source, target, uid, gid, mode}` ⇒ `(file_name, source)`,
///    where `file_name` is the basename of `target` (falling back to `source`).
///
/// AMBIGUITIES (flagged): the run-side `StoreFile { name, ref_name }` carries NO
/// `uid`/`gid`/`mode` slot and NO arbitrary target PATH — so the long form's
/// `uid`/`gid`/`mode` are parsed but NOT honored, and `target` collapses to its
/// basename (the hydrate path is `<cwd>/.lightr/{secrets,configs}/<name>` with a
/// fixed 0600/0644 mode). A long entry with no `source` is dropped fail-loud.
///
/// Behavior-preserving: an empty list ⇒ empty channel.
pub(super) fn lower_service_file_refs(
    refs: &[ServiceFileRef],
    kind: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        match r {
            ServiceFileRef::Short(s) => {
                if let Some(pair) = lower_short(s) {
                    out.push(pair);
                }
            }
            ServiceFileRef::Long(l) => {
                if let Some(pair) = lower_long(l, kind) {
                    out.push(pair);
                }
            }
        }
    }
    out
}

/// Resolve a SHORT scalar ref. `name=ref` (legacy) ⇒ `(name, ref)`; a bare name
/// ⇒ `(name, name)` (the source name doubles as the store ref). Empty ⇒ dropped.
fn lower_short(s: &str) -> Option<(String, String)> {
    let s = s.trim().trim_matches('"').trim();
    if s.is_empty() {
        return None;
    }
    if let Some((n, r)) = s.split_once('=') {
        return Some((n.trim().to_string(), r.trim().to_string()));
    }
    Some((s.to_string(), s.to_string()))
}

/// Resolve a LONG `{source, target, ...}` ref to `(file_name, source)`. The
/// file name is the basename of `target` (else `source`). A missing `source` is
/// fail-loud (stderr) and dropped — never silently materialized empty.
fn lower_long(l: &ServiceFileRefLong, kind: &str) -> Option<(String, String)> {
    let Some(source) = l.source.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        eprintln!("lightr compose: long {kind} ref has no `source`; ignored");
        return None;
    };
    let name = l
        .target
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .and_then(|t| t.rsplit('/').next())
        .filter(|b| !b.is_empty())
        .unwrap_or(source);
    Some((name.to_string(), source.to_string()))
}

/// `stop_grace_period`: graceful-stop window before SIGKILL.
///
/// WP-A: LOWERED-TO-NOOP (run-side gap). The compose teardown/`lightr stop` path
/// uses a FIXED grace window and the run-side `RunSpec` carries no stop-grace
/// slot — so there is nothing to lower onto without widening a non-owned surface
/// (`RunSpec` lives in `lightr-run`). Kept an honest no-op (the fixed grace is
/// today's behavior, behavior-preserving); the `_` binding documents the
/// intentionally-unconsumed source field.
pub(super) fn lower_stop_grace_period(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.stop_grace_period; // run side: RunSpec lacks a stop-grace slot
}

/// `entrypoint`: override the image entrypoint.
///
/// WP-A: lowers the compose `entrypoint` onto `svc.entrypoint`; the supervisor
/// threads it into `RunSpec.entrypoint`, which prepends it to `command` at exec
/// (Docker semantics). Mirrors `lower_command` (`lower.rs`): an EXEC-form list
/// is taken as argv as-is; a SHELL string becomes `["/bin/sh", "-c", <str>]`.
/// Absent ⇒ `None` ⇒ no override (today's behavior).
pub(super) fn lower_entrypoint(def: &ServiceDef, svc: &mut Service) {
    svc.entrypoint = def.entrypoint.as_ref().map(|e| match e {
        StringOrList::String(s) => vec!["/bin/sh".to_string(), "-c".to_string(), s.clone()],
        StringOrList::List(v) => v.clone(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short(s: &str) -> ServiceFileRef {
        ServiceFileRef::Short(s.to_string())
    }

    #[test]
    fn legacy_name_eq_ref_preserved() {
        // Behavior-preserving: `name=ref` lowers verbatim (old `lower_pairs`).
        let out = lower_service_file_refs(&[short("db_pw=storeref")], "secret");
        assert_eq!(out, vec![("db_pw".to_string(), "storeref".to_string())]);
    }

    #[test]
    fn compose_short_name_doubles_as_ref() {
        // A bare source name ⇒ `(name, name)` (source name is the store ref).
        let out = lower_service_file_refs(&[short("db_password")], "secret");
        assert_eq!(
            out,
            vec![("db_password".to_string(), "db_password".to_string())]
        );
    }

    #[test]
    fn long_form_maps_target_basename_to_name_source_to_ref() {
        let long = ServiceFileRef::Long(ServiceFileRefLong {
            source: Some("db_password".to_string()),
            target: Some("/run/secrets/dbpw".to_string()),
            uid: Some("103".to_string()),
            gid: Some("103".to_string()),
            mode: None,
        });
        let out = lower_service_file_refs(&[long], "secret");
        // name == basename(target), ref == source; uid/gid/mode dropped.
        assert_eq!(out, vec![("dbpw".to_string(), "db_password".to_string())]);
    }

    #[test]
    fn long_form_without_target_uses_source_as_name() {
        let long = ServiceFileRef::Long(ServiceFileRefLong {
            source: Some("cfg".to_string()),
            ..Default::default()
        });
        let out = lower_service_file_refs(&[long], "config");
        assert_eq!(out, vec![("cfg".to_string(), "cfg".to_string())]);
    }

    #[test]
    fn long_form_without_source_is_dropped() {
        let long = ServiceFileRef::Long(ServiceFileRefLong::default());
        assert!(lower_service_file_refs(&[long], "secret").is_empty());
    }

    #[test]
    fn empty_refs_is_empty_channel() {
        assert!(lower_service_file_refs(&[], "secret").is_empty());
    }

    #[test]
    fn classify_top_sources_file_external_other() {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert(
            "f".to_string(),
            serde_yaml::from_str("{ file: /etc/pw }").unwrap(),
        );
        m.insert(
            "e".to_string(),
            serde_yaml::from_str("{ external: true }").unwrap(),
        );
        m.insert(
            "o".to_string(),
            serde_yaml::from_str("{ environment: FOO }").unwrap(),
        );
        let out = lower_top_sources(&m);
        assert_eq!(out[0].kind, SourceKind::File("/etc/pw".to_string()));
        assert_eq!(out[1].kind, SourceKind::External);
        assert_eq!(out[2].kind, SourceKind::Other);
        // Declaration order preserved.
        assert_eq!(out[0].name, "f");
        assert_eq!(out[2].name, "o");
    }
}
