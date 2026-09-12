//! Strongly-typed identifiers.
//!
//! Every aggregate gets its own newtype so that mixing them up is a compile
//! error rather than a silent bug. `activate(config_id)` and
//! `activate(subscription_id)` must not type-check against each other.

use crate::shared::error::DomainError;

/// Declares an opaque string-backed identifier newtype.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $label:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Parses an identifier, rejecting empty/whitespace-only input.
            ///
            /// # Errors
            /// Returns [`DomainError::Invariant`] when the input is blank.
            pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
                let raw = raw.into();
                if raw.trim().is_empty() {
                    return Err(DomainError::invariant(concat!($label, " must not be empty")));
                }
                Ok(Self(raw))
            }

            /// Returns the raw identifier.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

define_id!(
    /// Identifies a managed Mihomo instance.
    ///
    /// Present from day one even though the MVP runs a single instance, so that
    /// multi-instance support does not require a breaking migration.
    MihomoInstanceId,
    "mihomo instance id"
);
define_id!(
    /// Identifies one immutable configuration version.
    ConfigVersionId,
    "config version id"
);
define_id!(
    /// Identifies a subscription definition.
    SubscriptionId,
    "subscription id"
);
define_id!(
    /// Identifies a background job run.
    JobId,
    "job id"
);
define_id!(
    /// Identifies a subscription converter implementation.
    ConverterId,
    "converter id"
);
define_id!(
    /// Identifies one audit log entry.
    AuditEntryId,
    "audit entry id"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_ids() {
        assert!(MihomoInstanceId::parse("").is_err());
        assert!(MihomoInstanceId::parse("   ").is_err());
        assert!(ConfigVersionId::parse("\t\n").is_err());
    }

    #[test]
    fn accepts_and_roundtrips() {
        let id = MihomoInstanceId::parse("default").expect("valid id");
        assert_eq!(id.as_str(), "default");
        assert_eq!(id.to_string(), "default");
    }

    #[test]
    fn ids_are_comparable_and_hashable() {
        use std::collections::HashSet;

        let a = ConfigVersionId::parse("v001").expect("valid");
        let b = ConfigVersionId::parse("v001").expect("valid");
        let c = ConfigVersionId::parse("v002").expect("valid");

        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        assert!(set.insert(a));
        assert!(!set.insert(b));
    }
}
