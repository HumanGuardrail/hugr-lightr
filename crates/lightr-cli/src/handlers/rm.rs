//! `lightr rm` handler — remove one or more stopped containers (docker rm).
//! CLI-surface freeze: honest fail-closed stub. Behavior lands in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_targets: &[String], _force: bool) -> i32 {
    stub("rm", "WP-LIFE-03")
}
