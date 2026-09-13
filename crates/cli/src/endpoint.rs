//! Where a client connects, and with what credential.
//!
//! # One type rather than two optional fields
//!
//! A unix socket needs no credential: the socket's file permissions are the whole
//! boundary on the agent side, and a caller that can open it has already passed
//! them. A TCP endpoint needs a token, because there the token *is* the identity —
//! nothing else distinguishes one caller from another.
//!
//! Those are two self-consistent configurations, so they are two variants. Two
//! optional fields would admit "a URL and a socket path" and "a URL with no token",
//! and every one of those combinations would need a runtime check somewhere. A
//! type makes them unrepresentable, which is cheaper than checking.
//!
//! # Which one, decided by the value
//!
//! `--socket` carries both, distinguished by `://`. The alternative was a separate
//! `--server`, and the cost of one flag is a slightly odd reading of
//! `--socket http://...`; the cost of two is that every operator has to know which
//! one to reach for, and a script that wants to switch between local and remote
//! edits a flag name rather than a value.

use std::path::{Path, PathBuf};

/// Where the agent is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A unix socket path. The default, and the only local transport.
    Socket(PathBuf),
    /// A URL reached over TCP, with a bearer token.
    Remote {
        /// The base URL, without a trailing slash.
        base_url: String,
        /// The bearer token.
        token: String,
    },
}

/// The `/api/v1` prefix.
///
/// Defined here rather than in `command` because this module builds request URLs
/// and `client` sits below `command`: a transport that reached upward for its
/// prefix would invert the layering, which an architecture test enforces. A test
/// asserts this equals the server's own constant.
pub const API_PREFIX: &str = "/api/v1";

/// The environment variable naming a remote token.
pub const TOKEN_ENV: &str = "PROXYCTL_TOKEN";

/// The scheme separator that marks a value as a URL rather than a path.
const SCHEME: &str = "://";

impl Endpoint {
    /// Whether this endpoint leaves the machine.
    ///
    /// Used to decide whether a plaintext connection deserves a warning: traffic
    /// to `127.0.0.1` never touches a network, so there is nothing to intercept.
    #[must_use]
    pub fn leaves_the_machine(&self) -> bool {
        match self {
            Self::Socket(_) => false,
            Self::Remote { base_url, .. } => !is_loopback_url(base_url),
        }
    }

    /// A short label for messages, without the token.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Socket(path) => path.display().to_string(),
            Self::Remote { base_url, .. } => base_url.clone(),
        }
    }

    /// The token, when this endpoint has one.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        match self {
            Self::Socket(_) => None,
            Self::Remote { token, .. } => Some(token),
        }
    }
}

/// Why an endpoint could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    /// A URL was given without a token.
    ///
    /// Refused here rather than left to the agent: a `401` means "the credential
    /// is wrong", while this means "no credential was offered", and the two send an
    /// operator to different places. Reporting the second as the first wastes the
    /// trip.
    MissingToken {
        /// The URL that would have been called.
        base_url: String,
    },
    /// The URL could not be parsed.
    UnusableUrl {
        /// The value as written.
        url: String,
        /// Why it failed.
        reason: String,
    },
}

impl EndpointError {
    /// A message naming what to do.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::MissingToken { base_url } => format!(
                "talking to {base_url} requires a token: pass --token, or set {TOKEN_ENV}. A token is the only thing identifying a caller over TCP — issue one with `proxyctl token issue` on the agent host"
            ),
            Self::UnusableUrl { url, reason } => {
                format!("{url:?} is not a usable URL: {reason}")
            }
        }
    }
}

/// Resolves where to connect.
///
/// Precedence, highest first: the explicit argument, then the environment, then
/// the documented socket default. `token` is the explicit credential; a URL from
/// any source still needs one.
///
/// # Errors
///
/// Returns [`EndpointError::MissingToken`] when a URL is given with no token, and
/// [`EndpointError::UnusableUrl`] when a URL cannot be parsed.
pub fn resolve(
    socket_or_url: Option<&Path>,
    explicit_token: Option<&str>,
) -> Result<Endpoint, EndpointError> {
    // The distinct-path property matters: a value containing `://` can never be a
    // valid socket path, and a socket path can never contain one, so the two are
    // unambiguous without a separate flag.
    let raw = socket_or_url
        .map(|p| p.display().to_string())
        .or_else(|| {
            std::env::var(crate::client::SOCKET_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        // A URL may also come from the socket variable, so both sources feed the
        // same decision rather than one meaning "path" and the other "URL".
        .unwrap_or_else(|| crate::client::DEFAULT_SOCKET.to_owned());

    let raw = raw.trim().to_owned();
    if !raw.contains(SCHEME) {
        return Ok(Endpoint::Socket(PathBuf::from(raw)));
    }

    let token = explicit_token
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            std::env::var(TOKEN_ENV)
                .ok()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
        });

    let Some(token) = token else {
        return Err(EndpointError::MissingToken { base_url: raw });
    };

    let base_url = normalize_url(&raw)?;
    Ok(Endpoint::Remote { base_url, token })
}

/// Validates and normalizes a base URL.
///
/// The scheme is required: `reqwest` would reject a bare host anyway, but with a
/// message about a missing scheme rather than about the configuration the operator
/// wrote.
fn normalize_url(raw: &str) -> Result<String, EndpointError> {
    let unusable = |reason: &str| EndpointError::UnusableUrl {
        url: raw.to_owned(),
        reason: reason.to_owned(),
    };

    let (scheme, rest) = raw
        .split_once(SCHEME)
        .ok_or_else(|| unusable("no scheme"))?;
    if !matches!(scheme, "http" | "https") {
        return Err(unusable("the scheme must be http or https"));
    }
    // The host is the part before any path or query. Checking only that `rest` is
    // non-empty would accept `https:///path`, whose host is empty — a typo that
    // would otherwise surface as a connection error naming a path.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.trim().is_empty() {
        return Err(unusable("no host"));
    }
    // A trailing slash would produce a doubled path when a request path is
    // appended, so it is removed here rather than at every call site.
    Ok(raw.trim_end_matches('/').to_owned())
}

/// Whether a URL addresses this machine only.
///
/// Deliberately string-based and narrow. Resolving a name would make the answer
/// depend on DNS at the moment of the warning, and the warning's purpose is to
/// describe the transport the operator configured, not to adjudicate whether their
/// particular hostname points at a loopback address.
#[must_use]
pub fn is_loopback_url(url: &str) -> bool {
    let Some((_scheme, rest)) = url.split_once(SCHEME) else {
        return false;
    };
    // Strip any path, then any userinfo, then any port.
    let authority = rest.split('/').next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);

    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        // An IPv6 literal, optionally with a port.
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };

    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// The warning to print for a plaintext connection off this machine, if any.
///
/// Returned rather than printed so the wording is testable and the caller decides
/// where it goes.
#[must_use]
pub fn plaintext_warning(endpoint: &Endpoint) -> Option<String> {
    let Endpoint::Remote { base_url, .. } = endpoint else {
        return None;
    };
    if base_url.starts_with("https://") || !endpoint.leaves_the_machine() {
        return None;
    }
    Some(format!(
        "sending the token to {} over plain HTTP, where anything on the network path can read it. Use https:// through a reverse proxy, or a private network such as WireGuard or Tailscale.",
        endpoint.describe()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path is a socket, and a URL is remote: the distinction is the scheme
    /// separator, which cannot appear in a valid socket path.
    #[test]
    fn a_path_is_a_socket_and_a_url_is_remote() {
        let socket = resolve(Some(Path::new("/run/proxy-agent/agent.sock")), None).expect("ok");
        assert_eq!(
            socket,
            Endpoint::Socket(PathBuf::from("/run/proxy-agent/agent.sock"))
        );

        let remote = resolve(
            Some(Path::new("http://proxy.example.com:8765")),
            Some("s3cr3t"),
        )
        .expect("ok");
        assert_eq!(
            remote,
            Endpoint::Remote {
                base_url: "http://proxy.example.com:8765".to_owned(),
                token: "s3cr3t".to_owned(),
            }
        );
    }

    /// A URL with no token is refused at resolution, not left to become a 401.
    #[test]
    fn a_url_without_a_token_is_refused() {
        // The environment must not supply one by accident during the test.
        if std::env::var(TOKEN_ENV).is_ok() {
            return;
        }
        let error = resolve(Some(Path::new("http://host:1")), None).expect_err("must be refused");
        assert!(matches!(error, EndpointError::MissingToken { .. }));
        let message = error.message();
        assert!(message.contains("--token"), "{message}");
        assert!(message.contains(TOKEN_ENV), "{message}");
        // It must say how to obtain one, since that is the next step.
        assert!(message.contains("proxyctl token issue"), "{message}");
    }

    /// A blank token is not a token: it would authenticate nothing and the failure
    /// would appear as a 401 rather than as a missing argument.
    #[test]
    fn a_blank_token_is_treated_as_absent() {
        if std::env::var(TOKEN_ENV).is_ok() {
            return;
        }
        for blank in ["", "   "] {
            let error = resolve(Some(Path::new("http://host:1")), Some(blank))
                .expect_err("must be refused");
            assert!(
                matches!(error, EndpointError::MissingToken { .. }),
                "{blank:?}"
            );
        }
    }

    /// A trailing slash must not survive, or appending a path produces `//`.
    #[test]
    fn a_trailing_slash_is_removed() {
        let remote = resolve(Some(Path::new("http://host:1/")), Some("t")).expect("ok");
        match remote {
            Endpoint::Remote { base_url, .. } => assert_eq!(base_url, "http://host:1"),
            other => panic!("expected remote, got {other:?}"),
        }
    }

    #[test]
    fn an_unsupported_scheme_is_refused() {
        let error = resolve(Some(Path::new("ftp://host/")), Some("t")).expect_err("refused");
        assert!(matches!(error, EndpointError::UnusableUrl { .. }));
        assert!(error.message().contains("http or https"));
    }

    /// A URL with no host is a typo, and saying so beats a connection failure.
    #[test]
    fn a_url_without_a_host_is_refused() {
        for url in ["http://", "https:///path"] {
            let error = resolve(Some(Path::new(url)), Some("t")).expect_err("refused");
            assert!(matches!(error, EndpointError::UnusableUrl { .. }), "{url}");
        }
    }

    /// The warning is the whole point of distinguishing loopback: a token sent to
    /// `127.0.0.1` never crosses a network.
    #[test]
    fn loopback_urls_are_recognised() {
        for url in [
            "http://127.0.0.1:8765",
            "http://localhost:8765",
            "http://[::1]:8765",
            "https://127.0.0.1:8765",
        ] {
            assert!(is_loopback_url(url), "{url}");
        }
    }

    #[test]
    fn off_host_urls_are_not_loopback() {
        for url in [
            "http://proxy.example.com:8765",
            "http://192.168.1.5:8765",
            "http://[2001:db8::1]:8765",
            "http://10.0.0.1",
        ] {
            assert!(!is_loopback_url(url), "{url}");
        }
    }

    /// A socket never leaves the machine; a loopback URL does not either.
    #[test]
    fn leaving_the_machine_is_reported() {
        assert!(!Endpoint::Socket(PathBuf::from("/tmp/x.sock")).leaves_the_machine());
        assert!(
            !resolve(Some(Path::new("http://127.0.0.1:1")), Some("t"))
                .expect("ok")
                .leaves_the_machine()
        );
        assert!(
            resolve(Some(Path::new("http://192.168.1.5:1")), Some("t"))
                .expect("ok")
                .leaves_the_machine()
        );
    }

    /// The warning fires for plaintext off-host, and only then.
    #[test]
    fn the_plaintext_warning_fires_only_for_off_host_http() {
        let warned = resolve(Some(Path::new("http://proxy.example.com:1")), Some("t")).expect("ok");
        let warning = plaintext_warning(&warned).expect("a warning is required");
        assert!(warning.contains("plain HTTP"), "{warning}");
        // It must name the alternatives, not just the problem.
        assert!(warning.contains("https://"), "{warning}");
        assert!(
            warning.contains("WireGuard") || warning.contains("Tailscale"),
            "{warning}"
        );

        // Loopback over HTTP is silent: nothing crosses a network.
        let local = resolve(Some(Path::new("http://127.0.0.1:1")), Some("t")).expect("ok");
        assert!(plaintext_warning(&local).is_none());

        // TLS is silent.
        let tls = resolve(Some(Path::new("https://proxy.example.com:1")), Some("t")).expect("ok");
        assert!(plaintext_warning(&tls).is_none());

        // A socket is silent.
        assert!(plaintext_warning(&Endpoint::Socket(PathBuf::from("/tmp/x"))).is_none());
    }

    /// The label is used in messages, so it must never contain the token.
    #[test]
    fn the_description_never_carries_the_token() {
        let remote = Endpoint::Remote {
            base_url: "http://host:1".to_owned(),
            token: "SUPERSECRET".to_owned(),
        };
        assert!(!remote.describe().contains("SUPERSECRET"));
        assert_eq!(remote.describe(), "http://host:1");
        assert_eq!(remote.token(), Some("SUPERSECRET"));
    }

    /// A socket has no token at all, which is what makes the variant meaningful.
    #[test]
    fn a_socket_has_no_token() {
        assert!(Endpoint::Socket(PathBuf::from("/tmp/x")).token().is_none());
    }
}
