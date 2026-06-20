//! `lightr network` handlers — container-network management (docker network).
//! CLI-surface freeze: honest fail-closed stubs. Behavior lands in WP-NET-*.

use crate::cli::cmd::NetworkCmd;
use crate::handlers::stub::stub;

pub fn run(subcmd: NetworkCmd) -> i32 {
    match subcmd {
        NetworkCmd::Create { .. } => stub("network create", "WP-NET-1"),
        NetworkCmd::Ls { .. } => stub("network ls", "WP-NET-1"),
        NetworkCmd::Rm { .. } => stub("network rm", "WP-NET-1"),
        NetworkCmd::Inspect { .. } => stub("network inspect", "WP-NET-1"),
        NetworkCmd::Connect { .. } => stub("network connect", "WP-NET-1"),
        NetworkCmd::Disconnect { .. } => stub("network disconnect", "WP-NET-1"),
    }
}
