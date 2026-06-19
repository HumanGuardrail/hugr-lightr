//! Compose submodule -- re-exports the public compose API.
pub mod down;
pub mod model;
pub mod parse;
pub mod supervise;
pub mod up;

pub use down::compose_down;
pub use model::{Compose, ComposeHandle, Service, ServiceSpec, StackSpec};
pub use parse::parse_compose;
pub use supervise::compose_supervise;
pub use up::compose_up;
