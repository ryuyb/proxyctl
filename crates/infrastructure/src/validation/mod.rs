//! Configuration validation.
//!
//! The kernel's own checker is not a dry run, so validation is layered: a
//! throwaway-directory kernel check combined with a field whitelist generated
//! from upstream's own struct tags.

pub mod validator;
pub mod whitelist;

pub use validator::KernelConfigValidator;
