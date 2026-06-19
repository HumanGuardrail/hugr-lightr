//! Compose submodule -- re-exports the public compose API.
pub mod model;
pub mod parse;
pub mod up;
pub mod supervise;
pub mod down;

pub use model::{Compose, ComposeHandle, Service, ServiceSpec, StackSpec};
pub use parse::parse_compose;
pub use up::compose_up;
pub use supervise::compose_supervise;
pub use down::compose_down;
