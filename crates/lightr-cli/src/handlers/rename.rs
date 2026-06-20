//! `lightr rename` handler — rename a container (docker rename).
//! CLI-surface freeze: honest fail-closed stub. Behavior lands in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_target: &str, _new_name: &str) -> i32 {
    stub("rename", "WP-LIFE-03")
}
