//! The pure domain model for the Mihomo management agent.
//!
//! This crate contains entities, value objects, invariants, and business rules —
//! and nothing else. It has no IO, no async runtime, no framework types, and no
//! clock of its own: time and identifiers arrive as arguments, which keeps every
//! rule deterministic and testable without a Linux host.
//!
//! # Layering
//!
//! ```text
//! interfaces ─┐
//!             ├─► application ─► domain
//! infrastructure ─┘
//! ```
//!
//! Dependencies point inward only. `domain` depends on no other crate in this
//! workspace, which is enforced by its `Cargo.toml`.
//!
//! # Where the rules live
//!
//! Two rules are load-bearing enough to be encoded in types rather than
//! comments:
//!
//! * **An unvalidated configuration cannot be activated.** [`configuration::ConfigCandidate`]
//!   is parameterised by a marker type, and `activate` is reachable only through
//!   [`configuration::ConfigCandidate::validate`].
//! * **Capability is not a boolean.** [`system::CapabilityStatus`] has five
//!   values because a present-but-unusable device is a real and distinct state.
//!   See [`system::capability::evaluate_tun`].
//!
//! # What is deliberately absent
//!
//! HTTP clients, command execution, filesystem access, database queries,
//! environment parsing, and API DTOs. Generating a *complete* kernel
//! configuration is present ([`configuration::generate`]) because the field set
//! and the capability gating are business rules; *writing* it is not.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// Panicking helpers are banned in shipping code: a malformed configuration or a
// failed probe must return an error, never abort the agent. Tests are exempt
// because `expect` on a fixture is the clearest way to express intent.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod audit;
pub mod configuration;
pub mod mihomo;
pub mod shared;
pub mod subscription;
pub mod system;

/// The domain's error type, re-exported for convenience.
pub use shared::error::DomainError;

/// Re-export of the time value object, since nearly every operation takes one.
pub use shared::time::Timestamp;
