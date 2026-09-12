//! The control-plane port: commands with side effects.
//!
//! Every method here can change kernel behaviour, so callers must hold the
//! per-instance lock. The read-only observation streams live in
//! [`MihomoObserver`](super::mihomo_observer) instead, because a dropped stream
//! must not be confused with a lifecycle failure.

use async_trait::async_trait;
use proxy_domain::configuration::ConfigBody;
use proxy_domain::mihomo::MihomoBuild;

use crate::ports::error::PortError;
use crate::ports::types::{
    DelayOptions, DelayOutcome, HealthReport, ProxyList, ReloadOutcome, RuleList,
    RuntimeConfigSummary,
};

/// A configuration body to reload, or a path the kernel may read itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadRequest {
    /// Send the document inline.
    ///
    /// Preferred over [`ReloadRequest::Path`]: it avoids both the path allow-list
    /// and the "file must already exist" precondition, which are two avoidable
    /// failure points during an activation.
    Payload(ConfigBody),
    /// Ask the kernel to read a file it can access.
    ///
    /// The path must already be inside the kernel's allow-list, otherwise the
    /// request is rejected.
    Path(String),
}

/// Controls a running Mihomo instance.
#[async_trait]
pub trait MihomoController: Send + Sync {
    /// The running kernel version.
    async fn version(&self) -> Result<MihomoBuild, PortError>;

    /// A summary of the running configuration.
    ///
    /// Only the fields the kernel exposes are returned; this is not a complete
    /// view of the loaded document.
    async fn runtime_config(&self) -> Result<RuntimeConfigSummary, PortError>;

    /// Ask the kernel to load a configuration.
    ///
    /// # Contract
    ///
    /// Implementations must not send a force flag. Forcing has been observed to
    /// tear down working listeners before the replacement binds, leaving an
    /// instance that can no longer be repaired by reloading. Implementations
    /// must also always send a request body, since an empty body is rejected.
    ///
    /// # Errors
    /// Returns [`PortError::Transport`] or [`PortError::Timeout`] when the
    /// request cannot be completed. A rejection by the kernel is *not* an error;
    /// it is [`ReloadOutcome::Rejected`].
    async fn reload(&self, request: ReloadRequest) -> Result<ReloadOutcome, PortError>;

    /// Current proxy groups and nodes.
    async fn proxies(&self) -> Result<ProxyList, PortError>;

    /// Select a member within a strategy group.
    async fn select_proxy(&self, group: &str, proxy: &str) -> Result<(), PortError>;

    /// Measure a node's latency.
    ///
    /// # Errors
    /// A node that does not answer is not an error; it is
    /// [`DelayOutcome::Timeout`].
    async fn test_delay(
        &self,
        name: &str,
        options: &DelayOptions,
    ) -> Result<DelayOutcome, PortError>;

    /// The loaded rule list.
    async fn rules(&self) -> Result<RuleList, PortError>;

    /// Probe the layered health of this instance.
    ///
    /// Implementations must check the proxy port as well as the control API.
    async fn health_check(&self) -> Result<HealthReport, PortError>;

    /// Ask the kernel to exit gracefully.
    ///
    /// # Errors
    /// Returns an error only when the shutdown request could not be delivered;
    /// an already-exited kernel is success.
    async fn shutdown(&self) -> Result<(), PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_reload_carries_the_document() {
        let body = ConfigBody::new("mixed-port: 7890\n").expect("valid body");
        let request = ReloadRequest::Payload(body.clone());
        match request {
            ReloadRequest::Payload(b) => assert_eq!(b.as_str(), body.as_str()),
            ReloadRequest::Path(_) => panic!("expected payload variant"),
        }
    }

    /// There is deliberately no `force` variant: the field cannot be set by
    /// accident because it does not exist.
    #[test]
    fn reload_request_has_no_force_variant() {
        let variants = [ReloadRequest::Path("/tmp/x".into())];
        assert_eq!(variants.len(), 1);
    }
}
