//! Wire types for the kernel's control API.
//!
//! Field names and shapes are taken from the upstream API as measured against
//! v1.19.30, not from documentation: several details are easy to get wrong and
//! fail *silently* rather than loudly.
//!
//! * `GET /proxies` returns an **object keyed by name**, not an array. Parsing it
//!   as an array yields nothing, with no error.
//! * `GET /rules` returns its array **wrapped under a `rules` key**.
//! * Unconfigured ports read **`0`, not `null`**, so presence must be tested
//!   against zero.
//!
//! Missing optional fields are `Option`, while a malformed body is a
//! deserialization error. Keeping those distinct matters: a renamed field would
//! otherwise make a healthy instance look unhealthy, and the activation path
//! would roll back a configuration that was fine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `GET /version`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct VersionResponse {
    /// Whether this is the Meta kernel.
    #[serde(default)]
    pub meta: bool,
    /// Version string, e.g. `v1.19.30`.
    pub version: String,
}

/// `GET /configs`.
///
/// Only the fields this adapter needs are declared. `serde` ignores the rest, so
/// an upstream addition does not break parsing — which is deliberate, since the
/// full response is a general-settings struct that grows over time.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ConfigsResponse {
    /// HTTP inbound port.
    #[serde(default)]
    pub port: u16,
    /// SOCKS inbound port.
    #[serde(default, rename = "socks-port")]
    pub socks_port: u16,
    /// Mixed HTTP+SOCKS inbound port.
    #[serde(default, rename = "mixed-port")]
    pub mixed_port: u16,
    /// Redirect inbound port.
    #[serde(default, rename = "redir-port")]
    pub redir_port: u16,
    /// TProxy inbound port.
    #[serde(default, rename = "tproxy-port")]
    pub tproxy_port: u16,
    /// Current mode: `rule`, `global`, or `direct`.
    #[serde(default)]
    pub mode: String,
    /// Current log level.
    #[serde(default, rename = "log-level")]
    pub log_level: String,
    /// Whether LAN access is allowed.
    #[serde(default, rename = "allow-lan")]
    pub allow_lan: bool,
    /// TUN settings, as configured.
    #[serde(default)]
    pub tun: TunSettings,
}

impl ConfigsResponse {
    /// Returns the mixed port when one is configured.
    ///
    /// `0` means "not configured", which the kernel uses instead of `null`, so a
    /// zero here must not be reported as a listening port.
    #[must_use]
    pub const fn mixed_port_or_none(&self) -> Option<u16> {
        if self.mixed_port == 0 {
            None
        } else {
            Some(self.mixed_port)
        }
    }

    /// Returns the HTTP port when one is configured.
    #[must_use]
    pub const fn http_port_or_none(&self) -> Option<u16> {
        if self.port == 0 {
            None
        } else {
            Some(self.port)
        }
    }

    /// Returns the SOCKS port when one is configured.
    #[must_use]
    pub const fn socks_port_or_none(&self) -> Option<u16> {
        if self.socks_port == 0 {
            None
        } else {
            Some(self.socks_port)
        }
    }

    /// Every configured inbound port, for connectivity checks.
    #[must_use]
    pub fn inbound_ports(&self) -> Vec<u16> {
        [
            self.mixed_port_or_none(),
            self.http_port_or_none(),
            self.socks_port_or_none(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// The `tun` sub-object of `GET /configs`.
///
/// Only `enable` is read. Its value reflects configuration *intent*, not whether
/// the kernel could actually create a device, so it must not be used to infer
/// capability.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct TunSettings {
    /// Whether TUN is enabled in the running configuration.
    #[serde(default)]
    pub enable: bool,
}

/// `GET /proxies`.
///
/// The kernel returns an object keyed by proxy name; there is no array form.
#[derive(Debug, Clone, Deserialize)]
pub struct ProxiesResponse {
    /// Proxies indexed by name.
    #[serde(default)]
    pub proxies: BTreeMap<String, ProxyEntry>,
}

/// One proxy or strategy group.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProxyEntry {
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Protocol or group type.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Whether the node is considered alive.
    #[serde(default)]
    pub alive: bool,
    /// Currently selected member, present only for groups.
    #[serde(default)]
    pub now: Option<String>,
    /// Member names, present only for groups.
    #[serde(default)]
    pub all: Option<Vec<String>>,
    /// Recent delay measurements.
    #[serde(default)]
    pub history: Vec<DelayHistory>,
}

impl ProxyEntry {
    /// Whether this entry is a strategy group rather than a node.
    #[must_use]
    pub fn is_group(&self) -> bool {
        self.all.is_some()
    }

    /// The most recent measured delay, if any.
    #[must_use]
    pub fn latest_delay(&self) -> Option<u32> {
        self.history
            .last()
            .and_then(|entry| u32::try_from(entry.delay).ok())
    }
}

/// One delay measurement.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DelayHistory {
    /// Measured delay in milliseconds.
    #[serde(default)]
    pub delay: i64,
}

/// `GET /rules`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RulesResponse {
    /// Rules, in evaluation order.
    #[serde(default)]
    pub rules: Vec<RuleEntry>,
}

/// One rule.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RuleEntry {
    /// Rule type, e.g. `DomainSuffix`.
    #[serde(default, rename = "type")]
    pub kind: String,
    /// The matched value.
    #[serde(default)]
    pub payload: String,
    /// The outbound target.
    #[serde(default)]
    pub proxy: String,
}

/// `GET /proxies/:name/delay`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DelayResponse {
    /// Measured delay in milliseconds.
    #[serde(default)]
    pub delay: i64,
    /// Error message when the test failed.
    #[serde(default)]
    pub message: Option<String>,
}

/// The request body for `PUT /configs`.
///
/// Exactly one variant is populated. `force` is deliberately absent: the kernel
/// *accepts* it, so its absence has to be enforced here rather than relied on
/// upstream to reject.
#[derive(Debug, Clone, Serialize)]
pub struct ReloadBody {
    /// Inline configuration document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// Path the kernel may read itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ReloadBody {
    /// An inline payload body.
    #[must_use]
    pub fn payload(document: impl Into<String>) -> Self {
        Self {
            payload: Some(document.into()),
            path: None,
        }
    }

    /// A path body.
    #[must_use]
    pub fn path(path: impl Into<String>) -> Self {
        Self {
            payload: None,
            path: Some(path.into()),
        }
    }

    /// Serializes to JSON.
    ///
    /// # Errors
    /// Returns a message when serialization fails, which for this shape cannot
    /// happen; the signature exists so the caller never unwraps.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| e.to_string())
    }
}

/// The request body for `PATCH /configs`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct PatchBody {
    /// New mode, if changing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// New log level, if changing.
    #[serde(skip_serializing_if = "Option::is_none", rename = "log-level")]
    pub log_level: Option<String>,
}

/// The kernel's error body.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ErrorResponse {
    /// Human-readable message.
    #[serde(default)]
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a live kernel, so a rename upstream fails here rather than
    /// silently degrading a health check.
    const LIVE_VERSION: &str = r#"{"meta":true,"version":"v1.19.30"}"#;

    const LIVE_CONFIGS: &str = r#"{"port":0,"socks-port":0,"redir-port":0,"tproxy-port":0,
        "mixed-port":17892,"tun":{"enable":false,"device":"","stack":"gVisor"},
        "mode":"rule","log-level":"info","allow-lan":false}"#;

    #[test]
    fn parses_live_version_response() {
        let parsed: VersionResponse = serde_json::from_str(LIVE_VERSION).expect("parses");
        assert!(parsed.meta, "meta=true identifies the Meta kernel");
        assert_eq!(parsed.version, "v1.19.30");
    }

    #[test]
    fn parses_live_configs_response() {
        let parsed: ConfigsResponse = serde_json::from_str(LIVE_CONFIGS).expect("parses");
        assert_eq!(parsed.mixed_port, 17892);
        assert_eq!(parsed.mode, "rule");
        assert_eq!(parsed.log_level, "info");
        assert!(!parsed.tun.enable);
    }

    /// The kernel reports unconfigured ports as zero, so zero must not be
    /// mistaken for a listening port.
    #[test]
    fn zero_ports_are_absent_not_present() {
        let parsed: ConfigsResponse = serde_json::from_str(LIVE_CONFIGS).expect("parses");
        assert_eq!(parsed.mixed_port_or_none(), Some(17892));
        assert_eq!(
            parsed.http_port_or_none(),
            None,
            "port 0 means unconfigured"
        );
        assert_eq!(parsed.socks_port_or_none(), None);
        assert_eq!(parsed.inbound_ports(), vec![17892]);
    }

    /// `/proxies` is an object keyed by name. Treating it as an array would yield
    /// an empty list rather than an error.
    #[test]
    fn proxies_is_an_object_keyed_by_name() {
        let body = r#"{"proxies":{
            "DIRECT":{"name":"DIRECT","type":"Direct","alive":true},
            "PROXY":{"name":"PROXY","type":"Selector","alive":true,"now":"DIRECT","all":["DIRECT"]}
        }}"#;
        let parsed: ProxiesResponse = serde_json::from_str(body).expect("parses");

        assert_eq!(parsed.proxies.len(), 2, "both entries must be present");
        let proxy = parsed.proxies.get("PROXY").expect("group present");
        assert!(proxy.is_group());
        assert_eq!(proxy.now.as_deref(), Some("DIRECT"));
        assert!(
            !parsed
                .proxies
                .get("DIRECT")
                .expect("node present")
                .is_group()
        );
    }

    #[test]
    fn an_array_shaped_proxies_body_fails_loudly() {
        let body = r#"{"proxies":[]}"#;
        assert!(
            serde_json::from_str::<ProxiesResponse>(body).is_err(),
            "a bare array must be a parse error, not an empty result"
        );
    }

    /// `/rules` wraps its array under a key.
    #[test]
    fn rules_are_wrapped_under_a_key() {
        let body = r#"{"rules":[{"index":0,"type":"Match","payload":"","proxy":"DIRECT"}]}"#;
        let parsed: RulesResponse = serde_json::from_str(body).expect("parses");
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].kind, "Match");
        assert_eq!(parsed.rules[0].proxy, "DIRECT");
    }

    #[test]
    fn latest_delay_uses_the_most_recent_measurement() {
        let body = r#"{"proxies":{"A":{"name":"A","type":"Shadowsocks","alive":true,
            "history":[{"delay":100},{"delay":42}]}}}"#;
        let parsed: ProxiesResponse = serde_json::from_str(body).expect("parses");
        assert_eq!(
            parsed.proxies.get("A").expect("present").latest_delay(),
            Some(42)
        );
    }

    #[test]
    fn negative_delay_is_not_reported_as_a_measurement() {
        let body = r#"{"proxies":{"A":{"name":"A","type":"Direct","history":[{"delay":-1}]}}}"#;
        let parsed: ProxiesResponse = serde_json::from_str(body).expect("parses");
        assert_eq!(
            parsed.proxies.get("A").expect("present").latest_delay(),
            None,
            "a negative delay signals failure, not a fast node"
        );
    }

    /// `force` must be unrepresentable, because the kernel would accept it.
    #[test]
    fn reload_body_never_contains_force() {
        let payload = ReloadBody::payload("mixed-port: 7890");
        let json = payload.to_json().expect("serializes");
        assert!(json.contains("payload"));
        assert!(
            !json.contains("force"),
            "force must not be expressible: {json}"
        );

        let by_path = ReloadBody::path("/var/lib/configs/v1.yaml");
        let json = by_path.to_json().expect("serializes");
        assert!(json.contains("path"));
        assert!(!json.contains("force"));
    }

    #[test]
    fn reload_body_omits_the_unused_field() {
        let json = ReloadBody::payload("x").to_json().expect("serializes");
        assert!(
            !json.contains("path"),
            "only one variant may be present: {json}"
        );
    }

    #[test]
    fn parses_the_kernel_error_body() {
        let body = r#"{"message":"yaml: unmarshal errors"}"#;
        let parsed: ErrorResponse = serde_json::from_str(body).expect("parses");
        assert!(parsed.message.contains("unmarshal"));
    }

    #[test]
    fn unknown_fields_are_ignored_so_upstream_additions_do_not_break_us() {
        let body = r#"{"meta":true,"version":"v1.19.30","brandNewField":{"nested":1}}"#;
        assert!(serde_json::from_str::<VersionResponse>(body).is_ok());
    }
}
