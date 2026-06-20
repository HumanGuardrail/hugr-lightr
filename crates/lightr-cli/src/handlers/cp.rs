//! `lightr cp` handler — copy files between a container and the host
//! (docker cp). CLI-surface freeze: honest fail-closed stub. Behavior lands
//! in WP-LIFE-03.

use crate::handlers::stub::stub;

pub fn run(_src: &str, _dest: &str) -> i32 {
    stub("cp", "WP-LIFE-03")
}
