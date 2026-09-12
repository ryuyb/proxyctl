//! Subscription conversion via an external Sub-Store backend.
//!
//! # Why this adapter has a side effect, and why it is explicit
//!
//! The port's `convert` reads like a pure transformation, but the backend it
//! talks to is not stateless: `/download/:name` resolves a subscription by name
//! **before** it looks at any query parameter, so a name has to exist before
//! anything can be downloaded. Measured: a request for an unknown name returns
//! `404 RESOURCE_NOT_FOUND` even when `content=` or `url=` is supplied.
//!
//! So `convert` registers the subscription as a side effect of being called. That
//! is stated here rather than hidden, because a caller that believes conversion is
//! read-only would be wrong about what the backend now holds.
//!
//! # Registration is idempotent, and the mechanism matters
//!
//! The registered name is a pure function of the subscription ([`registration_name`]), so
//! calling `convert` twice for the same subscription does not accumulate records.
//!
//! `PATCH /api/sub/:name` updates an existing record and returns 404 when there
//! is none; `POST /api/subs` creates one and returns `DUPLICATE_KEY` when there
//! already is one. This adapter tries `PATCH` first and creates only on 404.
//!
//! The alternative — `PUT /api/subs`, which replaces the entire subscription set
//! and is therefore idempotent in one call — was rejected because it destroys
//! every other subscription in the backend. A dedicated Sub-Store would tolerate
//! it; a shared one would not, and the adapter cannot tell which it is talking to.
//!
//! # What the backend maintains for us
//!
//! `PATCH` also rewrites the collections, artifacts, and files that reference the
//! subscription. Measured against v2.39.6. The agent must therefore *not* try to
//! keep those references consistent itself; doing so would duplicate work the
//! server already does, and the two could disagree.
//!
//! # This module owns all provider knowledge
//!
//! Endpoint paths, parameter names, the `target` allow-list, the error JSON shape,
//! and the registration payload all live here and must not leak upward. The
//! application layer names none of them.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use proxy_application::ports::error::{ConverterError, PortError};
use proxy_application::ports::subscription_converter::{ConvertRequest, SubscriptionConverter};
use proxy_application::ports::types::{ConverterCapabilities, ConverterHealth};
use proxy_domain::shared::id::ConverterId;
use proxy_domain::subscription::{ConvertedProxies, SubscriptionSource, TargetFormat};

/// How long a request to the backend may take.
///
/// The backend fetches the upstream subscription itself, so this bounds a slow
/// origin rather than just a local call.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The identifier this adapter reports for itself.
pub const CONVERTER_ID: &str = "sub-store";

/// The prefix used for names this agent registers.
///
/// A stable, recognisable namespace: an operator looking at the backend can tell
/// which records the agent owns, and the agent can avoid colliding with
/// hand-made ones.
pub const NAME_PREFIX: &str = "proxy-agent";

/// Converts subscriptions through a Sub-Store backend.
#[derive(Debug, Clone)]
pub struct SubStoreConverter {
    base_url: String,
    client: reqwest::Client,
    /// Whether the base URL was verified to be loopback.
    loopback_verified: bool,
}

impl SubStoreConverter {
    /// Creates a converter for `base_url`.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::InvalidResponse`] when `base_url` is not a usable URL,
    /// or when it is not loopback and the caller did not opt in.
    ///
    /// # Why loopback is enforced here as well as at composition
    ///
    /// The backend has no authentication at all (measured: an unauthenticated
    /// `POST /api/subs` returns 201 and writes), and its default bind is every
    /// interface. A non-loopback backend therefore lets anyone who can reach it
    /// read and write this agent's subscription records. Composition checks the
    /// configured value; this checks the value actually used, because an adapter
    /// that trusts its caller to have checked is one refactor away from not being
    /// checked at all.
    pub fn new(base_url: impl Into<String>, allow_non_loopback: bool) -> Result<Self, PortError> {
        let base_url = base_url.into();
        let trimmed = base_url.trim().trim_end_matches('/').to_owned();

        if trimmed.is_empty() {
            return Err(PortError::InvalidResponse(
                "the converter base URL must not be empty".into(),
            ));
        }
        if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
            return Err(PortError::InvalidResponse(format!(
                "the converter base URL must be absolute (http or https), got {trimmed}"
            )));
        }

        let loopback_verified = is_loopback_url(&trimmed);
        if !loopback_verified && !allow_non_loopback {
            return Err(PortError::InvalidResponse(format!(
                "the converter at {trimmed} is not loopback, and the backend has no \
                 authentication: anything able to reach it can read and write this agent's \
                 subscriptions"
            )));
        }

        let client = reqwest::Client::builder()
            .user_agent(concat!("proxy-agent/", env!("CARGO_PKG_VERSION")))
            // Redirects are not followed: a backend that redirects is not one this
            // adapter configured, and following it could send a subscription URL
            // somewhere unintended.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| PortError::Storage(format!("cannot build the HTTP client: {e}")))?;

        Ok(Self {
            base_url: trimmed,
            client,
            loopback_verified,
        })
    }

    /// The configured base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether the base URL was verified to be loopback.
    #[must_use]
    pub const fn is_loopback(&self) -> bool {
        self.loopback_verified
    }

    /// Registers the subscription, creating it only if absent.
    ///
    /// `PATCH` first: it updates an existing record and reports 404 when there is
    /// none, which is an unambiguous signal. Creating only on that signal keeps a
    /// repeat call idempotent without depending on the meaning of a duplicate-key
    /// error.
    ///
    /// # Errors
    ///
    /// Returns [`ConverterError::Unreachable`] when the backend cannot be reached,
    /// and [`ConverterError::InvalidRequest`] when it rejects the payload.
    async fn register(&self, name: &str, source: &SubscriptionSource) -> Result<(), PortError> {
        let url = source.url().map(|u| u.as_str()).unwrap_or_default();
        if url.is_empty() {
            return Err(ConverterError::InvalidRequest(
                "the subscription has no URL to register".into(),
            )
            .into());
        }

        // The payload is the minimum the backend needs. `type: remote` tells it to
        // fetch the URL itself, which is what makes the registration meaningful:
        // the agent does not have to inline content on every download.
        let mut payload = json!({ "type": "remote", "url": url });
        if let Some(agent) = source.user_agent()
            && let Some(object) = payload.as_object_mut()
        {
            object.insert("ua".to_owned(), json!(agent));
        }

        let patch = self
            .client
            .patch(format!("{}/api/sub/{}", self.base_url, name))
            .timeout(REQUEST_TIMEOUT)
            .json(&payload)
            .send()
            .await
            .map_err(|e| -> PortError {
                ConverterError::Unreachable(redact(&e.to_string())).into()
            })?;

        match patch.status().as_u16() {
            // Already present and now updated.
            200 => Ok(()),
            // Not present yet: create it.
            404 => {
                let post = self
                    .client
                    .post(format!("{}/api/subs", self.base_url))
                    .timeout(REQUEST_TIMEOUT)
                    .json(&json!({
                        "name": name,
                        "type": "remote",
                        "url": url,
                    }))
                    .send()
                    .await
                    .map_err(|e| -> PortError {
                        ConverterError::Unreachable(redact(&e.to_string())).into()
                    })?;

                match post.status().as_u16() {
                    201 | 200 => Ok(()),
                    // A concurrent caller created it between the two calls. That is
                    // a success for an idempotent registration, not a conflict.
                    500 => {
                        let body = post.text().await.unwrap_or_default();
                        if body.contains("DUPLICATE_KEY") {
                            Ok(())
                        } else {
                            Err(ConverterError::InvalidRequest(redact(&body)).into())
                        }
                    }
                    status => Err(PortError::UnexpectedStatus { status }),
                }
            }
            // A name containing a slash is rejected outright, and so is anything
            // else the backend considers malformed. Both are our bug, not a
            // transient condition.
            400 | 422 | 500 => {
                let body = patch.text().await.unwrap_or_default();
                Err(ConverterError::InvalidRequest(redact(&body)).into())
            }
            status => Err(PortError::UnexpectedStatus { status }),
        }
    }

    /// Downloads the converted fragment.
    ///
    /// # Errors
    ///
    /// Maps the backend's responses onto [`ConverterError`] so the application
    /// sees a business-level reason rather than an HTTP status.
    async fn download(&self, name: &str, target: TargetFormat) -> Result<String, PortError> {
        let target = target_label(target)?;

        let response = self
            .client
            .get(format!("{}/download/{}", self.base_url, name))
            .timeout(REQUEST_TIMEOUT)
            .query(&[
                ("target", target),
                // Ask for a fresh fetch: a cached fragment would make a
                // subscription update silently return the previous result, which
                // looks like success while changing nothing.
                ("noCache", "1"),
            ])
            .send()
            .await
            .map_err(|e| -> PortError {
                ConverterError::Unreachable(redact(&e.to_string())).into()
            })?;

        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        match status {
            200 => Ok(body),
            404 => Err(ConverterError::SubscriptionNotFound.into()),
            400 => Err(ConverterError::InvalidRequest(redact(&body)).into()),
            // The backend reports an upstream fetch failure as a 500 with the
            // reason in `details`. That is the origin being unreachable, which the
            // caller must be able to tell apart from a malformed request.
            500 => {
                if body.contains("远程订阅") || body.contains("Failed to download subscription")
                {
                    Err(ConverterError::Unreachable(redact(&body)).into())
                } else if body.contains("不是受支持的") || body.contains("not supported") {
                    Err(ConverterError::UnsupportedTarget.into())
                } else {
                    Err(ConverterError::InvalidOutput(redact(&body)).into())
                }
            }
            other => Err(PortError::UnexpectedStatus { status: other }),
        }
    }
}

#[async_trait]
impl SubscriptionConverter for SubStoreConverter {
    async fn convert(&self, request: &ConvertRequest) -> Result<ConvertedProxies, PortError> {
        let name = registration_name(request.source.url().map(|u| u.as_str()).unwrap_or_default());

        // Registration is a side effect of conversion, not an optional extra: the
        // backend resolves a name before it reads any parameter, so an
        // unregistered name cannot be downloaded at all.
        self.register(&name, &request.source).await?;

        let fragment = self.download(&name, request.target).await?;

        // An empty result is a failure, never a success: propagating it would
        // activate a configuration that routes nothing while appearing healthy.
        let node_count = count_proxies(&fragment);
        // The domain refuses a blank fragment or a zero-node list. Both mean the
        // converter produced nothing usable, which the port defines as a failure
        // rather than an empty success.
        ConvertedProxies::new(fragment, node_count)
            .map_err(|e| ConverterError::InvalidOutput(e.to_string()).into())
    }

    async fn capabilities(&self) -> Result<ConverterCapabilities, PortError> {
        // The target list is the backend's allow-list, which is not discoverable
        // at runtime: an unsupported target fails with a 500 rather than an error
        // naming the supported set. So it is stated here and kept in step with
        // `target_label`.
        Ok(ConverterCapabilities {
            id: ConverterId::parse(CONVERTER_ID)
                .map_err(|e| PortError::Storage(format!("invalid converter id: {e}")))?,
            supports_targets: vec![TargetFormat::Mihomo],
            // Merging several sources in one request is a backend feature this
            // adapter does not use: the agent registers one subscription per
            // source, so it never needs the merge path.
            supports_merge_sources: false,
            version: self.probe_version().await.ok().flatten(),
        })
    }

    async fn health(&self) -> Result<ConverterHealth, PortError> {
        if !self.loopback_verified {
            return Ok(ConverterHealth::Misconfigured {
                reason: format!(
                    "the converter at {} is not loopback, and the backend has no authentication",
                    self.base_url
                ),
            });
        }

        match self.probe_version().await {
            Ok(Some(version)) => Ok(ConverterHealth::Healthy {
                version: Some(version),
            }),
            Ok(None) => Ok(ConverterHealth::Healthy { version: None }),
            Err(e) => Ok(ConverterHealth::Unreachable {
                reason: redact(&e.to_string()),
            }),
        }
    }
}

impl SubStoreConverter {
    /// Reads the backend version, if it reports one.
    ///
    /// Uses the environment endpoint, which is the documented way to identify a
    /// running backend. Its response echoes every `SUB_STORE_*` environment
    /// variable, so only the version field is kept and the body is never logged.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Unreachable`] when the backend cannot be reached.
    async fn probe_version(&self) -> Result<Option<String>, PortError> {
        let response = self
            .client
            .get(format!("{}/api/utils/env", self.base_url))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                PortError::Unreachable(Box::new(std::io::Error::other(redact(&e.to_string()))))
            })?;

        if !response.status().is_success() {
            return Err(PortError::UnexpectedStatus {
                status: response.status().as_u16(),
            });
        }

        let body: serde_json::Value = match response.json().await {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };

        Ok(body
            .get("data")
            .and_then(|d| d.get("version"))
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned))
    }
}

/// The name this agent registers a subscription under.
///
/// A pure function of the source URL, so two calls for the same subscription use
/// the same name and no records accumulate.
///
/// # Why a hash rather than the subscription identifier
///
/// The port hands this adapter a [`SubscriptionSource`], not an identifier: the
/// task is to convert *what to convert*, and the adapter must not need to know
/// which record asked. Deriving the name from the URL keeps that boundary while
/// still being deterministic.
///
/// The name must not contain `/` — the backend rejects such names outright — so
/// the hash is hex and the host is not included.
#[must_use]
pub fn registration_name(url: &str) -> String {
    format!("{NAME_PREFIX}-{:016x}", fnv1a_64(url.as_bytes()))
}

/// A stable, dependency-free digest, used only to derive a name.
///
/// Not a security primitive: it makes names deterministic and collision-resistant
/// for distinct URLs. A collision would mean two subscriptions sharing a record,
/// which the 64-bit digest makes vanishingly unlikely for a per-host subscription
/// count in the tens.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The backend's label for a target format.
///
/// The allow-list is case-sensitive and counter-intuitive: `mihomo` and `Clash`
/// are accepted while `clash` and `ALL` return 500. Verified against v2.39.6.
///
/// # Errors
///
/// Returns [`ConverterError::UnsupportedTarget`] for a format this adapter cannot
/// request.
fn target_label(target: TargetFormat) -> Result<&'static str, PortError> {
    match target {
        // Lowercase is the documented spelling for this build; `ClashMeta` and
        // `Clash` also work, but one spelling is chosen so behaviour does not
        // depend on which alias upstream keeps.
        TargetFormat::Mihomo => Ok("mihomo"),
    }
}

/// Counts the entries in a `proxies:` list.
///
/// Parsed as YAML rather than counted by scanning lines, because the fragment is
/// machine-generated and a node's own fields are inline mappings: counting `- `
/// occurrences would overcount any value that happens to start that way.
fn count_proxies(fragment: &str) -> usize {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(fragment) else {
        return 0;
    };
    value
        .get("proxies")
        .and_then(|p| p.as_sequence())
        .map_or(0, Vec::len)
}

/// Whether a URL's host is loopback.
fn is_loopback_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);

    // Strip any path, query, and userinfo before reading the host.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);

    let host = if host_port.starts_with('[') {
        // IPv6 literal: `[::1]:3001`.
        host_port
            .split(']')
            .next()
            .map(|h| h.trim_start_matches('['))
            .unwrap_or(host_port)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };

    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// Removes anything credential-shaped from a string before it is surfaced.
///
/// The backend's error bodies quote the subscription URL, which carries its own
/// credentials in the query string or userinfo. An error that reaches a log or a
/// job record must not carry them.
#[must_use]
pub fn redact(text: &str) -> String {
    // The published shape is `订阅 <name> 的远程订阅 <url> 发生错误`, and the URL
    // is the only credential-bearing part. Truncation keeps the diagnosis while
    // dropping the URL: the caller already knows which subscription it asked for.
    let mut out = String::with_capacity(text.len());
    for token in text.split_whitespace() {
        if token.starts_with("http://") || token.starts_with("https://") {
            out.push_str("<redacted-url>");
        } else {
            out.push_str(token);
        }
        out.push(' ');
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
#[path = "substore/tests.rs"]
mod tests;
