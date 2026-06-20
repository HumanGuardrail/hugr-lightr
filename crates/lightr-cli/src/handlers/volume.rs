//! `lightr volume` handlers — named-volume management (docker volume).
//! CLI-surface freeze: honest fail-closed stubs. Behavior lands in WP-VOL-*.

use crate::cli::cmd::VolumeCmd;
use crate::handlers::stub::stub;

pub fn run(subcmd: VolumeCmd) -> i32 {
    match subcmd {
        VolumeCmd::Create { .. } => stub("volume create", "WP-VOL-4"),
        VolumeCmd::Ls { .. } => stub("volume ls", "WP-VOL-4"),
        VolumeCmd::Rm { .. } => stub("volume rm", "WP-VOL-4"),
        VolumeCmd::Inspect { .. } => stub("volume inspect", "WP-VOL-4"),
        VolumeCmd::Prune { .. } => stub("volume prune", "WP-VOL-4"),
    }
}
