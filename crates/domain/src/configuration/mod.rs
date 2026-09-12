//! Configuration lifecycle: immutable versions, validation gating, and
//! generation of a complete Mihomo config.
//!
//! Three responsibilities live here, all pure:
//!
//! * [`version`] — versions are immutable records; rollback activates an old
//!   version rather than rewriting history.
//! * [`validation`] — aggregates layer outcomes and gates activation behind a
//!   typestate, so an unvalidated candidate cannot reach `activate()`.
//! * [`generation`] — turns a converted node list plus capability state into a
//!   complete, runnable config. Phase 0 established that the subscription
//!   converter returns only a `proxies:` fragment, so assembling the full
//!   document is the agent's job, and it is a business rule rather than IO.

pub mod body;
pub mod generation;
pub mod preflight;
pub mod validation;
pub mod version;

pub use body::ConfigBody;
pub use generation::{GeneratedConfig, GenerationSpec, generate};
pub use preflight::{PreflightContext, PreflightOutcome};
pub use validation::{
    ConfigCandidate, LevelOutcome, Unvalidated, Validated, ValidationLevel, ValidationReport,
};
pub use version::{ConfigChecksum, ConfigSource, ConfigVersion};

/// Re-exported because several other modules identify a configuration version.
pub use crate::shared::id::ConfigVersionId;
