//! Subscription sources and their SSRF guard.
//!
//! Subscription URLs are operator-supplied, which makes them an input to an
//! outbound request. A subscription pointing at `127.0.0.1`, a link-local
//! address, or a cloud metadata endpoint turns the agent into a probe for the
//! host's internal network.
//!
//! The check is deliberately conservative and string/IP based, with no DNS
//! resolution: resolution would make this function impure and create a
//! time-of-check/time-of-use gap. Hostnames that *resolve* to private space are
//! a documented residual risk handled by the fetch layer, not silently ignored
//! here.

use crate::shared::error::DomainError;

/// A validated subscription URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionUrl {
    raw: String,
    host: String,
}

impl SubscriptionUrl {
    /// Parses a subscription URL.
    ///
    /// Only `http` and `https` are accepted. Other schemes could reach the
    /// filesystem or local sockets.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidInput`] for a blank value, an unsupported
    /// scheme, or a URL without a host.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DomainError::invalid_input(
                "subscription url must not be empty",
            ));
        }

        let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
            DomainError::invalid_input(format!("subscription url must include a scheme: {trimmed}"))
        })?;

        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(DomainError::invalid_input(format!(
                "unsupported url scheme '{scheme}'; only http and https are allowed"
            )));
        }

        // Strip userinfo, then take the authority up to the first delimiter.
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);

        let host = extract_host(host_port);
        if host.is_empty() {
            return Err(DomainError::invalid_input(format!(
                "subscription url has no host: {trimmed}"
            )));
        }

        Ok(Self {
            raw: trimmed.to_owned(),
            host,
        })
    }

    /// The full URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The host component, used for destination checks.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether the destination is a public address.
    ///
    /// Returns `false` for loopback, link-local (including the cloud metadata
    /// address), private ranges, unspecified addresses, and local hostnames.
    #[must_use]
    pub fn is_public_destination(&self) -> bool {
        !is_forbidden_host(&self.host)
    }
}

/// Extracts the host from `host` or `host:port`, tolerating bracketed IPv6.
fn extract_host(host_port: &str) -> String {
    if let Some(rest) = host_port.strip_prefix('[') {
        return rest
            .split_once(']')
            .map_or_else(|| rest.to_owned(), |(host, _)| host.to_owned());
    }
    match host_port.rsplit_once(':') {
        // A single colon means host:port; more than one means a bare IPv6 address.
        Some((host, port)) if !host.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => {
            host.to_owned()
        }
        _ => host_port.to_owned(),
    }
}

/// Whether a host must not be contacted.
fn is_forbidden_host(host: &str) -> bool {
    let host = host.trim_matches(|c| c == '[' || c == ']');
    let lowered = host.to_ascii_lowercase();

    if matches!(
        lowered.as_str(),
        "localhost" | "localhost.localdomain" | "ip6-localhost"
    ) {
        return true;
    }
    if lowered.ends_with(".localhost")
        || lowered.ends_with(".local")
        || lowered.ends_with(".internal")
    {
        return true;
    }

    match lowered.parse::<std::net::IpAddr>() {
        Ok(ip) => !is_public_ip(ip),
        // Unresolved hostnames are allowed; the fetch layer checks the resolved
        // address. Rejecting them here would break ordinary subscriptions.
        Err(_) => false,
    }
}

/// Whether an IP is in public address space.
fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // Carrier-grade NAT (100.64.0.0/10).
                || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
                // Benchmarking (198.18.0.0/15).
                || (v4.octets()[0] == 198 && (v4.octets()[1] == 18 || v4.octets()[1] == 19)))
        }
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false;
            }
            let segments = v6.segments();
            // Link-local fe80::/10 and unique-local fc00::/7.
            let link_local = segments[0] & 0xffc0 == 0xfe80;
            let unique_local = segments[0] & 0xfe00 == 0xfc00;
            if link_local || unique_local {
                return false;
            }
            // IPv4-mapped addresses must be judged on their embedded v4 value,
            // otherwise ::ffff:127.0.0.1 would slip through as "public".
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(std::net::IpAddr::V4(v4));
            }
            true
        }
    }
}

/// Where a subscription's nodes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionSource {
    /// Fetched from a remote URL.
    Url {
        /// The validated URL.
        url: SubscriptionUrl,
        /// Optional User-Agent override.
        user_agent: Option<String>,
    },
}

impl SubscriptionSource {
    /// Builds a URL source.
    ///
    /// # Errors
    /// Propagates [`SubscriptionUrl::parse`] failures.
    pub fn from_url(
        raw: impl Into<String>,
        user_agent: Option<String>,
    ) -> Result<Self, DomainError> {
        Ok(Self::Url {
            url: SubscriptionUrl::parse(raw)?,
            user_agent,
        })
    }

    /// The source URL, if any.
    #[must_use]
    pub fn url(&self) -> Option<&SubscriptionUrl> {
        match self {
            Self::Url { url, .. } => Some(url),
        }
    }

    /// The User-Agent override, if any.
    #[must_use]
    pub fn user_agent(&self) -> Option<&str> {
        match self {
            Self::Url { user_agent, .. } => user_agent.as_deref(),
        }
    }

    /// A short stable label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Url { .. } => "url",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_url_and_extracts_host() {
        let url = SubscriptionUrl::parse("https://example.com/sub?token=abc").expect("valid");
        assert_eq!(url.host(), "example.com");
        assert!(url.is_public_destination());
    }

    #[test]
    fn rejects_blank_and_schemeless() {
        assert!(SubscriptionUrl::parse("").is_err());
        assert!(SubscriptionUrl::parse("   ").is_err());
        assert!(SubscriptionUrl::parse("example.com/sub").is_err());
    }

    #[test]
    fn rejects_non_http_schemes() {
        for raw in [
            "file:///etc/passwd",
            "ftp://example.com/sub",
            "gopher://example.com",
        ] {
            assert!(
                SubscriptionUrl::parse(raw).is_err(),
                "{raw} must be rejected"
            );
        }
    }

    #[test]
    fn strips_userinfo_before_host_check() {
        let url = SubscriptionUrl::parse("https://user:pass@example.com/sub").expect("valid");
        assert_eq!(url.host(), "example.com");
    }

    /// The SSRF cases this guard exists for.
    #[test]
    fn rejects_loopback_and_local_names() {
        for raw in [
            "http://127.0.0.1:8080/sub",
            "http://localhost/sub",
            "http://[::1]:8080/sub",
            "http://sub.localhost/x",
            "http://thing.local/x",
            "http://metadata.internal/x",
        ] {
            let url = SubscriptionUrl::parse(raw).expect("parses");
            assert!(
                !url.is_public_destination(),
                "{raw} must not be treated as a public destination"
            );
        }
    }

    #[test]
    fn rejects_cloud_metadata_address() {
        let url =
            SubscriptionUrl::parse("http://169.254.169.254/latest/meta-data/").expect("valid");
        assert!(!url.is_public_destination());
    }

    #[test]
    fn rejects_private_ranges() {
        for raw in [
            "http://10.0.0.5/sub",
            "http://192.168.1.5/sub",
            "http://172.16.0.1/sub",
            "http://100.64.0.1/sub",
            "http://198.18.0.1/sub",
            "http://[fc00::1]/sub",
            "http://[fe80::1]/sub",
            "http://0.0.0.0/sub",
        ] {
            let url = SubscriptionUrl::parse(raw).expect("parses");
            assert!(!url.is_public_destination(), "{raw} must be rejected");
        }
    }

    /// IPv4-mapped IPv6 must be judged on the embedded address.
    #[test]
    fn rejects_ipv4_mapped_loopback() {
        let url = SubscriptionUrl::parse("http://[::ffff:127.0.0.1]/sub").expect("valid");
        assert!(!url.is_public_destination());
    }

    #[test]
    fn accepts_public_ipv4_and_ipv6() {
        for raw in ["http://1.1.1.1/sub", "https://[2606:4700::1111]/sub"] {
            let url = SubscriptionUrl::parse(raw).expect("valid");
            assert!(url.is_public_destination(), "{raw} should be public");
        }
    }

    /// Unresolved hostnames are permitted here; the fetch layer verifies the
    /// resolved address.
    #[test]
    fn allows_unresolved_hostnames() {
        let url = SubscriptionUrl::parse("https://sub.example.org/x").expect("valid");
        assert!(url.is_public_destination());
    }

    #[test]
    fn source_builder_validates_url() {
        let source = SubscriptionSource::from_url("https://example.com/sub", Some("clash".into()))
            .expect("valid");
        assert_eq!(source.as_str(), "url");
        assert_eq!(source.url().map(SubscriptionUrl::host), Some("example.com"));
        assert_eq!(source.user_agent(), Some("clash"));

        assert!(SubscriptionSource::from_url("not-a-url", None).is_err());
    }

    #[test]
    fn source_without_user_agent_reports_none() {
        let source = SubscriptionSource::from_url("https://example.com/sub", None).expect("valid");
        assert_eq!(source.user_agent(), None);
    }
}
