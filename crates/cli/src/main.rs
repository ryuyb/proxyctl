//! `proxyctl` — the single binary.
//!
//! # One binary, two roles
//!
//! ```text
//! proxyctl <command>      a client: builds one request, prints the response
//! proxyctl agent run      the daemon: composes the context and serves the socket
//! ```
//!
//! The client half reaches the agent only over the unix socket. It never calls a
//! use case directly, even though it links the crates that could: the socket's
//! file permissions are the access-control boundary, per-instance locks are
//! in-process, and the kernel's process outlives the request that started it. A
//! direct call would bypass all three.
//!
//! # Exit codes
//!
//! See [`exit::Exit`]. The short version: `0` success, `1` failure, `2` usage,
//! `3` not found, `4` conflict, `5` permission, `6` a dependency was unreachable,
//! `7` not implemented. Scripts branch on these, so they are a contract.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

// The binary is a thin shell over the library, so the dispatch under test is the
// same code the process runs.

use clap::Parser as _;
use proxyctl::args::Cli;
use proxyctl::dispatch;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    dispatch::run(Cli::parse()).await.as_exit_code()
}
