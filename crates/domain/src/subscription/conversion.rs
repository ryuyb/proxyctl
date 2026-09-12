//! Conversion inputs and outputs.

use crate::shared::error::DomainError;

/// The format a conversion should produce.
///
/// The domain states business intent; mapping this to a provider's own target
/// vocabulary is an adapter concern, and the mapping must be an explicit
/// allow-list because provider vocabularies are not validated upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetFormat {
    /// A Mihomo/Clash.Meta configuration fragment.
    Mihomo,
}

impl TargetFormat {
    /// A short stable label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mihomo => "mihomo",
        }
    }
}

/// The output of a conversion: a node list, not a usable configuration.
///
/// The name is deliberate. Phase 0 measured that the subscription backend
/// returns only a `proxies:` fragment — no ports, no controller, no rules — so
/// calling this a "subscription" or a "configuration" would invite the mistake
/// of activating it directly. It is an intermediate product; the agent assembles
/// the full document from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertedProxies {
    /// The YAML fragment, expected to contain a `proxies:` list.
    pub fragment: String,
    /// Number of nodes the converter reported producing.
    pub node_count: usize,
}

impl ConvertedProxies {
    /// Builds a conversion result.
    ///
    /// # Errors
    /// Returns [`DomainError::Invariant`] when the fragment is blank or reports
    /// zero nodes. An empty conversion is a failure, never a silent success:
    /// propagating an empty node list would activate a config that routes
    /// nothing while appearing healthy.
    pub fn new(fragment: impl Into<String>, node_count: usize) -> Result<Self, DomainError> {
        let fragment = fragment.into();
        if fragment.trim().is_empty() {
            return Err(DomainError::invariant(
                "converter returned an empty fragment; treating as failure",
            ));
        }
        if node_count == 0 {
            return Err(DomainError::invariant(
                "converter returned zero proxies; treating as failure",
            ));
        }
        Ok(Self {
            fragment,
            node_count,
        })
    }

    /// Whether any nodes were produced. Always `true` for a constructed value.
    #[must_use]
    pub const fn has_nodes(&self) -> bool {
        self.node_count > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_non_empty_conversion() {
        let converted = ConvertedProxies::new("proxies:\n  - {name: A}", 1).expect("valid");
        assert_eq!(converted.node_count, 1);
        assert!(converted.has_nodes());
    }

    /// The failure mode observed in a rejected converter: success returned with
    /// an empty payload. It must be impossible to represent here.
    #[test]
    fn rejects_empty_fragment_even_with_node_count() {
        assert!(ConvertedProxies::new("", 5).is_err());
        assert!(ConvertedProxies::new("   \n", 5).is_err());
    }

    #[test]
    fn rejects_zero_nodes_even_with_content() {
        let err = ConvertedProxies::new("proxies:\n", 0).expect_err("must reject");
        assert!(err.to_string().contains("zero proxies"));
    }

    #[test]
    fn target_format_label_is_stable() {
        assert_eq!(TargetFormat::Mihomo.as_str(), "mihomo");
    }
}
