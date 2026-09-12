//! Generation of a complete Mihomo configuration.
//!
//! Phase 0 measured that the subscription converter returns only a `proxies:`
//! fragment — no ports, no controller, no DNS, no groups, no rules. Assembling
//! the runnable document is therefore the agent's job.
//!
//! That assembly is a *business rule*, not IO: which fields are emitted, and
//! under which capability conditions TUN may be enabled, are decisions this
//! project owns. Keeping generation here (rather than in an adapter) means the
//! safety-relevant rules are covered by plain unit tests that need no Linux
//! host, no ports, and no kernel.
//!
//! The function is total and deterministic: same spec plus same capabilities
//! always yields byte-identical output, which is what makes checksums stable.

use crate::configuration::body::ConfigBody;
use crate::shared::error::DomainError;
use crate::system::capability::CapabilitySet;

/// The inputs needed to generate a configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationSpec {
    /// Inbound mixed (HTTP+SOCKS) proxy port.
    pub mixed_port: u16,
    /// Controller listen address. Must be loopback; see [`GenerationSpec::validate`].
    pub controller: ControllerEndpoint,
    /// Controller secret. Must be non-empty; see [`GenerationSpec::validate`].
    pub secret: String,
    /// The `proxies:` fragment produced by the converter.
    pub proxies_fragment: String,
    /// Proxy groups to emit, as (name, member names).
    pub groups: Vec<ProxyGroup>,
    /// Rules to emit, in order.
    pub rules: Vec<String>,
    /// Whether the caller wants TUN enabled if the environment permits it.
    pub tun_requested: bool,
}

/// A controller listen endpoint.
///
/// Validated at construction because a misconfigured controller endpoint is the
/// difference between a local-only control plane and a remotely reachable one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerEndpoint {
    /// A loopback TCP address such as `127.0.0.1:9090`.
    Loopback(String),
    /// A Unix domain socket path.
    UnixSocket(String),
}

impl ControllerEndpoint {
    /// Parses and validates a controller endpoint.
    ///
    /// Accepts `127.0.0.1:port`, `[::1]:port`, `localhost:port`, or a unix
    /// socket path beginning with `/`.
    ///
    /// # Errors
    /// Rejects wildcard binds (`:9090`, `0.0.0.0:9090`, `[::]:9090`) and any
    /// other non-loopback host. A wildcard controller exposes the full control
    /// plane — including kernel restart and upgrade endpoints — to the network.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DomainError::invalid_input(
                "controller address must not be empty",
            ));
        }

        if trimmed.starts_with('/') {
            return Ok(Self::UnixSocket(trimmed.to_owned()));
        }

        let (host, port) = split_host_port(trimmed).ok_or_else(|| {
            DomainError::invalid_input(format!("controller address must include a port: {trimmed}"))
        })?;

        if port.is_empty() || port.parse::<u16>().is_err() {
            return Err(DomainError::invalid_input(format!(
                "controller port is not a valid u16: {trimmed}"
            )));
        }

        // A leading colon (`:9090`) means "all interfaces" in Mihomo's syntax.
        if host.is_empty() {
            return Err(DomainError::invalid_input(
                "controller host must not be empty; binding all interfaces exposes the control plane",
            ));
        }

        if !is_loopback_host(host) {
            return Err(DomainError::invalid_input(format!(
                "controller host must be loopback, got {host}"
            )));
        }

        Ok(Self::Loopback(trimmed.to_owned()))
    }

    /// The rendered address.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Loopback(address) | Self::UnixSocket(address) => address,
        }
    }

    /// Whether this is a unix socket endpoint.
    #[must_use]
    pub const fn is_unix_socket(&self) -> bool {
        matches!(self, Self::UnixSocket(_))
    }
}

/// Splits `host:port`, tolerating bracketed IPv6.
fn split_host_port(value: &str) -> Option<(&str, &str)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once(']')?;
        let port = port.strip_prefix(':')?;
        return Some((host, port));
    }
    let (host, port) = value.rsplit_once(':')?;
    Some((host, port))
}

/// Whether `host` denotes loopback.
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// A proxy group to emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyGroup {
    /// Group name.
    pub name: String,
    /// Group type, e.g. `select`.
    pub kind: String,
    /// Member proxy or group names.
    pub members: Vec<String>,
}

impl ProxyGroup {
    /// Builds a `select` group.
    #[must_use]
    pub fn select(name: impl Into<String>, members: Vec<String>) -> Self {
        Self {
            name: name.into(),
            kind: "select".to_owned(),
            members,
        }
    }

    /// Builds a `url-test` group.
    #[must_use]
    pub fn url_test(name: impl Into<String>, members: Vec<String>) -> Self {
        Self {
            name: name.into(),
            kind: "url-test".to_owned(),
            members,
        }
    }
}

/// The generated configuration plus what was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedConfig {
    /// The complete document.
    pub body: ConfigBody,
    /// Whether TUN was enabled.
    pub tun_enabled: bool,
    /// Whether the controller uses a unix socket.
    pub controller_is_unix_socket: bool,
}

impl GenerationSpec {
    /// Validates the spec before generation.
    ///
    /// # Errors
    /// Rejects an empty secret and a proxies fragment with no nodes: emitting a
    /// config with no proxies would start a kernel that cannot route anything,
    /// which is worse than failing loudly.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.secret.trim().is_empty() {
            return Err(DomainError::invariant(
                "controller secret must not be empty; an unauthenticated controller is fully open",
            ));
        }
        if self.secret.chars().any(char::is_whitespace) {
            return Err(DomainError::invariant(
                "controller secret must not contain whitespace",
            ));
        }
        if !has_proxy_nodes(&self.proxies_fragment) {
            return Err(DomainError::invariant(
                "proxies fragment contains no nodes; refusing to generate an unroutable config",
            ));
        }
        Ok(())
    }
}

/// Whether a `proxies:` fragment contains at least one list item.
fn has_proxy_nodes(fragment: &str) -> bool {
    fragment.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- ") || trimmed.starts_with("-\t")
    })
}

/// Generates a complete Mihomo configuration.
///
/// TUN is enabled only when the caller requests it *and* the capability set
/// reports both a working device and `CAP_NET_ADMIN`. Any other status —
/// `Misconfigured`, `Unavailable`, `Unknown` — leaves TUN out, because a kernel
/// started with TUN enabled on a host that cannot provide it fails to route.
///
/// # Errors
/// Returns the [`GenerationSpec::validate`] error when the spec is unusable.
pub fn generate(
    spec: &GenerationSpec,
    capabilities: &CapabilitySet,
) -> Result<GeneratedConfig, DomainError> {
    spec.validate()?;

    let tun_enabled = spec.tun_requested && capabilities.can_enable_tun();
    let controller_is_unix_socket = spec.controller.is_unix_socket();

    let mut out = String::with_capacity(512 + spec.proxies_fragment.len());

    out.push_str("# Generated by proxy-agent. Do not edit in place.\n");
    out.push_str(&format!("mixed-port: {}\n", spec.mixed_port));
    out.push_str("bind-address: 127.0.0.1\n");
    out.push_str("allow-lan: false\n");
    out.push_str("mode: rule\n");
    out.push_str("log-level: info\n");
    out.push_str("ipv6: false\n");

    if let ControllerEndpoint::UnixSocket(path) = &spec.controller {
        out.push_str(&format!("external-controller-unix: {path}\n"));
    } else {
        out.push_str(&format!(
            "external-controller: {}\n",
            spec.controller.as_str()
        ));
    }

    out.push_str(&format!("secret: \"{}\"\n", spec.secret));

    // The upstream default is allow-origins: ["*"] with private-network access
    // enabled, which lets any page in the user's browser drive the control
    // plane. Narrowing it is mandatory, so it is written unconditionally.
    out.push_str("external-controller-cors:\n");
    out.push_str("  allow-origins: []\n");
    out.push_str("  allow-private-network: false\n");

    if tun_enabled {
        out.push_str("tun:\n");
        out.push_str("  enable: true\n");
        out.push_str("  stack: mixed\n");
        out.push_str("  auto-route: true\n");
        out.push_str("  auto-detect-interface: true\n");
    }

    if !spec.proxies_fragment.trim().is_empty() {
        let fragment = spec.proxies_fragment.trim_end();
        out.push_str(fragment);
        out.push('\n');
    }

    if !spec.groups.is_empty() {
        out.push_str("proxy-groups:\n");
        for group in &spec.groups {
            out.push_str(&format!("  - name: {}\n", group.name));
            out.push_str(&format!("    type: {}\n", group.kind));
            out.push_str("    proxies:\n");
            for member in &group.members {
                out.push_str(&format!("      - {member}\n"));
            }
        }
    }

    out.push_str("rules:\n");
    if spec.rules.is_empty() {
        out.push_str("  - MATCH,DIRECT\n");
    } else {
        for rule in &spec.rules {
            out.push_str(&format!("  - {rule}\n"));
        }
    }

    let body = ConfigBody::new(out)?;
    Ok(GeneratedConfig {
        body,
        tun_enabled,
        controller_is_unix_socket,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::time::Timestamp;
    use crate::system::capability::{
        Capability, CapabilityEvidence, CapabilityKind, CapabilityStatus,
    };

    const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

    fn cap(kind: CapabilityKind, status: CapabilityStatus) -> Capability {
        Capability::new(kind, status, CapabilityEvidence::new("test", "n/a", NOW))
    }

    fn spec() -> GenerationSpec {
        GenerationSpec {
            mixed_port: 7890,
            controller: ControllerEndpoint::parse("127.0.0.1:9090").expect("valid"),
            secret: "s3cret".to_owned(),
            proxies_fragment: "proxies:\n  - {name: HK, type: ss, server: 1.1.1.1, port: 443}"
                .to_owned(),
            groups: vec![ProxyGroup::select("PROXY", vec!["HK".to_owned()])],
            rules: vec!["MATCH,PROXY".to_owned()],
            tun_requested: false,
        }
    }

    fn capable_tun() -> CapabilitySet {
        CapabilitySet::new(vec![
            cap(CapabilityKind::TunDevice, CapabilityStatus::Supported),
            cap(CapabilityKind::NetAdmin, CapabilityStatus::Supported),
        ])
    }

    #[test]
    fn generates_complete_config_not_just_proxies() {
        let generated = generate(&spec(), &CapabilitySet::default()).expect("valid");
        let text = generated.body.as_str();

        // The fields the converter does not supply must all be present.
        assert!(text.contains("mixed-port: 7890"));
        assert!(text.contains("external-controller: 127.0.0.1:9090"));
        assert!(text.contains("secret: \"s3cret\""));
        assert!(text.contains("proxy-groups:"));
        assert!(text.contains("rules:"));
        assert!(text.contains("proxies:"));
    }

    #[test]
    fn cors_is_always_narrowed() {
        let text = generate(&spec(), &CapabilitySet::default())
            .expect("valid")
            .body
            .into_inner();
        assert!(text.contains("allow-origins: []"));
        assert!(text.contains("allow-private-network: false"));
        assert!(!text.contains("allow-origins: [\"*\"]"));
    }

    #[test]
    fn generation_is_deterministic() {
        let caps = capable_tun();
        let a = generate(&spec(), &caps).expect("valid");
        let b = generate(&spec(), &caps).expect("valid");
        assert_eq!(a.body.as_str(), b.body.as_str());
        assert_eq!(a.body.checksum(), b.body.checksum());
    }

    #[test]
    fn tun_omitted_when_not_requested() {
        let generated = generate(&spec(), &capable_tun()).expect("valid");
        assert!(!generated.tun_enabled);
        assert!(!generated.body.as_str().contains("tun:"));
    }

    #[test]
    fn tun_enabled_only_when_requested_and_capable() {
        let mut s = spec();
        s.tun_requested = true;

        let generated = generate(&s, &capable_tun()).expect("valid");
        assert!(generated.tun_enabled);
        assert!(generated.body.as_str().contains("tun:"));
        assert!(generated.body.as_str().contains("enable: true"));
    }

    /// Safety-critical: every non-Supported capability must withhold TUN.
    #[test]
    fn tun_withheld_for_every_non_supported_status() {
        for tun_status in [
            CapabilityStatus::Unavailable,
            CapabilityStatus::Misconfigured,
            CapabilityStatus::Unknown,
            CapabilityStatus::Unsupported,
        ] {
            let caps = CapabilitySet::new(vec![
                cap(CapabilityKind::TunDevice, tun_status),
                cap(CapabilityKind::NetAdmin, CapabilityStatus::Supported),
            ]);
            let mut s = spec();
            s.tun_requested = true;
            let generated = generate(&s, &caps).expect("valid");
            assert!(
                !generated.tun_enabled,
                "TUN must be withheld when device status is {tun_status:?}"
            );
            assert!(!generated.body.as_str().contains("tun:"));
        }
    }

    #[test]
    fn tun_withheld_when_device_ok_but_capability_missing() {
        let caps = CapabilitySet::new(vec![
            cap(CapabilityKind::TunDevice, CapabilityStatus::Supported),
            cap(CapabilityKind::NetAdmin, CapabilityStatus::Unavailable),
        ]);
        let mut s = spec();
        s.tun_requested = true;
        let generated = generate(&s, &caps).expect("valid");
        assert!(!generated.tun_enabled);
    }

    #[test]
    fn unix_socket_controller_is_rendered_as_such() {
        let mut s = spec();
        s.controller = ControllerEndpoint::parse("/run/proxy-agent/mihomo.sock").expect("valid");
        let generated = generate(&s, &CapabilitySet::default()).expect("valid");
        assert!(generated.controller_is_unix_socket);
        let text = generated.body.as_str();
        assert!(text.contains("external-controller-unix: /run/proxy-agent/mihomo.sock"));
        assert!(!text.contains("\nexternal-controller: "));
    }

    #[test]
    fn empty_rules_fall_back_to_match_direct() {
        let mut s = spec();
        s.rules.clear();
        let text = generate(&s, &CapabilitySet::default())
            .expect("valid")
            .body
            .into_inner();
        assert!(text.contains("MATCH,DIRECT"));
    }

    #[test]
    fn rejects_empty_secret() {
        let mut s = spec();
        s.secret = "  ".to_owned();
        assert!(generate(&s, &CapabilitySet::default()).is_err());
    }

    #[test]
    fn rejects_secret_with_whitespace() {
        let mut s = spec();
        s.secret = "a b".to_owned();
        assert!(generate(&s, &CapabilitySet::default()).is_err());
    }

    /// An empty node list must fail loudly rather than produce a dead config.
    #[test]
    fn rejects_node_less_fragment() {
        let mut s = spec();
        s.proxies_fragment = "proxies:\n".to_owned();
        let err = generate(&s, &CapabilitySet::default()).expect_err("must reject");
        assert!(err.to_string().contains("no nodes"));
    }

    #[test]
    fn controller_accepts_loopback_forms() {
        for address in ["127.0.0.1:9090", "[::1]:9090", "localhost:9090"] {
            assert!(
                ControllerEndpoint::parse(address).is_ok(),
                "{address} should be accepted"
            );
        }
    }

    /// Wildcard binds expose restart and upgrade endpoints to the network.
    #[test]
    fn controller_rejects_wildcard_and_remote_hosts() {
        for address in [
            ":9090",
            "0.0.0.0:9090",
            "[::]:9090",
            "192.168.1.10:9090",
            "example.com:9090",
        ] {
            assert!(
                ControllerEndpoint::parse(address).is_err(),
                "{address} must be rejected"
            );
        }
    }

    #[test]
    fn controller_rejects_empty_and_portless() {
        assert!(ControllerEndpoint::parse("").is_err());
        assert!(ControllerEndpoint::parse("127.0.0.1").is_err());
        assert!(ControllerEndpoint::parse("127.0.0.1:notaport").is_err());
    }
}
