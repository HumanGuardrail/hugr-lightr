//! WP-C: `FROM --platform=os/arch[/variant]` resolution + validation.
//!
//! Docker's `FROM --platform=<p>` selects which platform of the base image to
//! build FROM. lightr's OCI import is **single-arch** (one
//! [`ImageManifestRecord`](lightr_store::ImageManifestRecord) per ref — see
//! `lightr-oci/src/oci/retain.rs`), so there is NO manifest list to pick a
//! sub-manifest from at build time. The honest contract is therefore:
//!
//! - **No `--platform`** → the platform is the HOST (`host_platform()`),
//!   exactly preserving the pre-WP behavior (the base is hydrated as-is).
//! - **`--platform=<p>`** → validate `<p>` against the base image's ACTUAL
//!   platform (the `platform` field the importer recorded). If the base records
//!   a concrete, non-empty platform that does NOT match `<p>` → a hard,
//!   fail-closed error (no silent ignore). If the base records no platform
//!   (lightr-built base / `scratch` / an import predating push-fidelity), there
//!   is nothing to contradict, so the request is accepted (the host materializes
//!   it) — same as Docker pulling a single-arch image that happens to match.
//!
//! Either way the RESOLVED platform string folds into the build memo key
//! (`memo::step_key`) so two builds for different platforms never collide or
//! falsely cache-hit.

use lightr_core::{LightrError, Result};
use lightr_store::Store;

/// The host platform as an OCI `os/arch` string (e.g. `linux/amd64`).
///
/// lightr workspaces run a **Linux** userland regardless of the host kernel
/// (the runtime model — Linux base images), so the OS component is always
/// `linux`; the arch maps `std::env::consts::ARCH` to the OCI arch token
/// (`x86_64`→`amd64`, `aarch64`→`arm64`, else passthrough), mirroring
/// `lightr-oci`'s `host_arch()` (which is `pub(super)` there, so it is
/// re-derived here rather than imported across the crate boundary).
pub(crate) fn host_platform() -> String {
    format!("linux/{}", host_arch())
}

/// Map `std::env::consts::ARCH` → the OCI architecture token.
fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Normalize an OCI platform string for comparison: lowercase, and treat a bare
/// `arch` (no `/`) as `linux/<arch>` (Docker defaults the OS to the daemon's OS,
/// which for lightr is always `linux`). A trailing `/variant` is preserved.
fn normalize(p: &str) -> String {
    let p = p.trim().to_ascii_lowercase();
    if p.contains('/') {
        p
    } else {
        format!("linux/{p}")
    }
}

/// Two platform strings match if their normalized forms are equal, OR if their
/// `os/arch` pairs are equal while one side omits the `/variant` (Docker treats
/// an unspecified variant as compatible with the base's default variant). The
/// `os/arch` pair must always match exactly.
fn platforms_match(requested: &str, actual: &str) -> bool {
    let (r, a) = (normalize(requested), normalize(actual));
    if r == a {
        return true;
    }
    // Compare os/arch ignoring an absent variant on either side.
    let r_parts: Vec<&str> = r.splitn(3, '/').collect();
    let a_parts: Vec<&str> = a.splitn(3, '/').collect();
    if r_parts.len() < 2 || a_parts.len() < 2 {
        return false;
    }
    r_parts[0] == a_parts[0] && r_parts[1] == a_parts[1]
}

/// Resolve the platform for a `FROM` instruction.
///
/// `flag` is the parsed `--platform=<p>` value (already `${VAR}`-interpolated by
/// the caller), `None` when absent. Returns the RESOLVED platform string that
/// folds into the memo key: the requested platform (normalized) when present,
/// else the host platform.
pub(crate) fn resolve_platform(flag: Option<&str>) -> String {
    match flag {
        Some(p) => normalize(p),
        None => host_platform(),
    }
}

/// Validate a requested `--platform` against the base image's ACTUAL platform.
///
/// `image_ref` is the (interpolated) base ref; `requested` is the parsed
/// `--platform` value. A `None` request, a `scratch` base, or a base with no
/// recorded platform validates trivially (nothing to contradict). A base whose
/// recorded platform is non-empty and does NOT match the request is a hard,
/// fail-closed error — lightr cannot synthesize a platform its single-arch
/// import never captured, so it refuses honestly rather than silently building
/// the wrong arch.
pub(crate) fn validate_against_base(
    store: &Store,
    image_ref: &str,
    requested: Option<&str>,
) -> Result<()> {
    let Some(req) = requested else {
        return Ok(());
    };
    if image_ref == "scratch" {
        return Ok(());
    }
    let actual = match store.image_manifest_get(image_ref)? {
        Some(rec) if !rec.platform.trim().is_empty() => rec.platform,
        // No manifest record, or an unspecified platform — nothing to contradict.
        _ => return Ok(()),
    };
    if platforms_match(req, &actual) {
        Ok(())
    } else {
        Err(LightrError::InvalidManifest(format!(
            "FROM --platform={req}: base image {image_ref:?} is {actual:?} \
             (lightr's OCI import is single-arch — it cannot select or synthesize \
             a different platform); requested and actual platform must match"
        )))
    }
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
