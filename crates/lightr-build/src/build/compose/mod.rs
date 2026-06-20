//! Compose submodule -- re-exports the public compose API.
pub mod down;
pub mod interp;
pub(crate) mod lower;
pub mod model;
pub mod parse;
pub mod spec;
pub mod supervise;
pub mod up;

pub use down::compose_down;
pub use interp::{interpolate_compose, scope_from_project_dir};
pub use model::{Compose, ComposeHandle, Service, ServiceSpec, StackSpec};
pub use parse::{parse_compose, parse_compose_with_scope};
pub use spec::{ComposeSpec, EnvScalar, Environment, Healthcheck, ServiceDef, StringOrList};
pub use supervise::compose_supervise;
pub use up::compose_up;
