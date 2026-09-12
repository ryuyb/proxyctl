//! Composition root for the Mihomo management agent.
//!
//! The only crate that knows both the application's ports and concrete
//! adapters. It wires one to the other and holds no business logic.
//!
//! # Why this layer exists at all
//!
//! The application layer depends on traits, never on implementations, which is
//! what makes an adapter replaceable. Something has to choose the
//! implementations, and that choice is exactly the knowledge the application
//! must not have. This crate is that something, and it is the only place where
//! the dependency direction legitimately points outward.
//!
//! # Two halves
//!
//! * [`AdapterFactory`] supplies implementations. The in-memory factory lets the
//!   wiring be verified now; the real one replaces it without touching
//!   [`Bootstrap`].
//! * [`Bootstrap`] performs the decisions — which transport, which supervision
//!   model — and assembles the context.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod adapter_factory;
pub mod composition;
pub mod config;
pub mod real_factory;

pub use adapter_factory::AdapterFactory;
pub use composition::{Bootstrap, BootstrapError, SupervisionModel, supervision_model};
pub use config::{
    ControllerEndpoint, ConverterConfig, DEFAULT_KERNEL_BINARY, DataPaths, RuntimeConfig,
};
pub use real_factory::RealFactory;
