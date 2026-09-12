//! Interface adapters over the application use cases.
//!
//! This layer turns a transport into use-case calls and nothing else. It holds
//! no business logic: a decision that would survive a different transport belongs
//! in the application layer, and a mechanism belongs behind a port.
//!
//! # Dependency direction
//!
//! `interfaces -> application, domain`. Never infrastructure: an adapter that
//! knew a concrete implementation could not be replaced, and the whole point of
//! the port boundary is that it can be.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod dto;
pub mod http;
