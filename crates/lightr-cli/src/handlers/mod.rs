//! Verb handler modules.
pub mod attach;
pub mod bench;
pub mod bench_compare;
pub mod bench_compete_docker;
pub mod bisect;
pub mod build;
pub mod commit;
pub mod compose;
pub mod container;
pub mod cp;
pub mod create;
pub mod diff;
pub mod docker;
pub mod engine;
pub mod exec;
pub mod gc;
pub mod history;
pub mod hydrate;
pub mod images;
pub mod info;
pub mod inspect;
pub mod kill;
pub mod logs;
pub mod mcp;
// The net mesh (lightr_run::network) is unix-only; on Windows (WSL ring, future)
// the network verbs are honest-stubbed at dispatch. Gate the module to match.
#[cfg(unix)]
pub mod network;
pub mod oci;
pub mod pause;
pub mod plan;
pub mod port;
pub mod ps;
pub mod rename;
pub mod restart;
pub mod rm;
pub mod rmi;
pub mod run;
pub mod runproc;
pub mod schema;
pub mod snapshot;
pub mod start;
pub mod stats;
pub mod status;
pub mod stop;
// WP-RUNFLAGS: the honest "not yet implemented" stub helper is now reached ONLY
// from the `#[cfg(not(unix))]` `Cmd::Network` dispatch arm (the last run-flag
// stub was removed when `-v`/`--tmpfs`/`--name`/`--rm`/`--entrypoint` were wired
// and the networking flags moved to a specific honest error). On unix it is dead
// code → `clippy -D` fails; gate it (and its tests) to the platform that uses it
// (template 8a: cfg the item, not just the caller). A future WP that needs an
// honest unix stub re-widens this gate.
#[cfg(not(unix))]
pub mod stub;
pub mod supervise;
pub mod system;
pub mod tag;
#[cfg(test)]
pub mod testref;
pub mod top;
pub mod undo;
pub mod unpause;
pub mod version;
pub mod volume;
pub mod wait;
