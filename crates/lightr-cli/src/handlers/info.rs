//! `lightr info` handler — docker-info parity onto the daemonless CAS model.
//!
//! Docker's `info` describes a running engine (containers, images, driver,
//! the server daemon). lightr has no daemon, so this reports the honest
//! local facts: the CAS store root, default engine, ref/image count, total
//! CAS footprint, Action-Cache entry count, and the principle-#1 truth that
//! there is no running daemon.
//!
//! Human output:
//! ```text
//! Store root:     /home/u/.lightr/store
//! Default engine: native
//! Images (refs):  3
//! CAS objects:    42
//! CAS size:       4.2MB
//! Build cache:    7 entries
//! Daemonless:     true (no running daemon)
//! ```
//! `--json` output (stable keys):
//! ```json
//! {"store_root":"...","default_engine":"native","images":3,
//!  "cas_objects":42,"cas_bytes":4200000,"build_cache_entries":7,
//!  "daemonless":true}
//! ```

use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;
use crate::handlers::system::human_size;

/// lightr's default execution engine. `native` (reproducibility, not a sandbox)
/// is the floor every host supports; `engine ls` reports per-host availability.
const DEFAULT_ENGINE: &str = "native";

#[derive(Serialize)]
pub(crate) struct InfoJson {
    /// Absolute path of the CAS store root.
    store_root: String,
    /// Default engine selected when `--engine` is omitted.
    default_engine: &'static str,
    /// Number of named refs (a lightr "image" = a named ref).
    images: u64,
    /// Number of distinct CAS objects on disk.
    cas_objects: u64,
    /// Total CAS footprint in bytes.
    cas_bytes: u64,
    /// Number of Action-Cache (build-cache) entries.
    build_cache_entries: u64,
    /// Principle #1: lightr runs no daemon. Always true.
    daemonless: bool,
}

/// Collect the info facts from an already-open `store`. Read-only; takes no
/// lock. Factored out so tests can drive it against a tempdir store without
/// touching process-global env (parallel-safe).
pub(crate) fn gather(store: &Store) -> lightr_core::Result<InfoJson> {
    let usage = store.store_usage()?;
    Ok(InfoJson {
        store_root: store.root().display().to_string(),
        default_engine: DEFAULT_ENGINE,
        images: store.list_refs()?.len() as u64,
        cas_objects: usage.objects,
        cas_bytes: usage.bytes,
        build_cache_entries: store.list_ac()?.len() as u64,
        daemonless: true,
    })
}

/// Run `lightr info`. Exit 0 on success; 1 on store I/O error (die_lightr).
pub fn run(json: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };
    let info = match gather(&store) {
        Ok(i) => i,
        Err(e) => return die_lightr(&e),
    };

    if json {
        println!("{}", serde_json::to_string(&info).expect("serialize info"));
    } else {
        println!("Store root:     {}", info.store_root);
        println!("Default engine: {}", info.default_engine);
        println!("Images (refs):  {}", info.images);
        println!("CAS objects:    {}", info.cas_objects);
        println!("CAS size:       {}", human_size(info.cas_bytes));
        println!("Build cache:    {} entries", info.build_cache_entries);
        println!("Daemonless:     true (no running daemon)");
    }
    0
}

#[cfg(test)]
#[path = "info_tests.rs"]
mod tests;
