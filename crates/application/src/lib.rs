//! Use cases and ports for the Mihomo management agent.
//!
//! This layer owns **sequencing and decisions**. It answers questions like "must
//! a reload be followed by a health check?", "what happens when that health
//! check fails?", and "may this start request spawn a process?" — and it does so
//! without knowing how any of those steps are performed. Mechanisms live behind
//! the traits in [`ports`].
//!
//! # What this layer does not contain
//!
//! No HTTP client, no database driver, no process spawning, no filesystem
//! access. Every side effect crosses a port, which is what lets the whole
//! failure-handling story be tested with in-memory doubles and no external
//! system.
//!
//! # The invariants this layer protects
//!
//! * **A failed update never destroys the working configuration.** Subscription
//!   updates route through the same activation path as any other change, and
//!   every failure leaves the previous version active.
//! * **Activation failures recover.** If a reload or its health check fails, the
//!   previous version is restored by restarting the kernel — reloading cannot
//!   recover from a partially applied configuration.
//! * **Recovery is based on observation.** Rollback re-reads what is active
//!   rather than trusting what the caller believes was active, so a storage
//!   fault yields an accurate report instead of a false success.
//! * **Lifecycle operations are serialized per instance.** Concurrent starts
//!   must not each decide to spawn.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod commands;
pub mod context;
pub mod context_builder;
pub mod error;
pub mod locks;
pub mod ports;
pub mod queries;

/// In-memory test doubles with fault injection.
///
/// Behind the `test-support` feature so production builds exclude it. The
/// doubles are not `#[cfg(test)]` because integration tests in other crates
/// (and infrastructure tests) need to reuse them.
#[cfg(any(test, feature = "test-doubles"))]
pub mod test_support;

pub use context::{AppContext, ProcessState};
pub use context_builder::{AppContextBuilder, MissingDependency};
pub use error::ApplicationError;

/// How long to wait for a graceful kernel stop before forcing termination.
///
/// Matches the value the runtime design settles on, and stays below the unit's
/// `TimeoutStopSec` so the agent reports its own timeout rather than being killed
/// mid-shutdown.
pub const DEFAULT_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
