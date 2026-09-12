//! Removing credentials from text that is about to be logged or displayed.
//!
//! # Why this is not the same function as the others
//!
//! The codebase already redacts in two places, and both ask a different question:
//!
//! * `interfaces::http::error::redact` replaces a whole URL because the reader
//!   only needs to know *that* a URL was involved.
//! * `infrastructure::subscription::substore::redact` truncates a URL for the
//!   same reason.
//!
//! A log line is different. An operator diagnosing a failed fetch needs to see
//! **which host and path** were attempted — that is the diagnosis — while the
//! credential in the query string must not survive. Replacing the whole URL would
//! destroy the useful part; leaving it alone would leak the secret. So this
//! function redacts *within* a URL, keeping its shape.
//!
//! Three callers with three questions is not duplication, it is three different
//! functions. What would be a mistake is one function answering all three badly.
//!
//! # What counts as a credential here
//!
//! * Query parameters whose *name* looks like a secret: `token`, `key`,
//!   `password`, `secret`, `auth`, and the same words with suffixes. Matched on a
//!   word boundary so `monkey=` is not treated as a token.
//! * Userinfo: the `user:pass@` part of a URL.
//! * Values the caller names explicitly, for a secret that has no structure — the
//!   controller secret, for instance, which appears bare in some messages.

/// The replacement written in place of a credential value.
pub const PLACEHOLDER: &str = "<redacted>";

/// Parameter names that indicate the value is a credential.
///
/// Matched as the whole parameter name, or as a `-`/`_`-separated suffix, so
/// `subscription-token` and `api_key` match while `monkey` does not.
const SECRET_PARAMETERS: &[&str] = &["token", "key", "password", "passwd", "secret", "auth"];

/// Redacts credentials from a log message.
///
/// `known_secrets` are exact values to remove wherever they appear, for secrets
/// that carry no structure. Empty values in that list are ignored: replacing the
/// empty string would corrupt the text.
#[must_use]
pub fn redact_log_line(message: &str, known_secrets: &[String]) -> String {
    let mut out = redact_known(message, known_secrets);
    out = redact_userinfo(&out);
    redact_query_values(&out)
}

/// A redactor holding the secrets that must be removed by exact value.
///
/// Held rather than passed per call so the adapter builds it once: scanning a
/// message against a set is the per-line cost, and rebuilding the set per line
/// would repeat work for every log entry.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    /// Builds a redactor that removes `secrets` wherever they appear.
    ///
    /// Blank values are dropped rather than kept: a blank entry would match at
    /// every position and replace the entire message with the placeholder.
    #[must_use]
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            secrets: secrets
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect(),
        }
    }

    /// Whether any exact-value secret is configured.
    #[must_use]
    pub fn has_secrets(&self) -> bool {
        !self.secrets.is_empty()
    }

    /// Redacts a message.
    ///
    /// # Why the short values are checked first
    ///
    /// Replacing a shorter secret before a longer one that contains it would leave
    /// a fragment of the longer one behind. Sorting by descending length makes the
    /// replacement order-independent, which matters because the caller has no way
    /// to know the relationship between two secrets.
    #[must_use]
    pub fn redact(&self, message: &str) -> String {
        let mut ordered: Vec<&String> = self.secrets.iter().collect();
        ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
        redact_log_line(message, &ordered.into_iter().cloned().collect::<Vec<_>>())
    }
}

/// Replaces each configured secret wherever it appears.
fn redact_known(message: &str, secrets: &[String]) -> String {
    let mut out = message.to_owned();
    for secret in secrets {
        if secret.trim().is_empty() {
            continue;
        }
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), PLACEHOLDER);
        }
    }
    out
}

/// Removes the `user:pass@` portion of every URL in the text.
///
/// # Why the authority starts *after* the `//`
///
/// The authority is the part after `scheme://` and before the first `/`, `?`, or
/// `#`. Searching for that delimiter from the start of the URL finds the `//`
/// itself and yields an empty authority, so the search must begin past it. An
/// earlier version did not, and silently redacted nothing at all — the failure was
/// caught by the test that asserts the password is gone.
fn redact_userinfo(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while let Some(index) = find_scheme(rest) {
        let (before, tail) = rest.split_at(index);
        out.push_str(before);

        let Some(scheme_end) = tail.find("//").map(|i| i + 2) else {
            // A malformed scheme with no `//` cannot carry userinfo.
            out.push_str(tail);
            return out;
        };
        let after_scheme = &tail[scheme_end..];
        let authority_end = after_scheme
            .find(['/', '?', '#', ' ', '"', '\'', ')', ']', ','])
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];

        out.push_str(&tail[..scheme_end]);
        match authority.rsplit_once('@') {
            Some((_userinfo, host)) => {
                out.push_str(PLACEHOLDER);
                out.push('@');
                out.push_str(host);
            }
            None => out.push_str(authority),
        }
        rest = &after_scheme[authority_end..];
    }

    out.push_str(rest);
    out
}

/// Finds the start of the earliest URL scheme.
fn find_scheme(text: &str) -> Option<usize> {
    ["http://", "https://"]
        .iter()
        .filter_map(|scheme| text.find(scheme))
        .min()
}

/// Replaces the values of secret-looking query parameters.
///
/// Operates on the whole message rather than only inside URLs, because a message
/// can quote a bare `?token=...` fragment after other text has been trimmed.
fn redact_query_values(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    // Split on the separators that can begin a parameter list. The separators are
    // retained so the message keeps reading as prose.
    for (index, piece) in split_keeping(message, &['?', '&']).iter().enumerate() {
        if index == 0 {
            // Nothing before the first separator can be a parameter.
            out.push_str(piece);
            continue;
        }
        let (separator, body) = piece.split_at(piece.chars().next().map_or(0, char::len_utf8));
        match body.split_once('=') {
            Some((name, _value)) if is_secret_parameter(name) => {
                out.push_str(separator);
                out.push_str(name);
                out.push('=');
                out.push_str(PLACEHOLDER);
            }
            _ => out.push_str(piece),
        }
    }
    out
}

/// Splits `text` at each character in `separators`, keeping the separator at the
/// start of the following piece.
///
/// A separator terminates the piece before it, and a space or quote terminates the
/// parameter list entirely: `?a=1 ?b=2` is not one query string, and treating the
/// second `?` as another parameter would mangle prose that merely mentions two
/// URLs.
fn split_keeping(text: &str, separators: &[char]) -> Vec<String> {
    const ENDS: [char; 6] = [' ', '\t', '"', '\'', ')', ']'];
    let mut pieces = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ENDS.contains(&ch) && !current.is_empty() {
            pieces.push(std::mem::take(&mut current));
            current.push(ch);
            continue;
        }
        if separators.contains(&ch) && !current.is_empty() {
            pieces.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

/// Whether a parameter name denotes a credential.
///
/// Matched as the whole name or as a separator-delimited suffix, so `token`,
/// `api-key`, and `subscription_token` match while `monkey` does not.
fn is_secret_parameter(name: &str) -> bool {
    let lowered = name.trim().to_ascii_lowercase();
    let base = lowered
        .rsplit_once(['-', '_'])
        .map_or(lowered.as_str(), |(_, suffix)| suffix);
    // A name that merely *contains* the word, such as `tokenvalue` or `monkey`, is
    // not matched: requiring the whole name or a separator-delimited suffix is what
    // keeps ordinary parameters visible.
    SECRET_PARAMETERS.contains(&base)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case the whole module exists for: the host and path survive, the
    /// credential does not.
    #[test]
    fn a_token_query_parameter_is_masked_and_the_url_stays_readable() {
        let redacted = redact_log_line(
            "fetching https://subs.example.com/api?token=SUPERSECRET&id=7 failed",
            &[],
        );
        assert!(!redacted.contains("SUPERSECRET"), "{redacted}");
        assert!(redacted.contains("subs.example.com"), "{redacted}");
        assert!(redacted.contains("/api"), "{redacted}");
        assert!(redacted.contains("id=7"), "{redacted}");
        assert!(redacted.contains(PLACEHOLDER), "{redacted}");
    }

    #[test]
    fn several_secret_parameters_are_all_masked() {
        let redacted = redact_log_line("https://h/p?token=a&key=b&password=c&secret=d&auth=e", &[]);
        for leaked in ["=a", "=b", "=c", "=d", "=e"] {
            assert!(!redacted.contains(leaked), "{leaked} leaked: {redacted}");
        }
        assert_eq!(redacted.matches(PLACEHOLDER).count(), 5, "{redacted}");
    }

    /// A name that merely contains a secret word must not be masked: over-masking
    /// hides the diagnosis, which is the point of keeping the URL.
    #[test]
    fn a_parameter_that_merely_contains_a_secret_word_is_left_alone() {
        for name in ["monkey", "tokenvalue", "author", "keynote"] {
            let text = format!("https://h/p?{name}=visible");
            let redacted = redact_log_line(&text, &[]);
            assert!(
                redacted.contains("visible"),
                "{name} was over-masked: {redacted}"
            );
        }
    }

    /// A suffix after a separator is a real credential name.
    #[test]
    fn a_separator_delimited_suffix_is_matched() {
        for name in ["subscription-token", "api_key", "user_password", "x-secret"] {
            let text = format!("https://h/p?{name}=hidden");
            let redacted = redact_log_line(&text, &[]);
            assert!(
                !redacted.contains("hidden"),
                "{name} was not matched: {redacted}"
            );
        }
    }

    /// Userinfo is the other common shape, and it appears in node definitions.
    #[test]
    fn url_userinfo_is_masked() {
        let redacted = redact_log_line("proxy at http://alice:hunter2@10.0.0.1:8080/x", &[]);
        assert!(!redacted.contains("hunter2"), "{redacted}");
        assert!(!redacted.contains("alice"), "{redacted}");
        assert!(redacted.contains("10.0.0.1:8080"), "{redacted}");
    }

    #[test]
    fn a_url_without_userinfo_is_unchanged_apart_from_parameters() {
        let redacted = redact_log_line("see https://example.com/status for details", &[]);
        assert_eq!(redacted, "see https://example.com/status for details");
    }

    /// A bare secret with no structure is removed by exact value.
    #[test]
    fn a_known_secret_is_removed_wherever_it_appears() {
        let redacted = redact_log_line(
            "auth failed with secret=abc123 for user",
            &["abc123".to_owned()],
        );
        assert!(!redacted.contains("abc123"), "{redacted}");
        assert!(redacted.contains("auth failed"), "{redacted}");
    }

    /// A blank secret must not be treated as a match, or the whole message would
    /// become the placeholder.
    #[test]
    fn a_blank_known_secret_is_ignored() {
        let redacted = redact_log_line(
            "a perfectly ordinary line",
            &[String::new(), "   ".to_owned()],
        );
        assert_eq!(redacted, "a perfectly ordinary line");
    }

    /// The redactor drops blanks at construction, so the check is not repeated per
    /// line, and it reports honestly whether it holds anything.
    #[test]
    fn the_redactor_drops_blank_secrets_and_says_so() {
        let empty = Redactor::new(vec![String::new(), "  ".to_owned()]);
        assert!(!empty.has_secrets());
        assert_eq!(empty.redact("nothing to do"), "nothing to do");

        let one = Redactor::new(vec!["s3cr3t".to_owned()]);
        assert!(one.has_secrets());
        assert!(!one.redact("value is s3cr3t here").contains("s3cr3t"));
    }

    /// Overlapping secrets must be replaced without leaving a fragment.
    #[test]
    fn a_longer_secret_containing_a_shorter_one_leaves_no_fragment() {
        let redactor = Redactor::new(vec!["abc".to_owned(), "abcdef".to_owned()]);
        let redacted = redactor.redact("token=abcdef");
        assert!(!redacted.contains("def"), "a fragment survived: {redacted}");
    }

    /// Ordinary log lines must pass through untouched. Over-redaction is a real
    /// cost: it hides the diagnosis the operator needs.
    #[test]
    fn an_ordinary_line_is_unchanged() {
        for line in [
            "[Rule] use default rules",
            "Mixed(http+socks) proxy listening at: 127.0.0.1:7890",
            "Initial configuration complete, total time: 0ms",
            "Start initial compatible provider default",
        ] {
            assert_eq!(redact_log_line(line, &[]), line);
        }
    }

    /// A message with several URLs must have each one handled.
    #[test]
    fn multiple_urls_are_all_handled() {
        let redacted = redact_log_line(
            "tried https://a/x?token=one then https://u:p@b/y?key=two",
            &[],
        );
        for leaked in ["one", "two", "u:p"] {
            assert!(!redacted.contains(leaked), "{leaked} survived: {redacted}");
        }
        assert!(redacted.contains("https://a/x"), "{redacted}");
        assert!(redacted.contains("b/y"), "{redacted}");
    }
}
