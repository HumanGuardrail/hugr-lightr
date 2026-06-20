//! `lightr kill` handler — send a signal to a running container (docker kill).
//! CLI-surface freeze: honest fail-closed stub. Behavior lands in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_targets: &[String], _signal: Option<&str>) -> i32 {
    stub("kill", "WP-LIFE-03")
}
