//! `lightr top` handler — list the processes running in a container
//! (docker top). CLI-surface freeze: honest fail-closed stub. Behavior lands
//! in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_target: &str) -> i32 {
    stub("top", "WP-LIFE-03")
}
