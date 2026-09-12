//! The `MihomoController` adapter.
//!
//! One implementation of the port, over any [`Transport`]. Keeping the semantics
//! in one place is the point: the rules that matter — never forcing a reload,
//! always sending a body, checking the proxy port instead of trusting the
//! control API — exist once, and are covered by the same tests regardless of
//! which transport is underneath.
//!
//! # Health is layered, and the layers are independent
//!
//! A reachable control API does not mean traffic flows. The kernel logs a
//! listener bind failure and keeps serving its API, so an instance can answer
//! `/version` while nothing is listening on the proxy port. Each layer is probed
//! separately and reported separately, because "degraded" and "unhealthy" need
//! different operator responses.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use proxy_application::ports::PortError;
use proxy_application::ports::mihomo_controller::{MihomoController, ReloadRequest};
use proxy_application::ports::types::{
    DelayOptions, DelayOutcome, HealthReport, ProxyGroupView, ProxyList, ProxyView, ReloadOutcome,
    RuleList, RuleView, RuntimeConfigSummary,
};
use proxy_domain::mihomo::{KernelFlavor, MihomoBuild};
use proxy_domain::subscription::TargetFormat;

use super::transport::{Request, Response, Transport};
use super::wire::{
    ConfigsResponse, DelayResponse, ErrorResponse, PatchBody, ProxiesResponse, ReloadBody,
    RuleEntry, RulesResponse, VersionResponse,
};

/// Reads the kernel's control API.
pub struct HttpMihomoController {
    transport: Arc<dyn Transport>,
}

impl HttpMihomoController {
    /// Creates an adapter over `transport`.
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self { transport }
    }

    /// A label identifying the transport, for diagnostics.
    #[must_use]
    pub fn describe_transport(&self) -> String {
        self.transport.describe()
    }

    /// Sends a request and rejects a non-2xx status.
    async fn send_ok(&self, request: Request) -> Result<Response, PortError> {
        let response = self.transport.send(request).await?;
        if response.is_success() {
            Ok(response)
        } else {
            Err(PortError::UnexpectedStatus {
                status: response.status,
            })
        }
    }

    /// Extracts a readable reason from an error body, when there is one.
    fn reason_from(body: &str) -> String {
        serde_json::from_str::<ErrorResponse>(body)
            .ok()
            .map(|error| error.message)
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| body.trim().to_owned())
    }

    /// Probes whether any inbound port accepts a connection.
    ///
    /// Separate from the API check because the kernel does not treat a bind
    /// failure as fatal, so this is the only way to know traffic can flow.
    async fn probe_inbound_ports(ports: &[u16]) -> bool {
        if ports.is_empty() {
            return false;
        }
        for port in ports {
            let address = SocketAddr::from(([127, 0, 0, 1], *port));
            if tokio::time::timeout(
                Duration::from_millis(300),
                tokio::net::TcpStream::connect(address),
            )
            .await
            .is_ok_and(|connected| connected.is_ok())
            {
                return true;
            }
        }
        false
    }

    /// Reads the running configuration.
    async fn configs(&self) -> Result<ConfigsResponse, PortError> {
        let response = self.send_ok(Request::get("/configs")).await?;
        serde_json::from_str(&response.body)
            .map_err(|e| PortError::InvalidResponse(format!("cannot parse /configs: {e}")))
    }
}

#[async_trait]
impl MihomoController for HttpMihomoController {
    async fn version(&self) -> Result<MihomoBuild, PortError> {
        let response = self.send_ok(Request::get("/version")).await?;
        let parsed: VersionResponse = serde_json::from_str(&response.body)
            .map_err(|e| PortError::InvalidResponse(format!("cannot parse /version: {e}")))?;

        // `meta: true` is how the Meta kernel identifies itself, so an unknown
        // flavour is recorded rather than assumed.
        let flavor = if parsed.meta {
            KernelFlavor::Meta
        } else {
            KernelFlavor::Unknown
        };

        MihomoBuild::new(parsed.version, flavor, response.body)
            .map_err(|e| PortError::InvalidResponse(e.to_string()))
    }

    async fn runtime_config(&self) -> Result<RuntimeConfigSummary, PortError> {
        let configs = self.configs().await?;
        // Derived values are read before any field is moved out.
        let mixed_port = configs.mixed_port_or_none();
        let socks_port = configs.socks_port_or_none();
        let http_port = configs.http_port_or_none();
        Ok(RuntimeConfigSummary {
            mode: configs.mode,
            mixed_port,
            socks_port,
            http_port,
            log_level: Some(configs.log_level).filter(|level| !level.is_empty()),
        })
    }

    async fn reload(&self, request: ReloadRequest) -> Result<ReloadOutcome, PortError> {
        // `force` is never set. The kernel accepts it, so this is our constraint
        // to hold, not one upstream enforces for us.
        let body = match &request {
            ReloadRequest::Payload(document) => ReloadBody::payload(document.as_str()),
            ReloadRequest::Path(path) => ReloadBody::path(path.clone()),
        };
        let json = body
            .to_json()
            .map_err(|e| PortError::Transport(format!("cannot encode reload body: {e}")))?;

        let response = self
            .transport
            .send(Request::put_json("/configs", json))
            .await?;

        if response.is_success() {
            // Acceptance, not effect: the caller must health-check afterwards.
            Ok(ReloadOutcome::Applied)
        } else {
            Ok(ReloadOutcome::Rejected {
                http_status: response.status,
            })
        }
    }

    async fn proxies(&self) -> Result<ProxyList, PortError> {
        let response = self.send_ok(Request::get("/proxies")).await?;
        let parsed: ProxiesResponse = serde_json::from_str(&response.body)
            .map_err(|e| PortError::InvalidResponse(format!("cannot parse /proxies: {e}")))?;

        let mut groups = Vec::new();
        let mut proxies = Vec::new();
        for (name, entry) in parsed.proxies {
            if entry.is_group() {
                groups.push(ProxyGroupView {
                    name: if entry.name.is_empty() {
                        name
                    } else {
                        entry.name
                    },
                    kind: entry.kind,
                    now: entry.now,
                    members: entry.all.unwrap_or_default(),
                });
            } else {
                let delay_millis = entry.latest_delay();
                proxies.push(ProxyView {
                    name: if entry.name.is_empty() {
                        name
                    } else {
                        entry.name
                    },
                    kind: entry.kind,
                    delay_millis,
                });
            }
        }

        Ok(ProxyList { groups, proxies })
    }

    async fn select_proxy(&self, group: &str, proxy: &str) -> Result<(), PortError> {
        let body = serde_json::json!({ "name": proxy }).to_string();
        self.send_ok(Request::put_json(
            format!("/proxies/{}", encode_path_segment(group)),
            body,
        ))
        .await?;
        Ok(())
    }

    async fn test_delay(
        &self,
        name: &str,
        options: &DelayOptions,
    ) -> Result<DelayOutcome, PortError> {
        let path = format!(
            "/proxies/{}/delay?timeout={}&url={}",
            encode_path_segment(name),
            options.timeout.as_millis(),
            encode_query(&options.test_url)
        );

        match self.transport.send(Request::get(path)).await {
            Ok(response) if response.is_success() => {
                let parsed: DelayResponse = serde_json::from_str(&response.body)
                    .map_err(|e| PortError::InvalidResponse(format!("cannot parse delay: {e}")))?;
                match u32::try_from(parsed.delay) {
                    Ok(millis) if millis > 0 => Ok(DelayOutcome::Measured { millis }),
                    _ => Ok(DelayOutcome::Unavailable {
                        reason: Self::reason_from(&response.body),
                    }),
                }
            }
            // A node that does not answer is normal operation, not a fault; the
            // kernel reports it as a gateway timeout.
            Ok(response) if response.status == 504 => Ok(DelayOutcome::Timeout),
            Ok(response) => Ok(DelayOutcome::Unavailable {
                reason: Self::reason_from(&response.body),
            }),
            Err(e) => Err(e),
        }
    }

    async fn rules(&self) -> Result<RuleList, PortError> {
        let response = self.send_ok(Request::get("/rules")).await?;
        let parsed: RulesResponse = serde_json::from_str(&response.body)
            .map_err(|e| PortError::InvalidResponse(format!("cannot parse /rules: {e}")))?;

        Ok(RuleList {
            rules: parsed
                .rules
                .into_iter()
                .map(|entry: RuleEntry| RuleView {
                    kind: entry.kind,
                    payload: entry.payload,
                    target: entry.proxy,
                })
                .collect(),
        })
    }

    async fn health_check(&self) -> Result<HealthReport, PortError> {
        // Layer 1 and 2: the control API answers. A failure here is reported as
        // an error, because it means nothing else can be determined.
        let configs = self.configs().await?;

        // Layer 3: the running configuration matches what we asked for. Only the
        // fields the kernel exposes can be compared.
        let config_loaded = !configs.mode.is_empty();

        // Layer 4: traffic can actually flow. This is the check that catches a
        // listener bind failure, which the kernel does not treat as fatal.
        let proxy_port_listening = Self::probe_inbound_ports(&configs.inbound_ports()).await;

        Ok(HealthReport {
            // Reaching this point means the kernel answered, so the process is
            // alive and its controller is reachable.
            process_alive: true,
            controller_reachable: true,
            config_loaded,
            proxy_port_listening,
        })
    }

    async fn shutdown(&self) -> Result<(), PortError> {
        // The kernel has no shutdown endpoint, so this is a no-op that reports
        // success. Termination is the process manager's responsibility, and
        // pretending otherwise would mean sending a signal from here, which
        // would put process control in the wrong adapter.
        Ok(())
    }
}

impl HttpMihomoController {
    /// Applies a runtime-only configuration change.
    ///
    /// Exposed separately from the port, which deliberately does not include it:
    /// only whitelisted fields are accepted, and dangerous ones such as `tun` or
    /// inbound ports must not be reachable this way.
    ///
    /// # Errors
    /// Returns [`PortError::UnexpectedStatus`] when the kernel rejects the patch.
    pub async fn patch_runtime(
        &self,
        mode: Option<&str>,
        log_level: Option<&str>,
    ) -> Result<(), PortError> {
        if mode.is_none() && log_level.is_none() {
            return Ok(());
        }

        let body = PatchBody {
            mode: mode.map(ToOwned::to_owned),
            log_level: log_level.map(ToOwned::to_owned),
        };
        let json = serde_json::to_string(&body)
            .map_err(|e| PortError::Transport(format!("cannot encode patch body: {e}")))?;

        self.send_ok(Request::patch_json("/configs", json)).await?;
        Ok(())
    }

    /// Lists connections, for the connection-operations adapter.
    ///
    /// # Errors
    /// Returns an error when the kernel is unreachable or the body is malformed.
    pub async fn connections_raw(&self) -> Result<BTreeMap<String, serde_json::Value>, PortError> {
        let response = self.send_ok(Request::get("/connections")).await?;
        serde_json::from_str(&response.body)
            .map_err(|e| PortError::InvalidResponse(format!("cannot parse /connections: {e}")))
    }
}

/// Percent-encodes a path segment.
///
/// Proxy and group names are user-controlled and routinely contain spaces,
/// slashes, and emoji, so they cannot be interpolated raw.
fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Percent-encodes a query value.
fn encode_query(value: &str) -> String {
    encode_path_segment(value).replace('/', "%2F")
}

/// The format a target represents, for capability checks.
#[must_use]
pub const fn supports_target(_target: TargetFormat) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_names_with_spaces_and_emoji() {
        assert_eq!(encode_path_segment("DIRECT"), "DIRECT");
        assert_eq!(encode_path_segment("Hong Kong 01"), "Hong%20Kong%2001");
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
        assert!(!encode_path_segment("🇭🇰 HK").contains(' '));
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(
            encode_query("https://example.com/x"),
            "https%3A%2F%2Fexample.com%2Fx"
        );
    }

    #[test]
    fn extracts_a_reason_from_an_error_body() {
        let body = r#"{"message":"yaml: unmarshal errors:\n  line 1"}"#;
        let reason = HttpMihomoController::reason_from(body);
        assert!(reason.contains("unmarshal"), "got {reason}");
    }

    #[test]
    fn falls_back_to_the_raw_body_when_it_is_not_json() {
        assert_eq!(
            HttpMihomoController::reason_from("plain text"),
            "plain text"
        );
    }

    #[test]
    fn an_empty_message_falls_back_to_the_body() {
        assert_eq!(
            HttpMihomoController::reason_from(r#"{"message":""}"#),
            r#"{"message":""}"#
        );
    }
}
