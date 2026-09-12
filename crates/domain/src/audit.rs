//! Audit records.
//!
//! Audit entries describe privileged actions. Two rules shape the types: they
//! are append-only (no update or delete operation exists), and they must never
//! carry secrets — which is why there is no free-form "details" field that could
//! be filled with a raw configuration or a subscription URL.

use crate::configuration::ConfigVersionId;
use crate::shared::id::{AuditEntryId, MihomoInstanceId, SubscriptionId};
use crate::shared::time::Timestamp;

/// A privileged action worth recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditAction {
    /// Started the kernel.
    MihomoStart,
    /// Stopped the kernel.
    MihomoStop,
    /// Restarted the kernel.
    MihomoRestart,
    /// Reloaded configuration in place.
    MihomoReload,
    /// Updated the kernel binary.
    KernelUpdate,
    /// Activated a configuration version.
    ConfigActivate,
    /// Rolled back to an earlier version.
    ConfigRollback,
    /// Updated a subscription.
    SubscriptionUpdate,
    /// Applied firewall rules.
    SystemFirewallApply,
}

impl AuditAction {
    /// A stable dotted label, suitable for logs and queries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MihomoStart => "mihomo.start",
            Self::MihomoStop => "mihomo.stop",
            Self::MihomoRestart => "mihomo.restart",
            Self::MihomoReload => "mihomo.reload",
            Self::KernelUpdate => "kernel.update",
            Self::ConfigActivate => "config.activate",
            Self::ConfigRollback => "config.rollback",
            Self::SubscriptionUpdate => "subscription.update",
            Self::SystemFirewallApply => "system.firewall.apply",
        }
    }
}

/// Who performed an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditActor {
    /// Root via the local socket or CLI.
    LocalRoot,
    /// A named local user, identified by uid.
    LocalUser {
        /// Numeric user id.
        uid: u32,
        /// User name, if resolvable.
        name: Option<String>,
    },
    /// An authenticated remote principal.
    RemotePrincipal {
        /// Principal identifier.
        id: String,
    },
}

impl AuditActor {
    /// A redacted label for display. Never includes credentials.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::LocalRoot => "local:root".to_owned(),
            Self::LocalUser { uid, name } => match name {
                Some(name) => format!("local:{name}({uid})"),
                None => format!("local:uid({uid})"),
            },
            Self::RemotePrincipal { id } => format!("remote:{id}"),
        }
    }
}

/// The target of an action.
///
/// A closed enum rather than a string, so an audit record can be queried by
/// target type and cannot accidentally store a URL containing credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditTarget {
    /// A Mihomo instance.
    Instance(MihomoInstanceId),
    /// A configuration version.
    Config(ConfigVersionId),
    /// A subscription.
    Subscription(SubscriptionId),
    /// A kernel binary version string.
    KernelVersion(String),
    /// The host firewall.
    HostFirewall,
}

impl AuditTarget {
    /// A stable label.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Instance(id) => format!("instance:{id}"),
            Self::Config(id) => format!("config:{id}"),
            Self::Subscription(id) => format!("subscription:{id}"),
            Self::KernelVersion(v) => format!("kernel:{v}"),
            Self::HostFirewall => "firewall:host".to_owned(),
        }
    }
}

/// Whether an action succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditResult {
    /// The action completed.
    Success,
    /// The action failed, with a redacted reason.
    Failure {
        /// Failure reason. Must not contain secrets.
        reason: String,
    },
}

impl AuditResult {
    /// Whether the action succeeded.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }
}

/// A single append-only audit entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Entry identifier.
    pub id: AuditEntryId,
    /// What was attempted.
    pub action: AuditAction,
    /// Who attempted it.
    pub actor: AuditActor,
    /// What it was attempted on.
    pub target: AuditTarget,
    /// The outcome.
    pub result: AuditResult,
    /// When it happened.
    pub at: Timestamp,
}

impl AuditEntry {
    /// Builds an entry.
    #[must_use]
    pub const fn new(
        id: AuditEntryId,
        action: AuditAction,
        actor: AuditActor,
        target: AuditTarget,
        result: AuditResult,
        at: Timestamp,
    ) -> Self {
        Self {
            id,
            action,
            actor,
            target,
            result,
            at,
        }
    }

    /// A single-line summary suitable for logs.
    #[must_use]
    pub fn summary(&self) -> String {
        let outcome = match &self.result {
            AuditResult::Success => "success".to_owned(),
            AuditResult::Failure { reason } => format!("failure({reason})"),
        };
        format!(
            "action={} actor={} target={} result={}",
            self.action.as_str(),
            self.actor.label(),
            self.target.label(),
            outcome
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

    fn entry(action: AuditAction, result: AuditResult) -> AuditEntry {
        AuditEntry::new(
            AuditEntryId::parse("a1").expect("valid"),
            action,
            AuditActor::LocalRoot,
            AuditTarget::Config(ConfigVersionId::parse("v041").expect("valid")),
            result,
            NOW,
        )
    }

    #[test]
    fn action_labels_are_dotted_and_stable() {
        assert_eq!(AuditAction::ConfigRollback.as_str(), "config.rollback");
        assert_eq!(AuditAction::MihomoStart.as_str(), "mihomo.start");
        assert_eq!(
            AuditAction::SystemFirewallApply.as_str(),
            "system.firewall.apply"
        );
    }

    #[test]
    fn actor_labels_never_include_secrets() {
        assert_eq!(AuditActor::LocalRoot.label(), "local:root");
        assert_eq!(
            AuditActor::LocalUser {
                uid: 1000,
                name: Some("ops".into())
            }
            .label(),
            "local:ops(1000)"
        );
        assert_eq!(
            AuditActor::LocalUser {
                uid: 1000,
                name: None
            }
            .label(),
            "local:uid(1000)"
        );
        assert_eq!(
            AuditActor::RemotePrincipal { id: "admin".into() }.label(),
            "remote:admin"
        );
    }

    #[test]
    fn target_labels_are_typed() {
        let id = ConfigVersionId::parse("v041").expect("valid");
        assert_eq!(AuditTarget::Config(id).label(), "config:v041");
        assert_eq!(AuditTarget::HostFirewall.label(), "firewall:host");
        assert_eq!(
            AuditTarget::KernelVersion("v1.19.30".into()).label(),
            "kernel:v1.19.30"
        );
    }

    #[test]
    fn summary_formats_success() {
        let e = entry(AuditAction::ConfigActivate, AuditResult::Success);
        let s = e.summary();
        assert!(s.contains("action=config.activate"));
        assert!(s.contains("actor=local:root"));
        assert!(s.contains("target=config:v041"));
        assert!(s.contains("result=success"));
    }

    #[test]
    fn summary_formats_failure_with_reason() {
        let e = entry(
            AuditAction::MihomoReload,
            AuditResult::Failure {
                reason: "port in use".into(),
            },
        );
        assert!(e.summary().contains("failure(port in use)"));
        assert!(!e.result.is_success());
    }

    #[test]
    fn result_success_flag_is_correct() {
        assert!(AuditResult::Success.is_success());
        assert!(!AuditResult::Failure { reason: "x".into() }.is_success());
    }
}
