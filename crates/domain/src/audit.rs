//! Audit records.
//!
//! Audit entries describe privileged actions. Two rules shape the types: they
//! are append-only (no update or delete operation exists), and they must never
//! carry secrets — which is why there is no free-form "details" field that could
//! be filled with a raw configuration or a subscription URL.

use crate::configuration::ConfigVersionId;
use crate::shared::error::DomainError;
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

    /// Rebuilds an action from its [`as_str`](Self::as_str) label.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Invariant`] for an unrecognized label. A record
    /// whose action cannot be identified must fail loudly: silently mapping it
    /// to some default would fabricate an audit trail, which is worse than
    /// reporting that the trail is unreadable.
    pub fn from_label(label: &str) -> Result<Self, DomainError> {
        let action = match label.trim() {
            "mihomo.start" => Self::MihomoStart,
            "mihomo.stop" => Self::MihomoStop,
            "mihomo.restart" => Self::MihomoRestart,
            "mihomo.reload" => Self::MihomoReload,
            "kernel.update" => Self::KernelUpdate,
            "config.activate" => Self::ConfigActivate,
            "config.rollback" => Self::ConfigRollback,
            "subscription.update" => Self::SubscriptionUpdate,
            "system.firewall.apply" => Self::SystemFirewallApply,
            _ => {
                return Err(DomainError::invariant(format!(
                    "unknown audit action label: {label}"
                )));
            }
        };
        Ok(action)
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

    /// A stable discriminator for storage.
    ///
    /// Separate from [`label`](Self::label) because that one is for humans and
    /// is not injective: it renders a user's *name*, which can be changed,
    /// renamed, or collide. Storage keys must not depend on a display format.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::LocalRoot => "local-root",
            Self::LocalUser { .. } => "local-user",
            Self::RemotePrincipal { .. } => "remote-principal",
        }
    }

    /// The uid, for the variants that have one.
    #[must_use]
    pub const fn uid(&self) -> Option<u32> {
        match self {
            Self::LocalUser { uid, .. } => Some(*uid),
            _ => None,
        }
    }

    /// Rebuilds an actor from stored columns.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Invariant`] when the discriminator is unknown, or
    /// when a variant's required field is missing. Both indicate a record this
    /// version cannot faithfully represent, which must not be guessed at.
    pub fn from_parts(
        kind: &str,
        uid: Option<u32>,
        name: Option<String>,
        id: Option<String>,
    ) -> Result<Self, DomainError> {
        match kind.trim() {
            "local-root" => Ok(Self::LocalRoot),
            "local-user" => {
                let uid =
                    uid.ok_or_else(|| DomainError::invariant("a local-user actor requires a uid"))?;
                Ok(Self::LocalUser { uid, name })
            }
            "remote-principal" => {
                let id = id.ok_or_else(|| {
                    DomainError::invariant("a remote-principal actor requires an id")
                })?;
                Ok(Self::RemotePrincipal { id })
            }
            other => Err(DomainError::invariant(format!(
                "unknown audit actor kind: {other}"
            ))),
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

    /// A stable discriminator for storage.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Instance(_) => "instance",
            Self::Config(_) => "config",
            Self::Subscription(_) => "subscription",
            Self::KernelVersion(_) => "kernel-version",
            Self::HostFirewall => "host-firewall",
        }
    }

    /// The referenced identifier, for the variants that carry one.
    #[must_use]
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Instance(id) => Some(id.as_str()),
            Self::Config(id) => Some(id.as_str()),
            Self::Subscription(id) => Some(id.as_str()),
            Self::KernelVersion(v) => Some(v.as_str()),
            Self::HostFirewall => None,
        }
    }

    /// Rebuilds a target from stored columns.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Invariant`] when the discriminator is unknown or a
    /// required value is missing, so an unreadable record surfaces instead of
    /// being silently attributed to the wrong subject.
    pub fn from_parts(kind: &str, value: Option<&str>) -> Result<Self, DomainError> {
        let need = |what: &str| {
            value
                .filter(|v| !v.is_empty())
                .ok_or_else(|| DomainError::invariant(format!("a {what} target requires a value")))
        };
        match kind.trim() {
            "instance" => Ok(Self::Instance(MihomoInstanceId::parse(need("instance")?)?)),
            "config" => Ok(Self::Config(ConfigVersionId::parse(need("config")?)?)),
            "subscription" => Ok(Self::Subscription(SubscriptionId::parse(need(
                "subscription",
            )?)?)),
            "kernel-version" => Ok(Self::KernelVersion(need("kernel-version")?.to_owned())),
            "host-firewall" => Ok(Self::HostFirewall),
            other => Err(DomainError::invariant(format!(
                "unknown audit target kind: {other}"
            ))),
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

    /// A stable discriminator for storage.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure { .. } => "failure",
        }
    }

    /// The recorded reason, for a failure.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Success => None,
            Self::Failure { reason } => Some(reason.as_str()),
        }
    }

    /// Rebuilds a result from stored columns.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Invariant`] for an unknown discriminator, or for a
    /// failure that carries no reason: an audit record saying an action failed
    /// without saying why is not worth reconstructing as if it were complete.
    pub fn from_parts(kind: &str, reason: Option<String>) -> Result<Self, DomainError> {
        match kind.trim() {
            "success" => Ok(Self::Success),
            "failure" => {
                let reason = reason
                    .filter(|r| !r.is_empty())
                    .ok_or_else(|| DomainError::invariant("a failure result requires a reason"))?;
                Ok(Self::Failure { reason })
            }
            other => Err(DomainError::invariant(format!(
                "unknown audit result kind: {other}"
            ))),
        }
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

    /// Storage writes a label and reads it back; every action must survive.
    #[test]
    fn action_labels_round_trip() {
        for action in [
            AuditAction::MihomoStart,
            AuditAction::MihomoStop,
            AuditAction::MihomoRestart,
            AuditAction::MihomoReload,
            AuditAction::KernelUpdate,
            AuditAction::ConfigActivate,
            AuditAction::ConfigRollback,
            AuditAction::SubscriptionUpdate,
            AuditAction::SystemFirewallApply,
        ] {
            let parsed = AuditAction::from_label(action.as_str())
                .unwrap_or_else(|e| panic!("{} must parse: {e}", action.as_str()));
            assert_eq!(parsed, action);
        }
    }

    /// An audit trail that cannot be read must say so rather than invent an
    /// action.
    #[test]
    fn unknown_audit_labels_are_rejected() {
        assert!(AuditAction::from_label("mihomo.explode").is_err());
        assert!(AuditAction::from_label("").is_err());
        assert!(AuditActor::from_parts("nobody", None, None, None).is_err());
        assert!(AuditTarget::from_parts("planet", Some("earth")).is_err());
        assert!(AuditResult::from_parts("maybe", None).is_err());
    }

    /// The display label is not a storage key: a renamed local user must not
    /// change the stored discriminator.
    #[test]
    fn actor_storage_columns_round_trip_independently_of_the_label() {
        let actor = AuditActor::LocalUser {
            uid: 1000,
            name: Some("alice".into()),
        };
        assert_eq!(actor.kind(), "local-user");
        assert_eq!(actor.uid(), Some(1000));
        // The stored kind is stable even though the label embeds the name.
        assert!(actor.label().contains("alice"));

        let restored =
            AuditActor::from_parts(actor.kind(), actor.uid(), Some("alice".into()), None)
                .expect("valid");
        assert_eq!(restored, actor);

        // A renamed user restores just as well, because the name is not the key.
        let renamed = AuditActor::from_parts("local-user", Some(1000), Some("bob".into()), None)
            .expect("valid");
        assert_eq!(renamed.uid(), Some(1000));
        assert!(matches!(
            renamed,
            AuditActor::LocalUser { name: Some(ref n), .. } if n == "bob"
        ));
    }

    #[test]
    fn every_actor_variant_round_trips() {
        let actors = [
            AuditActor::LocalRoot,
            AuditActor::LocalUser { uid: 0, name: None },
            AuditActor::LocalUser {
                uid: 1000,
                name: Some("alice".into()),
            },
            AuditActor::RemotePrincipal {
                id: "prin-1".into(),
            },
        ];
        for actor in actors {
            // Restore from the same columns storage would write.
            let (kind, uid, name, id) = match &actor {
                AuditActor::LocalRoot => ("local-root", None, None, None),
                AuditActor::LocalUser { uid, name } => {
                    ("local-user", Some(*uid), name.clone(), None)
                }
                AuditActor::RemotePrincipal { id } => {
                    ("remote-principal", None, None, Some(id.clone()))
                }
            };
            let restored =
                AuditActor::from_parts(kind, uid, name, id).expect("every variant must restore");
            assert_eq!(restored, actor, "{kind} must round trip");
        }
    }

    #[test]
    fn every_target_variant_round_trips() {
        let targets = [
            AuditTarget::Instance(MihomoInstanceId::parse("default").expect("valid")),
            AuditTarget::Config(ConfigVersionId::parse("v041").expect("valid")),
            AuditTarget::Subscription(SubscriptionId::parse("sub-1").expect("valid")),
            AuditTarget::KernelVersion("v1.19.30".into()),
            AuditTarget::HostFirewall,
        ];
        for target in targets {
            let restored = AuditTarget::from_parts(target.kind(), target.value())
                .expect("every variant must restore");
            assert_eq!(restored, target, "{} must round trip", target.kind());
        }
    }

    /// A failure with no reason is not a faithful record, so it is rejected.
    #[test]
    fn a_failure_without_a_reason_is_rejected_on_restore() {
        assert!(AuditResult::from_parts("failure", None).is_err());
        assert!(AuditResult::from_parts("failure", Some(String::new())).is_err());
        assert_eq!(
            AuditResult::from_parts("success", None).expect("valid"),
            AuditResult::Success
        );
    }

    /// A variant missing its identifying column must not be guessed at.
    #[test]
    fn a_variant_missing_its_column_is_rejected() {
        assert!(AuditActor::from_parts("local-user", None, None, None).is_err());
        assert!(AuditActor::from_parts("remote-principal", None, None, None).is_err());
        assert!(AuditTarget::from_parts("config", None).is_err());
        assert!(AuditTarget::from_parts("instance", Some("")).is_err());
        // The no-value variant still works with no value.
        assert_eq!(
            AuditTarget::from_parts("host-firewall", None).expect("valid"),
            AuditTarget::HostFirewall
        );
    }

    #[test]
    fn result_kind_and_reason_are_exposed_for_storage() {
        assert_eq!(AuditResult::Success.kind(), "success");
        assert!(AuditResult::Success.reason().is_none());

        let failure = AuditResult::Failure {
            reason: "port in use".into(),
        };
        assert_eq!(failure.kind(), "failure");
        assert_eq!(failure.reason(), Some("port in use"));
    }
}
