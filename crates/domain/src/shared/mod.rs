//! Shared kernel: identifiers, time, and the domain error taxonomy.
//!
//! Everything in this module is dependency-free (only `thiserror`) and free of
//! IO, clocks, and randomness. Identifiers and timestamps are supplied by the
//! caller so the domain stays deterministic and testable.

pub mod clock;
pub mod error;
pub mod id;
pub mod time;
