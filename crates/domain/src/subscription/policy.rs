//! Outbound fetch policy for subscription sources.
//!
//! A subscription URL is operator-supplied and becomes the target of an outbound
//! request, so it is an SSRF surface: a URL pointing at `127.0.0.1`,
//! `169.254.169.254`, or an RFC1918 address turns the agent into a probe for the
//! host's internal network.
//!
//! # Why the rule lives here and not in `SubscriptionUrl`
//!
//! [`SubscriptionUrl::parse`] models *what a URL is*, and an internal address is a
//! perfectly well-formed URL. Rejecting it at construction would make the type
//! unable to represent a fact the system legitimately needs to reason about —
//! diagnostics have to report that a subscription points inward, and tests have to
//! build such a source to exercise this very policy.
//!
//! So parsing stays permissive and this type carries *the decision*. Whether an
//! outbound request may reach a private address is an operational choice, not a
//! property of the address.
//!
//! # Deny by default
//!
//! The allow-list starts empty, which means public destinations only. This is
//! deliberately not a blacklist: private and reserved ranges keep appearing
//! (`100.64.0.0/10` and `198.18.0.0/15` were both added to the reserved set after
//! the fact), so a list of things to reject is always incomplete in a way a list
//! of things to permit is not.
//!
//! [`SubscriptionUrl::parse`]: super::source::SubscriptionUrl::parse

use std::net::IpAddr;

use crate::shared::error::DomainError;
use crate::subscription::source::SubscriptionUrl;

/// What outbound destinations a subscription fetch may reach.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubscriptionFetchPolicy {
    allow: Vec<AllowRule>,
}

/// One accepted destination.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AllowRule {
    /// An exact host name, compared case-insensitively.
    Host(String),
    /// A network in CIDR form.
    Cidr { base: IpAddr, prefix: u8 },
}

impl SubscriptionFetchPolicy {
    /// A policy that permits public destinations only.
    ///
    /// The default for every deployment: a subscription pointing inward is
    /// refused unless an operator says otherwise.
    #[must_use]
    pub fn public_only() -> Self {
        Self::default()
    }

    /// Builds a policy from configuration strings.
    ///
    /// Each entry is either a host name (`sub.internal.example`) or a CIDR block
    /// (`10.0.0.0/8`, `fd00::/8`).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidInput`] for an entry that is neither. A
    /// malformed allow-list entry is rejected at construction rather than ignored,
    /// because silently dropping one would leave the operator believing a
    /// destination is permitted when it is not — and the failure would only appear
    /// at the moment they needed it.
    pub fn parse(
        entries: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, DomainError> {
        let mut allow = Vec::new();
        for entry in entries {
            let entry = entry.into();
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                return Err(DomainError::invalid_input(
                    "an allow-list entry must not be empty",
                ));
            }
            allow.push(parse_rule(trimmed)?);
        }
        Ok(Self { allow })
    }

    /// Whether `url` may be fetched.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidInput`] naming the destination when it is not
    /// permitted. The reason is returned rather than a bare `false` so a caller can
    /// report *which* address was refused: "the subscription was rejected" without
    /// the address is not actionable.
    pub fn check(&self, url: &SubscriptionUrl) -> Result<(), DomainError> {
        let host = url.host();

        if self.permits(host) {
            return Ok(());
        }

        Err(DomainError::invalid_input(format!(
            "the subscription destination {host} is not a public address and is not in the \
             fetch allow-list; a subscription pointing at loopback, a link-local address, or a \
             private range would turn this agent into a probe for the internal network. Add it \
             to the allow-list if reaching it is intended"
        )))
    }

    /// Whether `host` is permitted.
    fn permits(&self, host: &str) -> bool {
        for rule in &self.allow {
            match rule {
                AllowRule::Host(name) => {
                    if name.eq_ignore_ascii_case(host) {
                        return true;
                    }
                }
                AllowRule::Cidr { base, prefix } => {
                    if let Ok(ip) = host.parse::<IpAddr>()
                        && ip_in_network(ip, *base, *prefix)
                    {
                        return true;
                    }
                }
            }
        }

        // No explicit permission, so fall back to the inherent check: only public
        // destinations pass. This is what makes the empty allow-list mean
        // "public only" rather than "nothing".
        url_host_is_public(host)
    }

    /// The configured allow-list entries, for diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.allow.len()
    }

    /// Whether nothing is explicitly allowed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty()
    }
}

/// Whether a bare host is a public destination.
///
/// Delegates to the same judgement `SubscriptionUrl` uses, by building the
/// smallest value that can answer it. Keeping one implementation matters: two
/// copies of this rule would drift, and the drift would be a security hole.
fn url_host_is_public(host: &str) -> bool {
    // A scheme is required by the parser; the host is what is judged, so any
    // scheme works. Using `https` keeps the value well-formed.
    SubscriptionUrl::parse(format!("https://{host}/"))
        .map(|url| url.is_public_destination())
        .unwrap_or(false)
}

/// Parses one allow-list entry.
fn parse_rule(entry: &str) -> Result<AllowRule, DomainError> {
    // A CIDR entry always contains `/`; a host name never does.
    if let Some((base, prefix)) = entry.split_once('/') {
        let base = base.trim().parse::<IpAddr>().map_err(|e| {
            DomainError::invalid_input(format!(
                "allow-list entry '{entry}' has an invalid address: {e}"
            ))
        })?;
        let prefix = prefix.trim().parse::<u8>().map_err(|e| {
            DomainError::invalid_input(format!(
                "allow-list entry '{entry}' has an invalid prefix: {e}"
            ))
        })?;
        let max = if base.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(DomainError::invalid_input(format!(
                "allow-list entry '{entry}' has a prefix longer than {max}"
            )));
        }
        return Ok(AllowRule::Cidr { base, prefix });
    }

    // A bare IP without a prefix is treated as a single address, so
    // `10.0.0.5` means just that host rather than being rejected as malformed.
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Ok(AllowRule::Cidr {
            base: ip,
            prefix: if ip.is_ipv4() { 32 } else { 128 },
        });
    }

    if entry.contains(' ') {
        return Err(DomainError::invalid_input(format!(
            "allow-list entry '{entry}' is not a host name or a CIDR block"
        )));
    }

    Ok(AllowRule::Host(entry.to_owned()))
}

/// Whether `ip` falls inside `base/prefix`.
///
/// Written out rather than using a helper because the comparison has to work
/// across both families with their different widths, and comparing only the
/// leading whole bytes avoids constructing a mask value that would overflow for
/// a `/0` or a `/128`.
fn ip_in_network(ip: IpAddr, base: IpAddr, prefix: u8) -> bool {
    match (ip, base) {
        (IpAddr::V4(ip), IpAddr::V4(base)) => {
            let ip = u32::from(ip);
            let base = u32::from(base);
            if prefix == 0 {
                return true;
            }
            let mask = u32::MAX << (32 - u32::from(prefix.min(32)));
            ip & mask == base & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(base)) => {
            let ip = u128::from(ip);
            let base = u128::from(base);
            if prefix == 0 {
                return true;
            }
            let mask = u128::MAX << (128 - u32::from(prefix.min(128)));
            ip & mask == base & mask
        }
        // A v4 rule cannot describe a v6 address or the reverse. Note that an
        // IPv4-mapped v6 address is *not* accepted through a v4 rule, because the
        // judgement for such an address already maps it back to v4 and would
        // reach the same conclusion — accepting it here would be a second,
        // divergent path.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> SubscriptionUrl {
        SubscriptionUrl::parse(raw).expect("valid url")
    }

    /// The requirement's named cases: cloud metadata, loopback, and IPv6
    /// loopback must all be refused by the default policy.
    #[test]
    fn the_default_policy_refuses_the_named_ssrf_targets() {
        let policy = SubscriptionFetchPolicy::public_only();

        for target in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:3000/sub",
            "http://[::1]:3000/sub",
            "http://10.0.0.1/sub",
            "http://192.168.1.1/sub",
            "http://172.16.0.1/sub",
            "http://localhost/sub",
            "http://metadata.google.internal/computeMetadata/v1/",
        ] {
            let err = policy
                .check(&url(target))
                .expect_err(&format!("{target} must be refused"));
            assert!(
                err.to_string().contains("allow-list"),
                "the error must explain how to permit it: {err}"
            );
        }
    }

    #[test]
    fn the_default_policy_allows_public_destinations() {
        let policy = SubscriptionFetchPolicy::public_only();
        for target in [
            "https://example.com/sub",
            "https://sub.example.com/sub?token=x",
            "https://8.8.8.8/sub",
            "http://1.1.1.1/sub",
        ] {
            assert!(policy.check(&url(target)).is_ok(), "{target} should pass");
        }
    }

    /// An IPv4-mapped IPv6 loopback must not slip through as public. The
    /// underlying judgement maps it back to IPv4, and this asserts the policy
    /// inherits that rather than reimplementing it.
    #[test]
    fn an_ipv4_mapped_loopback_is_still_refused() {
        let policy = SubscriptionFetchPolicy::public_only();
        assert!(
            policy.check(&url("http://[::ffff:127.0.0.1]/sub")).is_err(),
            "::ffff:127.0.0.1 must not be treated as public"
        );
    }

    /// The legitimate case the policy exists to permit: a self-hosted converter
    /// on a private network.
    #[test]
    fn an_explicit_cidr_permits_that_network() {
        let policy = SubscriptionFetchPolicy::parse(["10.0.0.0/8"]).expect("valid");

        assert!(policy.check(&url("http://10.1.2.3/sub")).is_ok());
        assert!(!policy.is_empty());

        // And nothing outside it gains permission.
        assert!(policy.check(&url("http://192.168.1.1/sub")).is_err());
        assert!(policy.check(&url("http://169.254.169.254/")).is_err());
    }

    #[test]
    fn an_explicit_host_permits_that_host() {
        let policy = SubscriptionFetchPolicy::parse(["sub.internal.example"]).expect("valid");

        assert!(
            policy
                .check(&url("http://sub.internal.example/sub"))
                .is_ok()
        );
        // Case-insensitively, since host names are.
        assert!(
            policy
                .check(&url("http://SUB.INTERNAL.EXAMPLE/sub"))
                .is_ok()
        );

        // The allow-list is exact, not suffix-based: a sibling host is not
        // implied. It happens to pass here for a different reason — `.example`
        // is not a local suffix, so the URL judgement already calls it public —
        // which is why the assertion pins a host that is *not* public by default.
        let local = SubscriptionFetchPolicy::parse(["one.internal"]).expect("valid");
        assert!(local.check(&url("http://one.internal/sub")).is_ok());
        assert!(
            local.check(&url("http://two.internal/sub")).is_err(),
            "a sibling host must not inherit permission"
        );
    }

    /// A single address without a prefix is one host, not a malformed entry.
    #[test]
    fn a_bare_address_is_treated_as_a_single_host() {
        let policy = SubscriptionFetchPolicy::parse(["127.0.0.1"]).expect("valid");
        assert!(policy.check(&url("http://127.0.0.1:3000/sub")).is_ok());
        assert!(policy.check(&url("http://127.0.0.2/sub")).is_err());
    }

    #[test]
    fn an_ipv6_cidr_works() {
        let policy = SubscriptionFetchPolicy::parse(["fd00::/8"]).expect("valid");
        assert!(policy.check(&url("http://[fd12:3456::1]/sub")).is_ok());
        assert!(policy.check(&url("http://[fe80::1]/sub")).is_err());
    }

    /// A malformed entry must be rejected at construction, not silently dropped:
    /// an operator who wrote it believes the destination is permitted.
    #[test]
    fn a_malformed_entry_is_rejected() {
        assert!(SubscriptionFetchPolicy::parse([""]).is_err());
        assert!(SubscriptionFetchPolicy::parse(["   "]).is_err());
        assert!(SubscriptionFetchPolicy::parse(["10.0.0.0/33"]).is_err());
        assert!(SubscriptionFetchPolicy::parse(["::/129"]).is_err());
        assert!(SubscriptionFetchPolicy::parse(["10.0.0.0/abc"]).is_err());
        assert!(SubscriptionFetchPolicy::parse(["not a host"]).is_err());
        // A valid one alongside an invalid one must still fail the whole set.
        assert!(SubscriptionFetchPolicy::parse(["10.0.0.0/8", "bad/"]).is_err());
    }

    #[test]
    fn a_zero_prefix_permits_everything_in_that_family() {
        let policy = SubscriptionFetchPolicy::parse(["0.0.0.0/0"]).expect("valid");
        assert!(policy.check(&url("http://10.0.0.1/sub")).is_ok());
        assert!(policy.check(&url("http://169.254.169.254/")).is_ok());
        // Still only IPv4: a v6 address is not covered by a v4 rule.
        assert!(policy.check(&url("http://[::1]/sub")).is_err());
    }

    #[test]
    fn an_empty_policy_reports_itself_as_empty() {
        let policy = SubscriptionFetchPolicy::public_only();
        assert!(policy.is_empty());
        assert_eq!(policy.len(), 0);
        assert_eq!(
            SubscriptionFetchPolicy::parse(["10.0.0.0/8"])
                .expect("valid")
                .len(),
            1
        );
    }

    /// The policy must agree with the URL's own judgement for the addresses it
    /// does not explicitly allow — one rule, not two.
    #[test]
    fn the_policy_defers_to_the_url_judgement() {
        let policy = SubscriptionFetchPolicy::public_only();
        for raw in [
            "https://example.com/sub",
            "https://8.8.8.8/sub",
            "http://127.0.0.1/sub",
            "http://10.0.0.1/sub",
            "http://[::1]/sub",
            "http://[fe80::1]/sub",
        ] {
            let parsed = url(raw);
            assert_eq!(
                policy.check(&parsed).is_ok(),
                parsed.is_public_destination(),
                "the policy and the URL must agree about {raw}"
            );
        }
    }
}
