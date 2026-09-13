//! Commands: use cases that change state.
//!
//! Every command that touches kernel lifecycle or configuration takes the
//! per-instance lock before doing work, so two commands on the same instance
//! cannot interleave their decisions.
//!
//! # Lock discipline
//!
//! The instance lock is **not reentrant**. A command that holds it must call
//! internal helpers rather than other public commands, or it will deadlock
//! against itself. `RestartMihomo` is the case to watch: it holds the lock and
//! therefore drives the process directly instead of calling
//! [`StartMihomo::execute`].

pub mod activate_config;
pub mod connections;
pub mod lifecycle;
pub mod rollback_config;
pub mod store_config;
pub mod update_subscription;

pub use activate_config::{ActivateConfig, ActivateConfigInput, ActivateConfigOutput};
pub use connections::{CloseAllConnections, CloseConnection, CloseReport, ListConnections};
pub use lifecycle::{
    ReloadMihomo, RestartMihomo, StartMihomo, StartOutcome, StopMihomo, StopOutcome, signal_kernel,
};
pub use rollback_config::{RollbackConfig, RollbackConfigInput, RollbackConfigOutput};
pub use store_config::{StoreConfig, StoreConfigOutput};
pub use update_subscription::{
    SubscriptionCrud, SubscriptionTestResult, UpdateSubscription, UpdateSubscriptionInput,
    UpdateSubscriptionOutput,
};
