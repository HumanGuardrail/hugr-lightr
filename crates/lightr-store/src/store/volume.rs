//! Named-volume registry — daemonless, on-disk (WP-VOL-4, docker-volume parity).
//!
//! Layout (root is INJECTED by the caller — house convention, never read from
//! the global env here, so the unit tests pass a private tempdir and run safely
//! under the multi-threaded CI runner):
//!
//! ```text
//! <root>/volumes/<name>/_data/      ← the volume's data directory
//! <root>/volumes/<name>/meta.json   ← metadata (name, created_at, driver, labels)
//! ```
//!
//! These dirs back [`lightr_engine::MountKind::NamedVolume`] (the VOL-1 type).
//! Today nothing mounts a named volume yet (mount-wiring is VOL-8/9/10), so
//! `in_use` is always false — see [`remove`] and the WP-VOL-5 note. We do NOT
//! fake an in-use signal.
//!
//! `meta.json` is hand-encoded JSON (lightr-store is intentionally serde-free —
//! cf. the refs/ac/imgmeta planes), with a single fixed, well-defined schema:
//! `{"name":"…","created_at":<u64>,"driver":"local","labels":{…}}`.

use lightr_core::{LightrError, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Fixed driver for the local on-disk registry (docker's default driver name).
pub const DRIVER_LOCAL: &str = "local";

/// Metadata for one named volume — the decoded `meta.json` plus the resolved
/// host path of the `_data/` directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Volume name (validated docker charset).
    pub name: String,
    /// Creation time, unix seconds.
    pub created_at: u64,
    /// Driver — always `"local"` for the daemonless on-disk registry.
    pub driver: String,
    /// User labels, sorted by key for deterministic output.
    pub labels: Vec<(String, String)>,
    /// Absolute host path of the volume's `_data/` directory (the mountpoint).
    pub mountpoint: PathBuf,
}

impl VolumeInfo {
    /// Render this record as the canonical single-object JSON (the persisted
    /// `meta.json` fields plus the derived `mountpoint`). Used by the CLI
    /// `inspect`/`ls --json` handlers so the JSON shape has one source of truth.
    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(160);
        s.push('{');
        s.push_str("\"name\":");
        s.push_str(&json_str(&self.name));
        s.push_str(",\"created_at\":");
        s.push_str(&self.created_at.to_string());
        s.push_str(",\"driver\":");
        s.push_str(&json_str(&self.driver));
        s.push_str(",\"mountpoint\":");
        s.push_str(&json_str(&self.mountpoint.to_string_lossy()));
        s.push_str(",\"labels\":");
        s.push_str(&labels_json(&self.labels));
        s.push('}');
        s
    }
}

// ── name validation (docker charset; transcribed — lightr-store cannot depend
// on lightr-run where the mount-side `name_validate` lives) ───────────────────

/// Validate a volume name against docker's rule: `[a-zA-Z0-9][a-zA-Z0-9_.-]*`
/// — non-empty, first char alphanumeric, the rest alphanumeric or `_ . -`.
/// Fail-closed: anything else is rejected. (Same charset as the run-side
/// container-name validator and the `-v` named-volume parser.)
fn name_validate(name: &str) -> Result<()> {
    let mut chars = name.chars();
    match chars.next() {
        None => return Err(LightrError::InvalidRef("empty volume name".to_string())),
        Some(c) if c.is_ascii_alphanumeric() => {}
        Some(_) => {
            return Err(LightrError::InvalidRef(format!(
                "invalid volume name '{name}': must start with [a-zA-Z0-9]"
            )));
        }
    }
    for c in chars {
        let ok = c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
        if !ok {
            return Err(LightrError::InvalidRef(format!(
                "invalid volume name '{name}': only [a-zA-Z0-9_.-] allowed"
            )));
        }
    }
    Ok(())
}

// ── path helpers ──────────────────────────────────────────────────────────────

/// The volumes root: `<root>/volumes`.
fn volumes_root(root: &Path) -> PathBuf {
    root.join("volumes")
}

/// One volume's directory: `<root>/volumes/<name>`.
fn volume_dir(root: &Path, name: &str) -> PathBuf {
    volumes_root(root).join(name)
}

/// One volume's `_data/` directory (the mountpoint).
fn data_dir(root: &Path, name: &str) -> PathBuf {
    volume_dir(root, name).join("_data")
}

/// One volume's `meta.json` path.
fn meta_path(root: &Path, name: &str) -> PathBuf {
    volume_dir(root, name).join("meta.json")
}

// ── verbs (called from the CLI handler) ───────────────────────────────────────

/// Create a named volume under `root`. Errors if the name is invalid (exit-2
/// class) or if a volume of that name already exists (docker:
/// "volume already exists"). `labels` are stored verbatim, sorted by key.
pub fn create(root: &Path, name: &str, labels: &[(String, String)]) -> Result<VolumeInfo> {
    name_validate(name)?;

    let dir = volume_dir(root, name);
    if dir.exists() {
        return Err(LightrError::InvalidRef(format!(
            "volume already exists: {name}"
        )));
    }

    let mut labels = labels.to_vec();
    labels.sort_by(|a, b| a.0.cmp(&b.0));

    let info = VolumeInfo {
        name: name.to_string(),
        created_at: now_unix(),
        driver: DRIVER_LOCAL.to_string(),
        labels,
        mountpoint: data_dir(root, name),
    };

    // _data/ first (creates the parent volume dir too), then meta.json.
    fs::create_dir_all(data_dir(root, name))?;
    fs::write(meta_path(root, name), meta_json(&info))?;

    Ok(info)
}

/// List all volumes under `root`, sorted by name. A volume dir without a
/// readable/parseable `meta.json` is skipped (fail-soft on a partial dir —
/// listing is a view, not the truth). Absent registry ⇒ empty list.
pub fn list(root: &Path) -> Result<Vec<VolumeInfo>> {
    let vroot = volumes_root(root);
    if !vroot.exists() {
        return Ok(vec![]);
    }

    let mut out: Vec<VolumeInfo> = Vec::new();
    let entries = match fs::read_dir(&vroot) {
        Ok(d) => d,
        Err(_) => return Ok(vec![]),
    };
    for entry in entries.filter_map(|e| e.ok()) {
        if !entry.path().is_dir() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue, // non-UTF-8 dir name — skip
        };
        if let Ok(info) = read_info(root, &name) {
            out.push(info);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Inspect one volume. Missing ⇒ `RefNotFound` (docker: "no such volume").
pub fn inspect(root: &Path, name: &str) -> Result<VolumeInfo> {
    name_validate(name)?;
    if !volume_dir(root, name).exists() {
        return Err(LightrError::RefNotFound(format!("no such volume: {name}")));
    }
    read_info(root, name)
}

/// Remove one volume. Missing ⇒ `RefNotFound`. `in_use` true ⇒ refused
/// (docker: "volume is in use"). The caller passes the real ref-count; today
/// nothing mounts named volumes, so the handler passes `false`.
// WP-VOL-5: real refcount — wire `in_use` against running containers here.
pub fn remove(root: &Path, name: &str, in_use: bool) -> Result<()> {
    name_validate(name)?;
    let dir = volume_dir(root, name);
    if !dir.exists() {
        return Err(LightrError::RefNotFound(format!("no such volume: {name}")));
    }
    if in_use {
        return Err(LightrError::InvalidRef(format!("volume is in use: {name}")));
    }
    fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Prune dangling volumes — every volume that is not in use. Returns the names
/// removed, sorted. Today no volume is in use (WP-VOL-5 owns ref-counting), so
/// every volume is dangling and removed.
// WP-VOL-5: real refcount — only prune volumes whose ref-count is zero.
pub fn prune(root: &Path) -> Result<Vec<String>> {
    let mut removed: Vec<String> = Vec::new();
    for info in list(root)? {
        // WP-VOL-5: skip if in_use; today in_use is always false.
        remove(root, &info.name, false)?;
        removed.push(info.name);
    }
    removed.sort();
    Ok(removed)
}

// ── meta.json encode / decode (hand-rolled, fixed schema) ─────────────────────

/// The exact bytes written to `meta.json` (the persisted subset — `mountpoint`
/// is derived from `root`, not stored).
fn meta_json(info: &VolumeInfo) -> Vec<u8> {
    let mut s = String::with_capacity(128);
    s.push('{');
    s.push_str("\"name\":");
    s.push_str(&json_str(&info.name));
    s.push_str(",\"created_at\":");
    s.push_str(&info.created_at.to_string());
    s.push_str(",\"driver\":");
    s.push_str(&json_str(&info.driver));
    s.push_str(",\"labels\":");
    s.push_str(&labels_json(&info.labels));
    s.push('}');
    s.into_bytes()
}

/// Encode the labels map as a JSON object.
fn labels_json(labels: &[(String, String)]) -> String {
    let mut s = String::from("{");
    for (i, (k, v)) in labels.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&json_str(k));
        s.push(':');
        s.push_str(&json_str(v));
    }
    s.push('}');
    s
}

/// Read + decode one volume's `meta.json`, attaching the derived mountpoint.
/// Missing/corrupt meta ⇒ `InvalidManifest`.
fn read_info(root: &Path, name: &str) -> Result<VolumeInfo> {
    let bytes = fs::read(meta_path(root, name))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| LightrError::InvalidManifest(format!("volume {name}: non-UTF-8 meta.json")))?;
    let mut info = parse_meta(&text).ok_or_else(|| {
        LightrError::InvalidManifest(format!("volume {name}: malformed meta.json"))
    })?;
    // mountpoint is derived from `root`, not stored — attach it now.
    info.mountpoint = data_dir(root, name);
    Ok(info)
}

/// Minimal parser for the fixed `meta.json` schema this module writes. Returns
/// `None` on any deviation (fail-closed — we only ever read what we wrote). The
/// `mountpoint` is derived by the caller, so it is left empty here.
fn parse_meta(text: &str) -> Option<VolumeInfo> {
    Some(VolumeInfo {
        name: json_field_str(text, "\"name\":")?,
        created_at: json_field_u64(text, "\"created_at\":")?,
        driver: json_field_str(text, "\"driver\":")?,
        labels: json_field_labels(text)?,
        mountpoint: PathBuf::new(),
    })
}

/// Extract a string field value following `key` (e.g. `"name":`). Handles the
/// `\"` and `\\` escapes this module emits.
fn json_field_str(text: &str, key: &str) -> Option<String> {
    let start = text.find(key)? + key.len();
    let rest = text[start..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

/// Extract an unsigned-integer field value following `key`.
fn json_field_u64(text: &str, key: &str) -> Option<u64> {
    let start = text.find(key)? + key.len();
    let rest = text[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}

/// Parse the `"labels":{ … }` object into sorted key/value pairs.
fn json_field_labels(text: &str) -> Option<Vec<(String, String)>> {
    let start = text.find("\"labels\":")? + "\"labels\":".len();
    let rest = text[start..].trim_start();
    let rest = rest.strip_prefix('{')?;
    let end = rest.find('}')?;
    let body = &rest[..end];
    let mut out: Vec<(String, String)> = Vec::new();
    if body.trim().is_empty() {
        return Some(out);
    }
    // Schema guarantee: label keys/values are simple quoted strings whose only
    // escapes are `\"`/`\\`, joined by top-level `,` with a single `:` per pair
    // (labels come from the CLI as k=v). Split on `,` then the first `:`.
    for pair in body.split(',') {
        let (k, v) = pair.split_once(':')?;
        let k = unquote(k.trim())?;
        let v = unquote(v.trim())?;
        out.push((k, v));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// Strip surrounding quotes + unescape a JSON string token.
fn unquote(tok: &str) -> Option<String> {
    let inner = tok.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    Some(out)
}

/// JSON-escape a string and wrap it in quotes. Names are a restricted charset;
/// labels are arbitrary user strings, so we escape `"` and `\` defensively.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Wall-clock now, unix seconds (0 if the clock is before the epoch).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
