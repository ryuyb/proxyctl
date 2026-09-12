//! Connection operations.
//!
//! A separate port because connection records carry process identity (uid,
//! executable path). Exposure of that data is an access-control decision distinct
//! from lifecycle control, so it must be grantable independently.
//!
//! # What the kernel actually returns
//!
//! Measured against mihomo v1.19.30, a live connection record looks like this:
//!
//! ```text
//! { "id": "1f73...", "upload": 79, "download": 15116,
//!   "start": "2026-09-12T21:40:03.325015696+08:00",
//!   "chains": ["DIRECT"], "providerChains": [""], "rule": "", "rulePayload": "",
//!   "metadata": { "network": "tcp", "type": "HTTP",
//!                 "sourceIP": "127.0.0.1", "sourcePort": "51844",
//!                 "destinationIP": "127.0.0.1", "destinationPort": "18081",
//!                 "host": "", "inboundName": "DEFAULT-MIXED", "inboundPort": "7890",
//!                 "uid": 0, "process": "", "processPath": "", ... } }
//! ```
//!
//! Three details from that record shape the types below:
//!
//! * **`chains` is an array** and `rule` is an **empty string**, not `null`. A
//!   strategy group produces a chain of more than one entry, and taking the first
//!   would lose the answer to "which group did this go through".
//! * **`start` is RFC 3339 with an offset**, so an absolute time is available
//!   here — unlike the log stream, whose structured format reports only `HH:MM:SS`.
//! * **`uid`, `process`, and `processPath` are always present but may be empty**,
//!   which happens when the kernel cannot determine the socket's owner. An empty
//!   string therefore means "not available", not "the empty value", and the two
//!   are normalised to `None` at this boundary so no consumer has to know that.

use async_trait::async_trait;
use proxy_domain::shared::time::Timestamp;

use crate::ports::error::PortError;
use crate::ports::secret_store::Role;

/// One live connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionView {
    /// Opaque connection identifier, as the kernel reports it.
    pub id: String,
    /// Originating address, `host:port`.
    pub source: String,
    /// Destination address. The sniffed host name when there is one, otherwise
    /// `host:port`.
    pub destination: String,
    /// The matched rule's type. `None` when nothing matched, which the kernel
    /// reports as an empty string.
    pub rule: Option<String>,
    /// The matched rule's payload, such as the domain it named.
    pub rule_payload: Option<String>,
    /// The match chain, outermost first, for example `["ProxyGroup", "Node"]`.
    ///
    /// A vector rather than a single value: a group resolves through its members,
    /// and the flattened chain is the only place that relationship survives.
    pub chains: Vec<String>,
    /// UID of the originating process, when the kernel could determine it.
    pub uid: Option<u32>,
    /// Name of the originating process, when readable.
    pub process: Option<String>,
    /// Path of the originating process's executable, when readable.
    ///
    /// The most sensitive field in this record: together with the destination it
    /// identifies which local program contacted what. Never exposed to a
    /// read-only caller — see [`ConnectionView::redact_for`].
    pub process_path: Option<String>,
    /// When the connection was established.
    pub started_at: Option<Timestamp>,
    /// Bytes uploaded on this connection.
    pub upload: u64,
    /// Bytes downloaded on this connection.
    pub download: u64,
    /// The inbound listener that accepted it, such as `DEFAULT-MIXED`.
    pub inbound: Option<String>,
}

impl ConnectionView {
    /// Returns this view with the process-identifying fields removed.
    ///
    /// # Why this is a call rather than an absent field
    ///
    /// Hiding the fields by leaving them unpopulated would work, and would be
    /// invisible: a future consumer reading `view.uid` would have no reason to
    /// suspect that a permission check was supposed to happen first. Making it an
    /// explicit transformation means every path that hands a view to a caller has
    /// to say which role it is for, and a test can assert the transformation
    /// actually clears the fields rather than relying on them never being set.
    ///
    /// Administrative callers get the view unchanged. Everyone else loses the
    /// three process-identifying fields, which together answer "which local
    /// program contacted what" — a question that is not theirs to ask.
    #[must_use]
    pub fn redact_for(&self, role: Role) -> Self {
        if role == Role::Admin {
            return self.clone();
        }
        Self {
            uid: None,
            process: None,
            process_path: None,
            ..self.clone()
        }
    }
}

/// The current connection list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionList {
    /// Total bytes uploaded across all connections.
    pub upload_total: u64,
    /// Total bytes downloaded across all connections.
    pub download_total: u64,
    /// Active connections.
    pub connections: Vec<ConnectionView>,
}

impl ConnectionList {
    /// Returns the list with process identity removed from every connection.
    #[must_use]
    pub fn redact_for(&self, role: Role) -> Self {
        Self {
            upload_total: self.upload_total,
            download_total: self.download_total,
            connections: self
                .connections
                .iter()
                .map(|c| c.redact_for(role))
                .collect(),
        }
    }
}

/// What the kernel did with a close request.
///
/// # Why this is not a boolean
///
/// The kernel answers `204` for a connection it closed **and** for one that never
/// existed — measured, and consistent with its documentation. So "closed" and
/// "was not there" cannot be told apart, and an API that claimed to know would be
/// inventing the distinction. This type names what is actually known: the request
/// was accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The kernel accepted the request. It does not report whether anything was
    /// closed.
    Accepted,
    /// The kernel refused, with the status it returned.
    Rejected {
        /// The status code.
        http_status: u16,
    },
}

/// Inspects and terminates connections.
#[async_trait]
pub trait MihomoConnectionOps: Send + Sync {
    /// List active connections.
    ///
    /// # Errors
    /// Returns [`PortError`] when the kernel cannot be reached or the response is
    /// not the expected shape.
    async fn connections(&self) -> Result<ConnectionList, PortError>;

    /// Ask the kernel to close one connection.
    ///
    /// # Errors
    ///
    /// Returns [`PortError`] only for transport failures. A refusal by the kernel
    /// is [`CloseOutcome::Rejected`], not an error, because it is a business
    /// answer rather than a fault.
    async fn close_connection(&self, id: &str) -> Result<CloseOutcome, PortError>;

    /// Ask the kernel to close every connection, returning how many were active
    /// beforehand.
    ///
    /// The count is measured by listing connections on both sides of the request.
    /// The kernel does not report it, and returning a bare success would leave the
    /// caller unable to tell "closed 200 connections" from "there was nothing to
    /// close" — a difference that matters in an audit record.
    ///
    /// # Errors
    ///
    /// As [`close_connection`](Self::close_connection).
    async fn close_all(&self) -> Result<usize, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> ConnectionView {
        ConnectionView {
            id: "c1".to_owned(),
            source: "127.0.0.1:51844".to_owned(),
            destination: "example.com:443".to_owned(),
            rule: Some("DomainSuffix".to_owned()),
            rule_payload: Some("example.com".to_owned()),
            chains: vec!["Proxy".to_owned(), "Node".to_owned()],
            uid: Some(1000),
            process: Some("curl".to_owned()),
            process_path: Some("/usr/bin/curl".to_owned()),
            started_at: Some(Timestamp::from_unix_seconds(1)),
            upload: 79,
            download: 15116,
            inbound: Some("DEFAULT-MIXED".to_owned()),
        }
    }

    #[test]
    fn an_admin_sees_the_process_identity() {
        let redacted = view().redact_for(Role::Admin);
        assert_eq!(redacted.uid, Some(1000));
        assert_eq!(redacted.process.as_deref(), Some("curl"));
        assert_eq!(redacted.process_path.as_deref(), Some("/usr/bin/curl"));
    }

    /// The fields a read-only caller must not receive, asserted individually so a
    /// failure names which one leaked.
    #[test]
    fn a_read_only_caller_loses_every_process_identifying_field() {
        let redacted = view().redact_for(Role::ReadOnly);
        assert!(redacted.uid.is_none(), "uid leaked");
        assert!(redacted.process.is_none(), "process leaked");
        assert!(redacted.process_path.is_none(), "process_path leaked");
    }

    /// Redaction removes identity and nothing else: the connection remains
    /// observable, which is the whole point of showing a read-only caller a list.
    #[test]
    fn redaction_keeps_everything_that_is_not_process_identity() {
        let original = view();
        let redacted = original.redact_for(Role::ReadOnly);
        assert_eq!(redacted.id, original.id);
        assert_eq!(redacted.source, original.source);
        assert_eq!(redacted.destination, original.destination);
        assert_eq!(redacted.rule, original.rule);
        assert_eq!(redacted.rule_payload, original.rule_payload);
        assert_eq!(redacted.chains, original.chains);
        assert_eq!(redacted.upload, original.upload);
        assert_eq!(redacted.download, original.download);
        assert_eq!(redacted.started_at, original.started_at);
        assert_eq!(redacted.inbound, original.inbound);
    }

    /// The list-level helper must reach every entry, not just the first.
    #[test]
    fn the_list_redaction_reaches_every_connection() {
        let list = ConnectionList {
            upload_total: 100,
            download_total: 200,
            connections: vec![view(), view()],
        };
        let redacted = list.redact_for(Role::ReadOnly);
        assert_eq!(redacted.connections.len(), 2);
        for connection in &redacted.connections {
            assert!(connection.uid.is_none(), "{connection:?}");
            assert!(connection.process_path.is_none(), "{connection:?}");
        }
        // The totals are not identity and must survive.
        assert_eq!(redacted.upload_total, 100);
        assert_eq!(redacted.download_total, 200);
    }

    /// An admin's list is unchanged, so the redaction cannot accidentally strip a
    /// field the administrative view is supposed to have.
    #[test]
    fn an_administrative_list_is_unchanged() {
        let list = ConnectionList {
            upload_total: 1,
            download_total: 2,
            connections: vec![view()],
        };
        assert_eq!(list.redact_for(Role::Admin), list);
    }

    #[test]
    fn connection_view_tolerates_missing_process_identity() {
        let mut view = view();
        view.uid = None;
        view.process = None;
        view.process_path = None;
        assert!(
            view.uid.is_none(),
            "unreadable identity must be representable"
        );
        // And a second redaction is a no-op rather than an error.
        assert_eq!(view.redact_for(Role::ReadOnly), view);
    }

    /// An unmatched connection still has to be representable: the kernel reports
    /// an empty rule, which the adapter normalises to `None`.
    #[test]
    fn an_unmatched_connection_has_no_rule() {
        let mut view = view();
        view.rule = None;
        view.rule_payload = None;
        assert!(view.rule.is_none());
        assert_eq!(view.redact_for(Role::ReadOnly).rule, None);
    }

    #[test]
    fn connection_list_carries_totals() {
        let list = ConnectionList {
            upload_total: 1024,
            download_total: 2048,
            connections: Vec::new(),
        };
        assert_eq!(list.upload_total, 1024);
        assert!(list.connections.is_empty());
    }

    /// The whole reason `CloseOutcome` exists: the kernel cannot report whether
    /// anything was closed, and the type must not imply otherwise.
    #[test]
    fn a_close_reports_acceptance_not_a_count() {
        assert_eq!(CloseOutcome::Accepted, CloseOutcome::Accepted);
        assert_ne!(
            CloseOutcome::Accepted,
            CloseOutcome::Rejected { http_status: 404 }
        );
    }
}
