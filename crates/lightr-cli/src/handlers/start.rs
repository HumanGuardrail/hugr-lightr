//! `lightr start` handler — start one or more stopped containers (docker start).
//! CLI-surface freeze: honest fail-closed stub. Behavior lands in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_targets: &[String]) -> i32 {
    stub("start", "WP-LIFE-03")
}
