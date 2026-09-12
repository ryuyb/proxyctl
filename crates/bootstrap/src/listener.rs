//! Startup checks for the network listener.
//!
//! # The one rule, and why it has no exception
//!
//! **Listening on a TCP port requires at least one issued token.** Not "unless it
//! is loopback", not "unless the host is trusted" — always.
//!
//! Over a unix socket the caller's identity comes from the kernel: `SO_PEERCRED`
//! reports the uid, and the socket's file permissions decide who may connect. Over
//! TCP there is nothing to ask, so the token a caller presents *is* the identity.
//! With no token issued, every request would either be refused (an agent that
//! cannot be controlled) or accepted (an agent anyone can control), and the second
//! is one missing check away.
//!
//! Loopback gets no exemption because loopback is not a trust boundary. Any local
//! process — including one in a container sharing the network namespace — can
//! reach `127.0.0.1`, so "it is only local" would import a much weaker assumption
//! than the socket it is replacing. One rule is also one thing to verify; a
//! conditional rule has a second path through it, and that path is where the
//! mistake would live.
//!
//! # Warnings that must be loud
//!
//! Two situations are legal and still worth saying out loud at every start,
//! because both are easy to reach by following an example:
//!
//! * A bearer token over plain HTTP is readable by anything on the path. A
//!   deployment that intends remote access should terminate TLS in front.
//! * A non-loopback bind with no CORS origins means a browser cannot use it, which
//!   is surprising when the goal was a web UI.

use std::net::SocketAddr;

/// What the listener configuration requires the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerDecision {
    /// Do not listen: the agent serves the unix socket only.
    SocketOnly,
    /// Listen on this address.
    Listen {
        /// The parsed address.
        address: SocketAddr,
        /// Whether the address is reachable from off-host.
        off_host: bool,
    },
}

/// Why a listener configuration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerRefusal {
    /// The address could not be parsed.
    Unparsable {
        /// The value as written.
        bind: String,
        /// Why it failed.
        reason: String,
    },
    /// A port was requested with no token issued.
    NoToken {
        /// The address that would have been bound.
        bind: String,
    },
}

impl ListenerRefusal {
    /// A message naming what to do about it.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Unparsable { bind, reason } => {
                format!("the configured api.bind {bind:?} is not an address: {reason}")
            }
            Self::NoToken { bind } => format!(
                "refusing to listen on {bind}: no API token has been issued, and over TCP a token \
                 is the only thing identifying a caller. Issue one with \
                 `proxyctl token issue --principal <name> --role admin`, or remove [api] bind to \
                 keep the agent on the unix socket only"
            ),
        }
    }
}

/// Decides whether and where to listen.
///
/// `has_tokens` is whether any principal exists. It is passed in rather than
/// looked up here so this function stays a pure decision: the caller owns the
/// store, and a policy that reads the database would be hard to test at its edges.
///
/// # Errors
///
/// Returns [`ListenerRefusal`] when the address is unusable, or when a port was
/// requested with nothing to authenticate callers.
pub fn decide(bind: Option<&str>, has_tokens: bool) -> Result<ListenerDecision, ListenerRefusal> {
    // A missing, blank, or explicitly disabled bind all mean the same thing.
    let Some(bind) = bind
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != "none")
    else {
        return Ok(ListenerDecision::SocketOnly);
    };

    if !has_tokens {
        return Err(ListenerRefusal::NoToken {
            bind: bind.to_owned(),
        });
    }

    let address: SocketAddr = bind.parse().map_err(|e| ListenerRefusal::Unparsable {
        bind: bind.to_owned(),
        reason: format!("{e}"),
    })?;

    Ok(ListenerDecision::Listen {
        address,
        off_host: !address.ip().is_loopback(),
    })
}

/// Warnings to print at startup, given a decision that was accepted.
///
/// Returned rather than printed so the wording is testable and so a caller decides
/// where warnings go. They are warnings and not refusals: both describe a
/// deployment that works for its stated purpose.
#[must_use]
pub fn warnings(bind: Option<&str>, cors_origins: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(bind) = bind.map(str::trim).filter(|b| !b.is_empty()) else {
        return out;
    };
    let Ok(address) = bind.parse::<SocketAddr>() else {
        return out;
    };

    if !address.ip().is_loopback() {
        out.push(format!(
            "listening on {address}, which is reachable from other machines. The API speaks plain \
             HTTP, so the bearer token is readable by anything on the network path. Put a reverse \
             proxy with TLS in front if this is not a network you control."
        ));
    }

    if !address.ip().is_loopback() && cors_origins.is_empty() {
        out.push(
            "no api.cors_origins are configured, so a browser on another origin cannot call this \
             agent. Set them to the origin you serve the web UI from, or use a client that is not \
             a browser."
                .to_owned(),
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default: no bind at all means the socket only.
    #[test]
    fn no_bind_means_socket_only() {
        assert_eq!(
            decide(None, false).expect("ok"),
            ListenerDecision::SocketOnly
        );
        assert_eq!(
            decide(None, true).expect("ok"),
            ListenerDecision::SocketOnly
        );
    }

    /// A blank value is the same as an absent one, so a config line left empty
    /// does not accidentally mean "listen anywhere".
    #[test]
    fn a_blank_bind_means_socket_only() {
        for value in ["", "   ", "none"] {
            assert_eq!(
                decide(Some(value), true).expect("ok"),
                ListenerDecision::SocketOnly,
                "{value:?}"
            );
        }
    }

    /// The rule with no exception: a port without a token is refused.
    #[test]
    fn a_port_without_a_token_is_refused() {
        for bind in ["127.0.0.1:8765", "0.0.0.0:8765", "[::1]:8765"] {
            let refusal = decide(Some(bind), false).expect_err("must be refused");
            assert!(
                matches!(refusal, ListenerRefusal::NoToken { .. }),
                "{bind}: {refusal:?}"
            );
            // The message must say what to do, not only what failed.
            let message = refusal.message();
            assert!(message.contains("proxyctl token issue"), "{message}");
        }
    }

    /// Loopback gets no exemption, which is the decision most likely to be
    /// questioned, so it is asserted directly.
    #[test]
    fn loopback_is_not_exempt_from_the_token_requirement() {
        let refusal = decide(Some("127.0.0.1:8765"), false).expect_err("must be refused");
        assert!(matches!(refusal, ListenerRefusal::NoToken { .. }));
    }

    #[test]
    fn a_port_with_a_token_is_accepted() {
        let decision = decide(Some("127.0.0.1:8765"), true).expect("ok");
        match decision {
            ListenerDecision::Listen { address, off_host } => {
                assert_eq!(address.port(), 8765);
                assert!(!off_host, "loopback is not off-host");
            }
            other => panic!("expected a listener, got {other:?}"),
        }
    }

    /// Reachability decides `off_host`, which the warnings key off.
    #[test]
    fn reachability_is_derived_from_the_address() {
        for (bind, expected) in [
            ("0.0.0.0:8765", true),
            ("127.0.0.1:8765", false),
            ("[::1]:8765", false),
            ("[::]:8765", true),
        ] {
            match decide(Some(bind), true).expect("ok") {
                ListenerDecision::Listen { off_host, .. } => {
                    assert_eq!(off_host, expected, "{bind}");
                }
                other => panic!("{bind}: expected a listener, got {other:?}"),
            }
        }
    }

    /// An unparsable address is reported with the value as written, so the message
    /// points at the configuration line rather than at a parse error alone.
    #[test]
    fn an_unparsable_bind_is_reported() {
        for bind in ["not-an-address", "127.0.0.1", ":8765", "localhost:8765"] {
            let refusal = decide(Some(bind), true).expect_err("must be refused");
            match refusal {
                ListenerRefusal::Unparsable { bind: reported, .. } => {
                    assert_eq!(reported, bind, "the message must quote the value");
                }
                other => panic!("{bind}: expected Unparsable, got {other:?}"),
            }
        }
    }

    /// A hostname is refused rather than resolved: resolution at startup makes the
    /// bound address depend on DNS, which is exactly what a listener should not do.
    #[test]
    fn a_hostname_is_not_resolved() {
        assert!(matches!(
            decide(Some("localhost:8765"), true),
            Err(ListenerRefusal::Unparsable { .. })
        ));
    }

    /// The two situations worth warning about.
    #[test]
    fn an_off_host_bind_warns_about_plain_http() {
        let warnings = warnings(Some("0.0.0.0:8765"), &["https://ui.example.com".to_owned()]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("TLS"), "{warnings:?}");
        assert!(warnings[0].contains("plain HTTP"), "{warnings:?}");
    }

    /// A remote bind with no origins means a browser cannot use it, which is
    /// surprising precisely when a web UI was the goal.
    #[test]
    fn an_off_host_bind_without_origins_warns_about_cors() {
        let warnings = warnings(Some("0.0.0.0:8765"), &[]);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings.iter().any(|w| w.contains("cors_origins")),
            "{warnings:?}"
        );
    }

    /// A loopback bind with no browser origins is unremarkable and must not warn:
    /// a quiet start is what makes the other warnings noticeable.
    #[test]
    fn a_loopback_bind_warns_about_nothing() {
        assert!(warnings(Some("127.0.0.1:8765"), &[]).is_empty());
        assert!(warnings(None, &[]).is_empty());
    }

    /// A loopback bind with origins is also quiet: the origins are for a web UI
    /// served from elsewhere, which is a deliberate arrangement.
    #[test]
    fn a_loopback_bind_with_origins_warns_about_nothing() {
        assert!(
            warnings(
                Some("127.0.0.1:8765"),
                &["http://localhost:5173".to_owned()]
            )
            .is_empty()
        );
    }
}
