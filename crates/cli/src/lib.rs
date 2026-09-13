//! The `proxyctl` library.
//!
//! The binary in `main.rs` is a thin shell over this crate so the dispatch under
//! test is the code the process actually runs. The modules are split by role:
//!
//! * [`client`] and [`command`] are the **client** half. They build one HTTP
//!   request over the unix socket and render the response. They depend on neither
//!   the application nor the domain, and an architecture test enforces that.
//! * [`args`] and [`dispatch`] are the grammar and the mapping onto
//!   [`command`].
//! * [`agent`] is the **daemon** half. It is the only module that reaches below
//!   the transport, and nothing in the client half may depend on it.
//! * [`runtime`] translates the daemon's flags into a composition input.
//! * [`exit`] owns the exit codes, which are a contract with scripts.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod agent;
pub mod args;
pub mod client;
pub mod command;
pub mod dispatch;
pub mod endpoint;
pub mod exit;
pub mod runtime;
pub mod token;

pub use command::{Command, Format, Request};
pub use exit::Exit;
