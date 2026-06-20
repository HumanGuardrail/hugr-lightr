//! Public report types + JSON shapes for OCI index / manifest.

use lightr_core::Digest;
use serde::Deserialize;

// ─────────────────────────────────────────────────────────────────────────────
// Public contract types
// ─────────────────────────────────────────────────────────────────────────────

pub struct ImportReport {
    pub name: String,
    pub root: Digest,
    pub layers: u64,
    pub files: u64,
}

/// Result of a `push`: the registry target written, the synthesized OCI image
/// manifest's sha256 digest, the layer count (always 1 — see `push`), and the
/// gzipped layer size in bytes.
#[derive(Debug)]
pub struct PushReport {
    pub target: String,
    pub manifest_digest: String,
    pub layers: u64,
    pub size: u64,
}

/// Result of a `save` (WP-IMG-04): where the tar was written (`-` for stdout),
/// the layer count, the tar's total byte size, and whether the export was
/// FAITHFUL (verbatim from a retained record) or a SYNTHESIZED single-layer
/// fallback (lossy — no record). The caller reports `faithful` honestly.
#[derive(Debug)]
pub struct SaveReport {
    pub destination: String,
    pub layers: u64,
    pub size: u64,
    pub faithful: bool,
}

/// Result of a `load` (WP-IMG-05): the underlying [`ImportReport`] (root digest,
/// layer/file counts) plus the resolved local ref `name` the image was tagged
/// under (from the tar's `RepoTags`, or a deterministic fallback) and whether
/// that name came FROM the tar (`tagged_from_tar = true`) or was synthesized
/// because the save carried no tag (`false` — Docker-faithful tag-less load).
#[derive(Debug)]
pub struct LoadReport {
    pub name: String,
    pub root: Digest,
    pub layers: u64,
    pub files: u64,
    pub tagged_from_tar: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON shapes for OCI index / manifest
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Default)]
pub(super) struct OciDescriptor {
    #[serde(default)]
    pub(super) digest: String,
    // media_type drives content-type routing in pull() and is retained verbatim
    // in the WP-IMG-01 ImageManifestRecord descriptor (faithful push fidelity).
    #[serde(rename = "mediaType", default)]
    pub(super) media_type: String,
    // size is the OCI descriptor's declared length; retained in the WP-IMG-01
    // descriptor. Content integrity is verified via sha256, not size.
    #[serde(default)]
    pub(super) size: u64,
    #[serde(default)]
    pub(super) platform: Option<OciPlatform>,
}

#[derive(Deserialize, Debug)]
pub(super) struct OciPlatform {
    pub(super) os: String,
    pub(super) architecture: String,
}

#[derive(Deserialize)]
pub(super) struct OciIndex {
    pub(super) manifests: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
pub(super) struct OciManifest {
    pub(super) layers: Vec<OciDescriptor>,
    /// The image config descriptor (entrypoint/cmd/env/os/arch live in this
    /// blob). Captured at pull/import + stored via `Store::image_config_put` so
    /// `oci push` re-emits a runnable image. `#[serde(default)]`: a manifest
    /// without it (or an unparsable one) yields an empty descriptor → skipped.
    #[serde(default)]
    pub(super) config: OciDescriptor,
}

// docker-save manifest.json item
#[derive(Deserialize)]
pub(super) struct DockerSaveItem {
    #[serde(rename = "Layers")]
    pub(super) layers: Vec<String>,
    /// Path (within the tar) of the image config JSON — `<hex>.json` (legacy) or
    /// `blobs/sha256/<hex>` (modern/OCI-layout export). Captured for push-fidelity
    /// (entrypoint/cmd/env). `#[serde(default)]`: absent ⇒ push falls back.
    #[serde(rename = "Config", default)]
    pub(super) config: String,
    /// Repo:tag names this image was saved under (`docker save`'s `RepoTags`).
    /// `oci load` uses the first entry to name the loaded ref (Docker-faithful —
    /// `docker load` re-tags from RepoTags). `#[serde(default)]`: a tag-less save
    /// (`docker save <id>`) yields an empty list ⇒ `oci load` falls back to a
    /// deterministic name (see `load.rs`).
    #[serde(rename = "RepoTags", default)]
    pub(super) repo_tags: Vec<String>,
}

// OCI distribution API responses
#[derive(Deserialize)]
pub(super) struct TokenResponse {
    pub(super) token: String,
}

#[derive(Deserialize)]
pub(super) struct ManifestList {
    pub(super) manifests: Vec<OciDescriptor>,
}
