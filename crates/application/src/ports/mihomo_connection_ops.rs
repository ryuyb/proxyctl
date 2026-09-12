//! Connection operations.
//!
//! A separate port because connection records carry process identity (uid,
//! executable path). Exposure of that data is an access-control decision
//! distinct from lifecycle control, so it must be grantable independently.

use async_trait::async_trait;

use crate::ports::error::PortError;

/// One live connection.
///
/// Process-identifying fields are optional so an adapter running without the
/// privilege to read them can still report the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionView {
    /// Opaque connection identifier.
    pub id: String,
    /// Originating host.
    pub source: String,
    /// Destination host.
    pub destination: String,
    /// Rule or chain that produced the match.
    pub rule: Option<String>,
    /// UID of the originating process, when readable.
    pub uid: Option<u32>,
    /// Name of the originating process, when readable.
    pub process: Option<String>,
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

/// Inspects and terminates connections.
#[async_trait]
pub trait MihomoConnectionOps: Send + Sync {
    /// List active connections.
    async fn connections(&self) -> Result<ConnectionList, PortError>;

    /// Close one connection by identifier.
    ///
    /// # Errors
    /// Returns [`PortError::InvalidResponse`] when the identifier is unknown.
    async fn close_connection(&self, id: &str) -> Result<(), PortError>;

    /// Close every connection.
    async fn close_all(&self) -> Result<(), PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_view_tolerates_missing_process_identity() {
        let view = ConnectionView {
            id: "c1".into(),
            source: "10.0.0.2:5000".into(),
            destination: "1.1.1.1:443".into(),
            rule: Some("MATCH".into()),
            uid: None,
            process: None,
        };
        assert!(
            view.uid.is_none(),
            "unreadable identity must be representable"
        );
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
}
