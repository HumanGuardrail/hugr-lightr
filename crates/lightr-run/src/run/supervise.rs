//! Process supervisor: `supervise` dispatches between the vz container path
//! (`svz::supervise_vz`) and the native path (`supervise_native::supervise_native`,
//! which owns the WP-RC-RESTART re-spawn loop). The Windows named-pipe
//! control-server helpers live in `supervise_win.rs` (mod `win`), shared by the
//! native path via `#[path]`.

use lightr_core::Result;
use lightr_store::Store;
use std::path::PathBuf;

use super::paths::{lightr_home, read_spec_on_disk};
use super::supervise_native::supervise_native;
use super::svz::supervise_vz;

pub fn supervise(dir: &std::path::Path) -> Result<i32> {
    let spec = read_spec_on_disk(dir)?;
    let _cwd = PathBuf::from(&spec.cwd);

    // We need a store for mount hydration — open from LIGHTR_HOME.
    let store_root = lightr_home().join("store");
    let store = Store::open(&store_root)?;

    // WP-NET2: a vz container run (engine "vz" + a rootfs ref) boots a Linux
    // microVM in this supervisor process and forwards each published port to the
    // guest's DHCP IP, instead of spawning a host child. The native path below
    // is the unchanged host-process supervisor (now with the WP-RC-RESTART
    // re-spawn loop). Selected by the engine field written at spawn time.
    if spec.engine == "vz" && spec.rootfs_ref.is_some() {
        return supervise_vz(dir, &spec, &store);
    }

    supervise_native(dir, &spec, &store)
}
