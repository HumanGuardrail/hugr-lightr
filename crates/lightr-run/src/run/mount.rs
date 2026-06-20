//! Volume / mount TYPES — frozen by the FREEZE-GATE (parity-contract.md §0
//! R-MOUNT). This module freezes the SHAPES only; the PARSING + RESOLUTION
//! behaviour is WP-VOL-1's job (and the VOL-2..VOL-12 ring).
//!
//! The five Docker mount kinds, the pre-resolution [`MountSpec`] (what a CLI
//! `-v` / `--mount` / `--tmpfs` flag parses into) and the post-resolution
//! [`ResolvedMount`] (what `ExecSpec` carries to the engine) all land here so
//! the dependent WPs transcribe a frozen interface instead of designing one.
//!
//! Absolute-target rule (frozen, behaviour deferred to WP-VOL-1): the `native`
//! engine keeps the relative-CasRef law (targets stay under cwd); the bind
//! variants accept absolute targets under ns/vz.

use crate::run::registry::name_validate;
use lightr_core::{LightrError, Result};

// `MountKind` + `ResolvedMount` are DEFINED in `lightr-engine` (the lower crate
// `ExecSpec` lives in; lightr-run depends on lightr-engine, so the types ExecSpec
// borrows must live there to stay acyclic). R-MOUNT names THIS file as the type
// home, so we re-export them here — this module is the single canonical surface.
pub use lightr_engine::{MountKind, ResolvedMount};

/// A mount BEFORE resolution — the direct parse of one `-v` / `--mount` /
/// `--tmpfs` flag. `source` is `None` for anonymous volumes and tmpfs.
/// `opts` carries the raw, unparsed long-form options (e.g. `ro`, `bind`,
/// `size=64m`); WP-VOL-1 interprets them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountSpec {
    pub kind: MountKind,
    pub source: Option<String>,
    pub target: String,
    pub readonly: bool,
    pub opts: Vec<String>,
}

// `ResolvedMount` (post-resolution, what `ExecSpec` carries) is re-exported
// from `lightr-engine` above. WP-VOL-1 fills how each `MountSpec` resolves to
// one (CasRef hydration, host-path canonicalization, named-volume dir
// allocation, tmpfs sizing).

// ---------------------------------------------------------------------------
// Parser ENTRY POINTS — WP-VOL-1 grammar (parity-contract.md §0 R-MOUNT).
//
// These transcribe Docker's three mount syntaxes into [`MountSpec`]. They are
// PURE (no I/O, no global state) — resolution (CasRef hydration, host-path
// canonicalization, named-volume dir allocation, tmpfs sizing) is the
// VOL-2..VOL-12 ring's job. The absolute-vs-relative target rule is captured
// faithfully but NOT enforced here (that is the materialization WPs).
// ---------------------------------------------------------------------------

/// Is `src` a path (HostBind) rather than a volume name (NamedVolume)?
///
/// Docker's rule: the short-form source is a bind path iff it contains a path
/// separator or begins with `.` (relative) or `~` (home). Otherwise it is a
/// volume name. (Forward `/` is the separator on every Lightr-supported host;
/// `\` is not treated as a separator — see ambiguity note in the return card.)
fn src_is_path(src: &str) -> bool {
    src.contains('/') || src.starts_with('.') || src.starts_with('~')
}

/// Lightr-only prefix marking a source as a content-addressed ref (CasRef).
///
/// Lightr is imageless/CAS-native (CLAUDE.md principle 2): a mount source may
/// name a CAS ref instead of a host path / volume. A leading `@` selects this
/// 4th kind. Disambiguation in `parse_v`: `@`-prefix (CasRef) > path
/// (HostBind) > name (NamedVolume) — the `@` is checked FIRST, so a CAS ref
/// wins even over a name that would otherwise be a valid volume name.
const CAS_REF_PREFIX: char = '@';

/// Resolve a raw mount source string into its [`MountKind`], stripping the
/// `@` marker when the source is a CAS ref. Shared by `parse_v` and
/// `parse_mount_long` so both syntaxes apply the SAME precedence:
/// `@`-prefix (CasRef) > path (HostBind) > name (NamedVolume).
///
/// Returns the kind plus the source to store (sans `@` for a CasRef). Fail-
/// closed on a bare `@` (empty ref) and on an invalid CasRef/volume name —
/// both reuse the existing [`name_validate`] charset.
fn classify_source(value: &str, src: &str) -> Result<(MountKind, String)> {
    if let Some(refname) = src.strip_prefix(CAS_REF_PREFIX) {
        if refname.is_empty() {
            return Err(LightrError::InvalidRef(format!(
                "malformed mount value '{value}': empty CAS ref after '@'"
            )));
        }
        name_validate(refname).map_err(|_| {
            LightrError::InvalidRef(format!(
                "malformed mount value '{value}': invalid CAS ref '{refname}'"
            ))
        })?;
        return Ok((MountKind::CasRef, refname.to_string()));
    }
    if src_is_path(src) {
        return Ok((MountKind::HostBind, src.to_string()));
    }
    // Volume name — validate the Docker charset (reused from registry).
    name_validate(src).map_err(|_| {
        LightrError::InvalidRef(format!(
            "malformed mount value '{value}': invalid volume name '{src}'"
        ))
    })?;
    Ok((MountKind::NamedVolume, src.to_string()))
}

/// Fold one short-form / tmpfs option token into the spec. `ro`/`rw` set the
/// readonly flag; everything else passes through into `opts` verbatim.
fn fold_opt(opt: &str, readonly: &mut bool, opts: &mut Vec<String>) {
    match opt {
        "ro" => *readonly = true,
        "rw" => *readonly = false,
        other => opts.push(other.to_string()),
    }
}

/// Parse a short `-v` / `--volume` flag value: `SRC:DST[:OPTS]` or `DST`.
///
/// - `name:/dst` → [`MountKind::NamedVolume`] (SRC validated as a volume name).
/// - `/host:/dst`, `./rel:/dst`, `~/x:/dst` → [`MountKind::HostBind`].
/// - `/dst` (no SRC) → [`MountKind::AnonVolume`].
///
/// `ro`/`rw` set `readonly`; other OPTS pass through into `opts`. Fail-closed
/// on empty value, empty target, or an invalid named-volume charset.
pub fn parse_v(value: &str) -> Result<MountSpec> {
    if value.is_empty() {
        return Err(LightrError::InvalidRef("empty -v value".to_string()));
    }
    let parts: Vec<&str> = value.split(':').collect();
    let mut readonly = false;
    let mut opts: Vec<String> = Vec::new();

    let (source, target): (Option<String>, &str) = match parts.as_slice() {
        // `/dst` — anonymous volume, no source, no opts.
        [dst] => (None, *dst),
        // `src:dst`
        [src, dst] => (Some((*src).to_string()), *dst),
        // `src:dst:opt[,opt...]` — Docker takes one trailing opts field.
        [src, dst, optstr] => {
            for opt in optstr.split(',').filter(|o| !o.is_empty()) {
                fold_opt(opt, &mut readonly, &mut opts);
            }
            (Some((*src).to_string()), *dst)
        }
        _ => {
            return Err(LightrError::InvalidRef(format!(
                "malformed -v value '{value}': expected SRC:DST[:OPTS] or DST"
            )));
        }
    };

    if target.is_empty() {
        return Err(LightrError::InvalidRef(format!(
            "malformed -v value '{value}': empty target"
        )));
    }

    // Disambiguate SRC: `@`-prefix (CasRef) > path (HostBind) > name
    // (NamedVolume). `classify_source` strips the `@` for a CasRef.
    let (kind, source) = match source {
        None => (MountKind::AnonVolume, None),
        Some(src) => {
            let (k, s) = classify_source(value, &src)?;
            (k, Some(s))
        }
    };

    Ok(MountSpec {
        kind,
        source,
        target: target.to_string(),
        readonly,
        opts,
    })
}

/// Parse a long `--mount type=…,source=…,target=…,readonly[,opt=…]` flag value.
///
/// Recognised keys: `type` → [`MountKind`]; `source`/`src` → source;
/// `target`/`dst`/`destination` → target; `readonly`/`ro` (bare or `=true`).
/// Unknown `key=value` pairs pass through into `opts`. Fail-closed on a missing
/// `target`, an unknown `type=`, or a malformed token.
pub fn parse_mount_long(value: &str) -> Result<MountSpec> {
    if value.is_empty() {
        return Err(LightrError::InvalidRef("empty --mount value".to_string()));
    }

    let mut kind: Option<MountKind> = None;
    let mut source: Option<String> = None;
    let mut target: Option<String> = None;
    let mut readonly = false;
    let mut opts: Vec<String> = Vec::new();

    for tok in value.split(',').filter(|t| !t.is_empty()) {
        let (key, val) = match tok.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (tok, None),
        };
        match key {
            "type" => {
                let v = val.ok_or_else(|| {
                    LightrError::InvalidRef(format!("--mount '{value}': bare 'type'"))
                })?;
                kind = Some(parse_mount_kind(v).ok_or_else(|| {
                    LightrError::InvalidRef(format!("--mount '{value}': unknown type '{v}'"))
                })?);
            }
            "source" | "src" => {
                source = Some(opt_val(value, key, val)?.to_string());
            }
            "target" | "dst" | "destination" => {
                target = Some(opt_val(value, key, val)?.to_string());
            }
            "readonly" | "ro" => match val {
                None | Some("true") | Some("1") => readonly = true,
                Some("false") | Some("0") => readonly = false,
                Some(other) => {
                    return Err(LightrError::InvalidRef(format!(
                        "--mount '{value}': readonly={other} is not a boolean"
                    )));
                }
            },
            // Unknown key — pass the raw token through for the resolution ring.
            _ => opts.push(tok.to_string()),
        }
    }

    let target = target.ok_or_else(|| {
        LightrError::InvalidRef(format!("--mount '{value}': missing required target"))
    })?;
    if target.is_empty() {
        return Err(LightrError::InvalidRef(format!(
            "--mount '{value}': empty target"
        )));
    }

    // A `@`-prefixed source is a CAS ref (the imageless 4th kind) — it wins
    // over `type=`, mirroring `parse_v`'s `@` > path > name precedence and
    // strips the marker. Otherwise: `type=` defaults to `volume` in Docker;
    // with a source it is a named volume, without one it is anonymous;
    // type=bind/tmpfs override.
    let (kind, source) = match source {
        Some(src) if src.starts_with(CAS_REF_PREFIX) => {
            let (k, s) = classify_source(value, &src)?;
            (k, Some(s))
        }
        Some(src) => {
            let k = kind.unwrap_or(MountKind::NamedVolume);
            (k, Some(src))
        }
        None => (kind.unwrap_or(MountKind::AnonVolume), None),
    };

    Ok(MountSpec {
        kind,
        source,
        target,
        readonly,
        opts,
    })
}

/// Map a `--mount type=` value onto a [`MountKind`]. `volume` with a source is
/// resolved to Named vs Anon by the caller (it inspects `source`); here a bare
/// `volume` becomes [`MountKind::NamedVolume`] and the caller has already set
/// the source, so this only needs the literal three Docker types.
fn parse_mount_kind(v: &str) -> Option<MountKind> {
    match v {
        "bind" => Some(MountKind::HostBind),
        "tmpfs" => Some(MountKind::Tmpfs),
        // `volume` maps to NamedVolume; an absent source is downgraded to
        // AnonVolume by the materialization ring (the parser keeps source=None).
        "volume" => Some(MountKind::NamedVolume),
        _ => None,
    }
}

/// Require a non-empty value for a `key=value` long-form token.
fn opt_val<'a>(full: &str, key: &str, val: Option<&'a str>) -> Result<&'a str> {
    match val {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(LightrError::InvalidRef(format!(
            "--mount '{full}': '{key}' needs a value"
        ))),
    }
}

/// Parse a `--tmpfs DST[:OPTS]` flag value → a [`MountKind::Tmpfs`] spec.
///
/// OPTS (e.g. `size=64m`, `mode=1777`) are captured verbatim into `opts` — the
/// materialization ring interprets them. No dedicated size parser exists in the
/// crate to reuse, and Docker itself keeps these opaque at parse time, so they
/// pass through unparsed. Fail-closed on empty value / empty target.
pub fn parse_tmpfs(value: &str) -> Result<MountSpec> {
    if value.is_empty() {
        return Err(LightrError::InvalidRef("empty --tmpfs value".to_string()));
    }
    let (target, optstr) = match value.split_once(':') {
        Some((t, o)) => (t, o),
        None => (value, ""),
    };
    if target.is_empty() {
        return Err(LightrError::InvalidRef(format!(
            "malformed --tmpfs value '{value}': empty target"
        )));
    }
    let opts: Vec<String> = optstr
        .split(',')
        .filter(|o| !o.is_empty())
        .map(|o| o.to_string())
        .collect();

    Ok(MountSpec {
        kind: MountKind::Tmpfs,
        source: None,
        target: target.to_string(),
        readonly: false,
        opts,
    })
}

// Tests live in `run/tests/mount.rs` (house convention: per-module test files
// under `run/tests/`, wired in `run/tests/mod.rs`) to keep this file < 400 LOC.
