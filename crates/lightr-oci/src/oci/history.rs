//! WP-IMG-08 — `oci history`: per-layer image history (docker `history`).
//!
//! Reproduces `docker history`'s shape from the OCI image config we RETAIN at
//! pull/import (WP-IMG-01: the config sidecar via [`Store::image_config_get`]).
//! An OCI image config carries an ordered `history` array — one entry per build
//! step — where every entry has a `created_by` (the instruction that produced
//! it) and may be flagged `empty_layer: true` (a metadata-only step that added
//! no filesystem layer, e.g. `ENV`/`CMD`). The non-empty-layer entries map 1:1,
//! in order, to the image's actual layers; the empty ones have size 0. This is
//! exactly how docker assigns the SIZE column.
//!
//! ## Where SIZE comes from (positional layer mapping)
//!
//! The retained [`ImageManifestRecord`] holds the ordered descriptors as they
//! appear in the manifest: descriptor[0] is the CONFIG, the rest are the LAYERS
//! in order. We walk the config's `history` array; each non-`empty_layer` entry
//! consumes the next layer descriptor and takes that descriptor's `size`. Empty
//! entries take size 0. If the layer descriptors run out (a malformed/mismatched
//! record), remaining non-empty entries report `<missing>` size honestly rather
//! than lying with a zero.
//!
//! ## `<missing>` — honest about absent history
//!
//! Docker prints `<missing>` in the CREATED-BY position for layers it has no
//! build provenance for (squashed/imported images). We mirror that honesty in
//! two ways:
//!   * a config with NO `history` array (e.g. a `snapshot`'d ref, or an image
//!     imported without history) ⇒ one row PER retained layer, each CREATED-BY
//!     `<missing>` with its positional size;
//!   * a single history entry missing its `created_by` ⇒ that field is
//!     `<missing>` for that row.
//!
//! Fail-closed: an absent ref is [`LightrError::RefNotFound`] (exit 2). A ref
//! that exists but has neither a config nor a manifest record (no provenance at
//! all) surfaces [`LightrError::InvalidManifest`] (no history available), never
//! a silent empty table.

use lightr_core::{LightrError, Result};
use lightr_store::{ImageManifestRecord, Store};
use serde::Deserialize;

/// Sentinel docker prints when it has no value for a column.
pub const MISSING: &str = "<missing>";

/// One rendered history row (docker `history` shape).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    /// The build instruction (`created_by`), or [`MISSING`] when absent.
    pub created_by: String,
    /// The layer's byte size. `None` ⇒ size unknown ([`MISSING`] when rendered),
    /// `Some(0)` ⇒ an empty (metadata-only) layer.
    pub size: Option<u64>,
    /// True iff this is an `empty_layer` (metadata-only) step — no filesystem
    /// layer was produced, so SIZE is 0.
    pub empty_layer: bool,
}

/// The slice of the OCI image config we read for history.
#[derive(Deserialize, Default)]
struct ConfigHistory {
    #[serde(default)]
    history: Vec<ConfigHistoryEntry>,
}

#[derive(Deserialize, Default)]
struct ConfigHistoryEntry {
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    empty_layer: bool,
}

/// Build the per-layer history rows for image `name`, newest-first like docker.
///
/// Resolution order (fail-closed):
///   1. absent ref ⇒ [`LightrError::RefNotFound`] (exit 2);
///   2. read the retained config + manifest record;
///   3. with a `history` array ⇒ one row per entry (size from the positional
///      layer descriptor, 0 for empty layers, [`MISSING`] when descriptors
///      run short);
///   4. no `history` but a manifest record ⇒ one `<missing>` row per layer;
///   5. neither config nor manifest ⇒ [`LightrError::InvalidManifest`]
///      ("no history available") — never a silent empty table.
///
/// Returned rows are ordered newest-layer-first (docker's order); the caller
/// renders them top-to-bottom.
pub fn image_history(store: &Store, name: &str) -> Result<Vec<HistoryRow>> {
    // 1. Fail-closed on an absent ref (exit 2). An empty name is an absent ref.
    if store.ref_get(name)?.is_none() {
        return Err(LightrError::RefNotFound(name.to_string()));
    }

    let config = store.image_config_get(name)?;
    let record = store.image_manifest_get(name)?;
    let layer_sizes = layer_sizes(record.as_ref());

    // Parse the config's history array (fail-soft: a corrupt config ⇒ treated as
    // having no history, so we fall through to the manifest-record path).
    let history = config
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<ConfigHistory>(bytes).ok())
        .map(|c| c.history)
        .unwrap_or_default();

    let mut rows = if !history.is_empty() {
        rows_from_history(&history, &layer_sizes)
    } else if record.is_some() {
        // No history array but we DO have retained layers ⇒ honest `<missing>`
        // per layer (squashed/imported-without-history image).
        layer_sizes
            .iter()
            .map(|&size| HistoryRow {
                created_by: MISSING.to_string(),
                size: Some(size),
                empty_layer: false,
            })
            .collect()
    } else {
        // No provenance at all — never lie with an empty table.
        return Err(LightrError::InvalidManifest(format!(
            "no history available for image {name} (no retained config or manifest)"
        )));
    };

    // Docker prints newest layer first.
    rows.reverse();
    Ok(rows)
}

/// Map the config `history` (build order, oldest-first) onto the positional
/// layer descriptors. Non-empty entries consume the next layer size in order;
/// empty entries take size 0; entries past the available layers report `None`
/// (rendered `<missing>`) rather than a fabricated zero.
fn rows_from_history(history: &[ConfigHistoryEntry], layer_sizes: &[u64]) -> Vec<HistoryRow> {
    let mut layers = layer_sizes.iter();
    history
        .iter()
        .map(|e| {
            let created_by = e
                .created_by
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| MISSING.to_string());
            let size = if e.empty_layer {
                Some(0)
            } else {
                // Consume the next layer; if descriptors run short, size unknown.
                layers.next().copied()
            };
            HistoryRow {
                created_by,
                size,
                empty_layer: e.empty_layer,
            }
        })
        .collect()
}

/// Extract the ordered LAYER sizes from the retained record: descriptor[0] is
/// the config, the rest are the layers in order. Absent record ⇒ no sizes.
fn layer_sizes(record: Option<&ImageManifestRecord>) -> Vec<u64> {
    match record {
        Some(rec) if !rec.descriptors.is_empty() => {
            // Skip the config descriptor (index 0); the rest are layers.
            rec.descriptors[1..].iter().map(|d| d.size).collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
#[path = "tests/history_tests.rs"]
mod history_tests;
