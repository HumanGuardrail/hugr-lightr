//! lightr-views — O(1) materialization (ADR-0013), the other half of the
//! perf headline. The PLAN + SOLIDIFIER logic is pure and host-tested; real
//! mount backends (composefs/EROFS on Linux, NFS-loopback on macOS — the
//! EdenFS-proven route) are `cfg`-gated and marked `// VIEW-PATH (S1/S3)`:
//! they compile here, runtime validation is the S1/S3 spike on a target box.
//! NO unmeasured runtime claim. Bodies: WP-W5.

use lightr_core::Manifest;
use std::path::Path;

/// A plan to present a manifest as a virtual view — appears in O(1),
/// entries fault in lazily; the solidifier promotes hot ones to real CoW.
pub struct ViewPlan {
    _entries: Vec<String>,
}

/// Build a view plan from a manifest (every entry path, lazy).
pub fn plan_view(_manifest: &Manifest) -> ViewPlan {
    todo!("W5: collect entry paths in manifest order")
}

/// OS actions a view backend performs — seamed for host testing.
pub trait ViewBackend {
    fn mount(&mut self, plan: &ViewPlan, at: &Path) -> std::io::Result<()>;
    fn fault_in(&mut self, path: &str) -> std::io::Result<()>;
    fn unmount(&mut self, at: &Path) -> std::io::Result<()>;
}

/// Promote-on-access policy: decides which entries to CoW-clone to real disk
/// and when the mount can evaporate. Pure + fully unit-tested.
pub struct Solidifier {
    _todo: (),
}

impl Solidifier {
    pub fn new(_plan: &ViewPlan) -> Self {
        todo!("W5")
    }
    pub fn record_access(&mut self, _path: &str) {
        todo!("W5")
    }
    pub fn next_to_promote(&mut self) -> Option<String> {
        todo!("W5")
    }
    pub fn is_fully_solid(&self) -> bool {
        todo!("W5")
    }
}
