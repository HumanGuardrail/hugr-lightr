//! `lightr stats` handler — live resource-usage stats (docker stats).
//! CLI-surface freeze: honest fail-closed stub. Behavior lands in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_target: Option<&str>) -> i32 {
    stub("stats", "WP-LIFE-03")
}
