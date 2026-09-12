//! Subscription conversion.
//!
//! The converter is replaceable, so this module is the only place that knows a
//! provider's endpoints, parameters, and payload shapes.

pub mod substore;

pub use substore::SubStoreConverter;
