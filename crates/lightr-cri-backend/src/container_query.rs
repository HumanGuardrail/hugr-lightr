//! Container query plane — `container_status` / `list_containers` reads.
//! Split out of `container.rs` for the godfile cap (WP-CRI-MVP logic, moved
//! verbatim; the trait impl in lib.rs delegates here unchanged).

use crate::util::{container_matches, rec_to_status};
use crate::{BackendError, ContainerFilter, ContainerId, ContainerStatus, LightrBackend, Result};

impl LightrBackend {
    pub(crate) fn container_status_impl(&self, id: &ContainerId) -> Result<ContainerStatus> {
        let cache = self.cache();
        let rec = cache
            .containers
            .get(&id.0)
            .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;
        // Read the sandbox log_dir from the SAME guard (re-locking deadlocks).
        let log_dir = cache.sandbox_log_dir(&rec.sandbox);
        Ok(rec_to_status(rec, &log_dir))
    }

    pub(crate) fn list_containers_impl(
        &self,
        filter: &ContainerFilter,
    ) -> Result<Vec<ContainerStatus>> {
        let cache = self.cache();
        let mut out = Vec::new();
        for r in cache.containers.values() {
            if container_matches(r, filter) {
                let log_dir = cache.sandbox_log_dir(&r.sandbox);
                out.push(rec_to_status(r, &log_dir));
            }
        }
        Ok(out)
    }
}
