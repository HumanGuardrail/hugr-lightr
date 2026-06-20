//! `lightr restart` handler — restart one or more containers (docker restart).
//! CLI-surface freeze: honest fail-closed stub. Behavior lands in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_targets: &[String], _grace: u64) -> i32 {
    stub("restart", "WP-LIFE-03")
}
