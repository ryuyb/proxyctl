//! Commands: use cases that change state.
//!
//! Every command that touches kernel lifecycle or configuration takes the
//! per-instance lock before doing work, so two commands on the same instance
//! cannot interleave their decisions.

pub mod activate_config;
pub mod rollback_config;

pub use activate_config::{ActivateConfig, ActivateConfigInput, ActivateConfigOutput};
pub use rollback_config::{RollbackConfig, RollbackConfigInput, RollbackConfigOutput};
