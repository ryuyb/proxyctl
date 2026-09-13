//! Checks on a configuration's *values* that the kernel accepts but ignores.
//!
//! # Why this exists
//!
//! `mihomo -t` validates that a configuration is well-formed, not that it does
//! what its author meant. Two classes of mistake pass it:
//!
//! * a **misspelled key**, which the kernel ignores and replaces with a default —
//!   handled by the field whitelist in [`super::whitelist`];
//! * a **well-formed value on the wrong key**, which the kernel may accept and
//!   then fail to use.
//!
//! This module covers the second. It is a different question from the whitelist —
//! the key is spelled correctly and is in the list — so it is a separate check.
//!
//! # The case that prompted it
//!
//! `external-controller: /run/mihomo.sock` looks correct and is not. The kernel
//! parses that field as a `host:port` pair, so an absolute path produces
//! `listen tcp: address ...: missing port in address`.
//!
//! What makes it worth a dedicated check is the failure mode: **the kernel logs one
//! error and keeps running.** The proxy port opens, the process stays up,
//! `proxyctl status` reports `Running`, and the only thing missing is the control
//! API — which is every management feature the agent has. An operator sees a
//! healthy kernel and a dashboard that cannot connect, and nothing in either place
//! says why.
//!
//! The correct field is `external-controller-unix`, which the agent's own
//! generator already writes (verified: `proxy_domain::configuration::generation`).
//! This check exists for configurations the agent did not generate — an operator's
//! hand-written file, or one migrated from another tool.

use proxy_domain::configuration::ConfigBody;

/// The configuration field that takes a socket path.
const SOCKET_FIELD: &str = "external-controller-unix";

/// The configuration field that takes a `host:port`.
const ADDRESS_FIELD: &str = "external-controller";

/// A value that looks like a filesystem path rather than an address.
///
/// An absolute path is the shape an operator writes when they mean a socket. The
/// kernel's own parser treats anything without a port as malformed, so the test is
/// deliberately narrow: only a leading `/` is reported. A value like
/// `mihomo.sock` would also be wrong, but it is indistinguishable from a hostname
/// and reporting it would mean guessing.
#[must_use]
pub fn is_socket_path(value: &str) -> bool {
    value.trim().starts_with('/')
}

/// Misconfigurations found by inspecting values.
///
/// Each carries the field, what was written, and what to write instead, so a
/// caller can render it without knowing anything about mihomo's configuration
/// format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mistake {
    /// The field that was written.
    pub field: &'static str,
    /// The value that was written.
    pub value: String,
    /// The field that should have been used.
    pub expected_field: &'static str,
    /// A one-line explanation, safe to show an operator.
    pub explanation: String,
}

/// Inspects a configuration's values for mistakes the kernel accepts.
///
/// Returns every mistake found rather than the first: an operator fixing a file
/// should see all of them at once.
///
/// A document that does not parse yields nothing here. Syntax is the kernel's L1
/// check, and reporting a value problem from a document whose structure is already
/// in doubt would produce noise rather than a finding.
#[must_use]
pub fn inspect(body: &ConfigBody) -> Vec<Mistake> {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(body.as_str()) else {
        return Vec::new();
    };
    let Some(mapping) = value.as_mapping() else {
        return Vec::new();
    };

    let mut mistakes = Vec::new();

    // `external-controller` with a path. The one that prompted this module.
    if let Some(written) = mapping
        .get(serde_yaml::Value::String(ADDRESS_FIELD.to_owned()))
        .and_then(|value| value.as_str())
    {
        if is_socket_path(written) {
            mistakes.push(Mistake {
                field: ADDRESS_FIELD,
                value: written.to_owned(),
                expected_field: SOCKET_FIELD,
                explanation: format!(
                    "`{ADDRESS_FIELD}` expects a `host:port` address, so a path is parsed as \
                     one and fails with \"missing port in address\". The kernel logs that \
                     error and keeps running, so the proxy works while the control API — and \
                     everything that needs it — does not. Use `{SOCKET_FIELD}` for a socket \
                     path, or write an address such as `127.0.0.1:9090`."
                ),
            });
        }
    }

    mistakes
}

/// Renders a mistake as the message a report shows.
#[must_use]
pub fn describe(mistake: &Mistake) -> String {
    format!(
        "{}: {}={:?} — {}",
        "misconfigured_controller", mistake.field, mistake.value, mistake.explanation
    )
}

/// The stable code for a controller-value mistake.
///
/// Stable because a client keys on it, and because the message is prose that may
/// be reworded.
pub const CODE: &str = "misconfigured_controller";

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a body, or panics. `ConfigBody::new` is fallible because a body is
    /// validated on construction; every literal here is non-empty and small, so a
    /// failure would be a mistake in the test rather than in the code under test.
    fn config(body: &str) -> ConfigBody {
        ConfigBody::new(body).expect("the test body should be accepted")
    }

    /// The case that prompted this module.
    #[test]
    fn a_socket_path_on_the_address_field_is_reported() {
        let mistakes = inspect(&config(
            "mixed-port: 7890\nexternal-controller: /run/mihomo.sock\n",
        ));
        assert_eq!(mistakes.len(), 1, "{mistakes:?}");
        assert_eq!(mistakes[0].field, "external-controller");
        assert_eq!(mistakes[0].value, "/run/mihomo.sock");
        assert_eq!(mistakes[0].expected_field, "external-controller-unix");
        // The message has to name the symptom, because the symptom is what the
        // operator will have noticed.
        assert!(
            mistakes[0].explanation.contains("control API"),
            "the message must say what breaks: {}",
            mistakes[0].explanation
        );
    }

    /// A correct address on that field is not a mistake. Reporting it would make
    /// the check noise, and an operator would learn to ignore it.
    #[test]
    fn a_real_address_is_accepted() {
        for address in ["127.0.0.1:9090", "0.0.0.0:9090", "localhost:9090"] {
            let mistakes = inspect(&config(&format!(
                "mixed-port: 7890\nexternal-controller: {address}\n"
            )));
            assert!(mistakes.is_empty(), "{address}: {mistakes:?}");
        }
    }

    /// The correct field is not reported, obviously — but asserted, because a
    /// check that flagged its own recommendation would be worse than none.
    #[test]
    fn the_correct_field_is_accepted() {
        let mistakes = inspect(&config(
            "mixed-port: 7890\nexternal-controller-unix: /run/mihomo.sock\n",
        ));
        assert!(mistakes.is_empty(), "{mistakes:?}");
    }

    /// A document with neither field is the common case and must stay silent.
    #[test]
    fn a_document_without_a_controller_is_accepted() {
        assert!(inspect(&config("mixed-port: 7890\nmode: rule\n")).is_empty());
    }

    /// A document that does not parse yields nothing: syntax is the kernel's own
    /// check, and a value finding from an unparseable document would be noise.
    ///
    /// An empty body is not among the cases: `ConfigBody::new` refuses one, so it
    /// cannot reach this function at all. Asserted separately below, because the
    /// place the refusal happens is the point.
    #[test]
    fn an_unparseable_document_yields_nothing() {
        assert!(inspect(&config("mixed-port: [unclosed\n")).is_empty());
        assert!(inspect(&config("mixed-port: 7890\n  bad indent: 1\n")).is_empty());
    }

    /// An empty body is refused at construction, which is upstream of this check
    /// and the right place for it — there is nothing to inspect.
    #[test]
    fn an_empty_body_never_reaches_the_inspection() {
        assert!(ConfigBody::new("").is_err());
    }

    /// A document that is not a mapping yields nothing rather than panicking.
    #[test]
    fn a_non_mapping_document_yields_nothing() {
        assert!(inspect(&config("- a\n- b\n")).is_empty());
        assert!(inspect(&config("just a string\n")).is_empty());
    }

    /// A non-string value on the field is not a path. YAML's implicit typing means
    /// this is reachable without anyone intending it.
    #[test]
    fn a_non_string_value_is_not_reported_as_a_path() {
        let mistakes = inspect(&config("external-controller: 9090\n"));
        assert!(mistakes.is_empty(), "{mistakes:?}");
    }

    /// The path test is about shape, and must not fire on an address that merely
    /// contains a slash.
    #[test]
    fn only_a_leading_slash_is_a_path() {
        assert!(is_socket_path("/run/mihomo.sock"));
        assert!(is_socket_path("  /run/mihomo.sock  "), "surrounding space");

        assert!(!is_socket_path("127.0.0.1:9090"));
        assert!(!is_socket_path("localhost:9090"));
        assert!(!is_socket_path("mihomo.sock"));
        assert!(!is_socket_path(""));
    }

    /// The rendered message names the field, the value, and the fix.
    #[test]
    fn the_message_names_the_field_the_value_and_the_fix() {
        let mistakes = inspect(&config("external-controller: /run/mihomo.sock\n"));
        let text = describe(&mistakes[0]);
        assert!(text.starts_with(CODE), "{text}");
        assert!(text.contains("external-controller"), "{text}");
        assert!(text.contains("/run/mihomo.sock"), "{text}");
        assert!(text.contains("external-controller-unix"), "{text}");
    }
}
