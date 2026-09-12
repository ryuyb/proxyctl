//! Adapters for the application ports.
//!
//! This layer is where side effects belong: talking to the kernel, reading and
//! writing files, spawning processes. Each adapter implements a trait declared
//! by the application layer, so no business rule is decided here and the
//! application never depends on a concrete type.
//!
//! # What belongs here
//!
//! Mechanism only. If a decision would still hold with a different adapter —
//! "always health-check after a reload", "never force an activation" — it
//! belongs in the application layer, not in an adapter.
//!
//! # What does not
//!
//! Adapters may not call each other. A dependency between two adapters is a
//! wiring problem, and the bootstrap layer's `AdapterFactory` makes it
//! unrepresentable by never passing a context into an adapter constructor.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod mihomo;
pub mod process;
