//! Subscriptions and their conversion inputs.
//!
//! The converter is an external, replaceable component, so the domain describes
//! only *intent*: a source, a target format, and whether the result may be
//! merged. Nothing here names a specific provider's parameters.

pub mod conversion;
pub mod schedule;
pub mod source;
#[allow(clippy::module_inception)]
pub mod subscription;

pub use conversion::{ConvertedProxies, TargetFormat};
pub use schedule::{Interval, Schedule};
pub use source::{SubscriptionSource, SubscriptionUrl};
pub use subscription::{
    Subscription, SubscriptionState, UpdateFailure, UpdateOutcome, UpdateRecord,
};
