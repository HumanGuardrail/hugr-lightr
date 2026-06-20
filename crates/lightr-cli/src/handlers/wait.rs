//! `lightr wait` handler — block until containers stop, print exit codes
//! (docker wait). CLI-surface freeze: honest fail-closed stub. Behavior lands
//! in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_targets: &[String]) -> i32 {
    stub("wait", "WP-LIFE-03")
}
