//! lightr-core — frozen contract: build-spec v2 §3 (ADR-0009/0004).
//! Types are the contract; method bodies are WP-1.
#![forbid(unsafe_code)]

mod core;

pub use core::consts::{MANIFEST_MAGIC, OUTPUT_CAP_BYTES, REF_KEY_DOMAIN};
pub use core::digest::Digest;
pub use core::error::{LightrError, Result};
pub use core::limits::ResourceLimits;
pub use core::manifest::{Entry, Manifest};
pub use core::refrecord::{ref_key, validate_ref_name, RefRecord};
