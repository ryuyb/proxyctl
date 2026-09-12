//! Subscription conversion.
//!
//! The converter is replaceable, so this port describes business intent only.
//! Nothing here names a specific provider's endpoint, parameters, or payload
//! shape: the application must not learn that a backend exists, only that
//! conversion is possible.
//!
//! The result is a node fragment, not a usable configuration — the backend does
//! not emit ports, controller settings, DNS, groups, or rules. Assembling a
//! complete document is the agent's job.

use async_trait::async_trait;
use proxy_domain::subscription::{ConvertedProxies, SubscriptionSource, TargetFormat};

use crate::ports::error::PortError;
use crate::ports::types::{CachePolicy, ConverterCapabilities, ConverterHealth};

/// What to convert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertRequest {
    /// Where the nodes come from.
    pub source: SubscriptionSource,
    /// The output format to produce.
    pub target: TargetFormat,
    /// Optional upstream proxy for the converter to fetch through.
    pub proxy: Option<String>,
    /// Whether several sources may be merged.
    pub merge_sources: bool,
    /// Cache behaviour for this fetch.
    pub cache: CachePolicy,
}

/// Converts subscription sources into a node fragment.
#[async_trait]
pub trait SubscriptionConverter: Send + Sync {
    /// Convert a source into nodes.
    ///
    /// # Errors
    /// Returns [`PortError::Converter`] for every business-level failure,
    /// including an empty result. An empty result is a failure, never a success:
    /// treating it as success would activate a configuration with no routes.
    async fn convert(&self, request: &ConvertRequest) -> Result<ConvertedProxies, PortError>;

    /// What this converter can do.
    async fn capabilities(&self) -> Result<ConverterCapabilities, PortError>;

    /// Whether the converter is reachable and usable.
    ///
    /// Used at startup to decide between a configured converter and the built-in
    /// fallback, and to report a degraded reason rather than failing an update
    /// opaquely.
    async fn health(&self) -> Result<ConverterHealth, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::shared::id::ConverterId;

    #[test]
    fn request_carries_intent_not_provider_details() {
        let source = SubscriptionSource::from_url("https://example.com/sub", None).expect("valid");
        let request = ConvertRequest {
            source,
            target: TargetFormat::Mihomo,
            proxy: None,
            merge_sources: false,
            cache: CachePolicy::PreferCache,
        };
        assert_eq!(request.target, TargetFormat::Mihomo);
        assert!(!request.merge_sources);
    }

    #[test]
    fn cache_policy_is_explicit() {
        assert_ne!(CachePolicy::PreferCache, CachePolicy::Bypass);
    }

    #[test]
    fn capabilities_allow_list_is_queryable() {
        let caps = ConverterCapabilities {
            id: ConverterId::parse("sub-store").expect("valid"),
            supports_targets: vec![TargetFormat::Mihomo],
            supports_merge_sources: true,
            version: None,
        };
        assert!(caps.supports(TargetFormat::Mihomo));
        assert_eq!(caps.supports_targets.len(), 1);
    }
}
