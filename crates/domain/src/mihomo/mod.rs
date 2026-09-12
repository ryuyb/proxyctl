//! Mihomo instance modelling: version identity and lifecycle.
//!
//! The data plane itself is not modelled here — only what the control plane must
//! know about *which* kernel is running and *what state* it is in.

pub mod instance;
pub mod status;
pub mod version;

pub use instance::{FailureRecord, MihomoInstance, StartDecision, Transition};
pub use status::MihomoStatus;
pub use version::{KernelFlavor, MihomoBuild, MihomoVersion};
