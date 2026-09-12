//! Process supervision adapters.

pub mod procinfo;
pub mod supervisor;

pub use supervisor::{DEFAULT_GRACEFUL_TIMEOUT, SharedSupervisor, SupervisedChildProcess};
