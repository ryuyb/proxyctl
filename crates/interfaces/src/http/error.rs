//! Mapping failures onto HTTP responses.
//!
//! # Status codes follow meaning, not message text
//!
//! A caller has to be able to branch on the status without parsing prose, so each
//! application error maps to the status that describes its *kind*: a state
//! conflict is `409`, a missing object `404`, an unreachable dependency `502`.
//! Matching on substrings would make the mapping change whenever a message did.
//!
//! The machine-readable code in the body comes from
//! `ApplicationError::code`, which is already a stable part of the surface.
//!
//! # Redaction is applied here, once
//!
//! Error text can quote a subscription URL, which carries its own credentials.
//! Rather than trusting every producer upstream to have cleaned its message, the
//! response builder runs the redaction as the last step before serialisation. That
//! way a new error path cannot leak by omission.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use proxy_application::ApplicationError;
use proxy_application::ports::error::PortError;

use super::auth::AuthError;
use crate::dto::ErrorDto;

/// A failure that will be rendered as an HTTP response.
#[derive(Debug)]
pub struct HttpError {
    status: StatusCode,
    code: String,
    message: String,
}

impl HttpError {
    /// Builds an error with an explicit status.
    #[must_use]
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    /// The status this will be rendered with.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// The machine-readable code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// A 400 for input the caller can fix.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
    }
}

/// The status an application error deserves.
#[must_use]
pub fn status_for(error: &ApplicationError) -> StatusCode {
    match error {
        ApplicationError::NotFound(_) => StatusCode::NOT_FOUND,
        // The request was well formed but conflicts with current state — a start
        // while stopping, an activation with no active version. `409` is what a
        // client should retry or re-read for.
        ApplicationError::InvalidState(_) => StatusCode::CONFLICT,
        ApplicationError::ValidationFailed(_) | ApplicationError::Domain(_) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        // A dependency the agent depends on is not answering. `502` keeps this
        // distinct from the agent itself being broken (`500`).
        ApplicationError::ConverterUnavailable => StatusCode::BAD_GATEWAY,
        ApplicationError::Port(port) => status_for_port(port),
        // Activation, reload, and rollback failures mean the agent could not
        // complete an operation it started. The active configuration is intact,
        // which the body states; the status reflects the failed attempt.
        ApplicationError::ConfigActivationFailed(_)
        | ApplicationError::MihomoReloadFailed(_)
        | ApplicationError::RollbackFailed(_) => StatusCode::CONFLICT,
        // Not a fault: the environment cannot do this. `501` says so without
        // implying the agent is broken.
        ApplicationError::CapabilityUnavailable(_) => StatusCode::NOT_IMPLEMENTED,
        // The operation did not take effect but nothing was harmed. `409` rather
        // than an error status, because the outcome is a product state.
        ApplicationError::DegradedPreservingActiveConfig(_) => StatusCode::CONFLICT,
    }
}

/// The status a port failure deserves.
#[must_use]
pub fn status_for_port(error: &PortError) -> StatusCode {
    match error {
        PortError::PermissionDenied(_) => StatusCode::FORBIDDEN,
        PortError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
        PortError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
        PortError::Unreachable(_) | PortError::Transport(_) => StatusCode::BAD_GATEWAY,
        PortError::Converter(_) => StatusCode::BAD_GATEWAY,
        PortError::Storage(_) | PortError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        PortError::InvalidResponse(_) | PortError::UnexpectedStatus { .. } => {
            StatusCode::BAD_GATEWAY
        }
    }
}

impl From<ApplicationError> for HttpError {
    fn from(error: ApplicationError) -> Self {
        // The code comes from the application's own stable identifier, so the wire
        // code and the internal one cannot diverge.
        let code = error.code().to_owned();
        Self {
            status: status_for(&error),
            code,
            message: error.to_string(),
        }
    }
}

impl From<PortError> for HttpError {
    fn from(error: PortError) -> Self {
        // A port failure that reaches a handler directly — a repository read, for
        // instance — is mapped with the same rules as one wrapped in an
        // application error, so the two paths cannot disagree.
        let code = error.code().to_owned();
        Self {
            status: status_for_port(&error),
            code,
            message: error.to_string(),
        }
    }
}

impl From<AuthError> for HttpError {
    fn from(error: AuthError) -> Self {
        let status = match &error {
            // A missing or unrecognised credential is unauthenticated.
            AuthError::NoCredential
            | AuthError::MissingToken
            | AuthError::InvalidToken
            | AuthError::VerificationFailed(_)
            | AuthError::InvalidSession => StatusCode::UNAUTHORIZED,
            // A recognised caller that is not allowed is a different answer, and
            // so is a refusal for crossing origins: the credential may be
            // perfectly valid, which is what makes the distinction worth keeping.
            AuthError::NotPermitted { .. }
            | AuthError::MethodNotAllowed
            | AuthError::CrossOrigin => StatusCode::FORBIDDEN,
        };
        Self {
            status,
            code: match status {
                StatusCode::UNAUTHORIZED => "UNAUTHENTICATED".to_owned(),
                _ => "FORBIDDEN".to_owned(),
            },
            message: error.to_string(),
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let body = ErrorDto {
            code: self.code,
            // Redaction runs as the last step, so no error path can leak a URL by
            // forgetting to clean its own message.
            message: redact(&self.message),
        };
        (self.status, Json(body)).into_response()
    }
}

/// Removes anything credential-shaped from a message.
///
/// Subscription URLs embed their own credentials in the query string or userinfo,
/// and the converter's error bodies quote them.
///
/// # Why this scans for the scheme rather than splitting on whitespace
///
/// A URL in prose is rarely followed by a space: it ends a clause, so it is
/// followed by a comma, a period, or a closing bracket. Splitting on whitespace and
/// testing `starts_with` handles only the case where the URL stands alone, and a
/// real error message hit exactly that gap — `failed: (http://a/x), and ...`
/// carried the URL through untouched.
///
/// So the scan finds each occurrence of a scheme and replaces everything up to the
/// next character that cannot appear in a URL. The set is deliberately generous on
/// what counts as "inside" a URL: over-redacting costs a readable message, while
/// under-redacting leaks a credential.
#[must_use]
pub fn redact(message: &str) -> String {
    const SCHEMES: [&str; 2] = ["http://", "https://"];

    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while let Some((index, scheme)) = find_earliest_scheme(rest, &SCHEMES) {
        out.push_str(&rest[..index]);
        let tail = &rest[index + scheme.len()..];
        out.push_str("<redacted-url>");
        // Consume the URL body: everything up to a delimiter that cannot be part
        // of one in prose. A trailing sentence mark is left in place, so the
        // sentence still reads correctly.
        let end = url_body_end(tail);
        rest = &tail[end..];
    }

    out.push_str(rest);
    out
}

/// How much of `tail` belongs to the URL.
///
/// A dot is ambiguous: it appears inside host names and also ends sentences. It
/// ends the URL only when what follows cannot continue one — whitespace, or the
/// end of the message. Treating every dot as a delimiter would truncate
/// `example.com`; treating none would swallow the sentence's full stop.
fn url_body_end(tail: &str) -> usize {
    let bytes = tail.as_bytes();
    for (index, ch) in tail.char_indices() {
        if ch.is_whitespace() || matches!(ch, ')' | ']' | '}' | '"' | '\'' | ',' | ';') {
            return index;
        }
        if ch == '.' {
            // Look past the dot: a run of dots followed by whitespace or the end
            // is punctuation, anything else is part of the address.
            let rest = &bytes[index..];
            let dots = rest.iter().take_while(|b| **b == b'.').count();
            let after = index + dots;
            let boundary = after >= tail.len()
                || tail[after..]
                    .chars()
                    .next()
                    .is_none_or(|c| c.is_whitespace());
            if boundary {
                // Stop *before* the punctuation: it is part of the sentence, not
                // of the address, and dropping it would corrupt the message.
                return index;
            }
        }
    }
    tail.len()
}

/// Finds the earliest of `schemes` in `text`.
///
/// The returned slice borrows from `text`, not from `schemes`, so only `text`'s
/// lifetime is named.
fn find_earliest_scheme(text: &str, schemes: &[&'static str]) -> Option<(usize, &'static str)> {
    let mut best: Option<(usize, &'static str)> = None;
    for scheme in schemes {
        if let Some(index) = text.find(*scheme)
            && best.is_none_or(|(current, _)| index < current)
        {
            best = Some((index, scheme));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_missing_object_is_a_404() {
        assert_eq!(
            status_for(&ApplicationError::NotFound("cfg-1".into())),
            StatusCode::NOT_FOUND
        );
    }

    /// A state conflict is not a bad request: the caller's request was well formed.
    #[test]
    fn a_state_conflict_is_a_409() {
        assert_eq!(
            status_for(&ApplicationError::InvalidState("no start options".into())),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_for(&ApplicationError::ConfigActivationFailed("bad".into())),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn authorization_and_availability_get_distinct_statuses() {
        assert_eq!(
            status_for_port(&PortError::PermissionDenied("x".into())),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_for_port(&PortError::NotImplemented("x")),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            status_for_port(&PortError::Timeout(Duration::from_secs(1))),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            status_for_port(&PortError::Unreachable(Box::new(std::io::Error::other(
                "x"
            )))),
            StatusCode::BAD_GATEWAY
        );
    }

    /// A storage fault is the agent's own problem, not the caller's and not a
    /// dependency's.
    #[test]
    fn a_storage_fault_is_a_500() {
        assert_eq!(
            status_for_port(&PortError::Storage("disk full".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// An absent capability is not a defect, so it must not read as `500`.
    #[test]
    fn a_missing_capability_is_not_a_server_error() {
        assert_eq!(
            status_for(&ApplicationError::CapabilityUnavailable("tun".into())),
            StatusCode::NOT_IMPLEMENTED
        );
    }

    #[test]
    fn an_auth_failure_says_which_kind() {
        assert_eq!(
            HttpError::from(AuthError::MissingToken).status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            HttpError::from(AuthError::InvalidToken).status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            HttpError::from(AuthError::NotPermitted { uid: 1, gid: 2 }).status(),
            StatusCode::FORBIDDEN
        );
        // The method gate is a 403 too, but for a different reason: the caller is
        // fine, the request is not.
        assert_eq!(
            HttpError::from(AuthError::MethodNotAllowed).status(),
            StatusCode::FORBIDDEN
        );
        // A store that cannot be reached must not read as permission granted.
        assert_eq!(
            HttpError::from(AuthError::VerificationFailed("store down".into())).status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn the_code_comes_from_the_application() {
        let err = HttpError::from(ApplicationError::NotFound("x".into()));
        assert_eq!(err.code(), "NOT_FOUND");
        let err = HttpError::from(ApplicationError::Port(PortError::Timeout(
            Duration::from_secs(1),
        )));
        assert_eq!(err.code(), "PORT_TIMEOUT");
    }

    /// The measured converter error quotes the subscription URL, credentials and
    /// all, so redaction is not hypothetical.
    #[test]
    fn redaction_removes_urls_but_keeps_the_sentence() {
        let message = "converter unreachable: 订阅 x 的远程订阅 https://example.com/sub?token=secret123 发生错误";
        let cleaned = redact(message);
        assert!(!cleaned.contains("secret123"), "{cleaned}");
        assert!(!cleaned.contains("https://"), "{cleaned}");
        assert!(cleaned.contains("<redacted-url>"), "{cleaned}");
        assert!(cleaned.contains("converter unreachable"), "{cleaned}");
    }

    #[test]
    fn redaction_handles_punctuation_and_multiple_urls() {
        let cleaned = redact("failed: (http://a/x), and https://b/y.");
        assert_eq!(cleaned, "failed: (<redacted-url>), and <redacted-url>.");
    }

    #[test]
    fn redaction_leaves_ordinary_text_alone() {
        let message = "invalid state: no start options configured; cannot spawn";
        assert_eq!(redact(message), message);
    }

    /// A message that is only a URL must still produce something readable.
    #[test]
    fn a_bare_url_becomes_the_placeholder() {
        assert_eq!(redact("https://example.com/x?t=1"), "<redacted-url>");
    }

    /// Regression: this is the case the first implementation missed. A URL in
    /// prose is usually followed by punctuation rather than a space, and a
    /// whitespace split left the whole URL — credentials included — intact.
    #[test]
    fn a_url_followed_by_punctuation_is_still_redacted() {
        let cases = [
            ("see http://a/x?token=s1, then stop", "s1"),
            ("see http://a/x?token=s2.", "s2"),
            ("see (http://a/x?token=s3)", "s3"),
            ("see [http://a/x?token=s4]", "s4"),
            ("see \"http://a/x?token=s5\"", "s5"),
            ("see http://a/x?token=s6; done", "s6"),
        ];
        for (message, secret) in cases {
            let cleaned = redact(message);
            assert!(!cleaned.contains(secret), "leaked {secret}: {cleaned}");
            assert!(!cleaned.contains("http://"), "leaked a URL: {cleaned}");
            assert!(cleaned.contains("<redacted-url>"), "{cleaned}");
        }
    }

    /// A dot inside a host name belongs to the URL and must be consumed, while a
    /// dot that ends a sentence must not. Both shapes appear in real messages.
    #[test]
    fn dots_are_split_between_addresses_and_sentences() {
        let cleaned = redact("failed to reach https://sub.example.com/path?t=1.");
        assert_eq!(cleaned, "failed to reach <redacted-url>.");
        assert!(!cleaned.contains("example.com"));
    }
}
