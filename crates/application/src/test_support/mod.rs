//! Test scaffolding.
//!
//! Two layers, behind two features:
//!
//! * [`doubles`] — in-memory implementations of every port. Enough to assemble a
//!   working [`AppContext`](crate::AppContext) with no external system, which is
//!   what the bootstrap layer needs to prove the wiring compiles and runs.
//!   Available with `test-doubles`.
//! * [`harness`] — the same doubles plus fault injection and call recording, for
//!   asserting failure behaviour. Available with `test-support`.
//!
//! The split exists so a shipping build can never pick up the fault-injection
//! API by asking for the assembler.

pub mod doubles;

#[cfg(feature = "test-support")]
pub mod harness;

#[cfg(feature = "test-support")]
pub use harness::Harness;

pub use doubles::*;
