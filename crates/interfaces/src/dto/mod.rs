//! Data transfer objects for the HTTP API.
//!
//! # Why these exist rather than serialising the application's views
//!
//! `AGENTS.md` is explicit that domain entities must not be exposed directly, and
//! the application's `queries` already returns views for that reason. These are a
//! second, thinner layer, and the reason is worth stating: deriving `Serialize` on
//! an application or domain type makes "change the model" and "break the API"
//! the same act. A field rename, a split, or a new variant would silently alter
//! the wire format.
//!
//! So every response shape here is written out and mapped explicitly. The mapping
//! is mechanical and will look redundant; that redundancy is the contract. Adding
//! a field to a domain type does not add it to the API until someone writes the
//! line, which is exactly the review checkpoint this layer provides.

use serde::{Deserialize, Serialize};

use proxy_application::ports::job_registry::{JobRecord, JobState};
use proxy_application::queries::{
    CapabilitiesView, ConfigVersionSummary, MihomoStatusView, SubscriptionSummary,
};
use proxy_domain::audit::AuditEntry;
use proxy_domain::system::capability::CapabilitySet;
use proxy_domain::system::environment::SystemEnvironment;

/// A status string, spelled for the wire.
///
/// The domain's own labels are reused rather than invented, so a client that
/// knows the state machine sees the same words it does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusDto {
    /// Lifecycle state label, e.g. `Running`.
    pub status: String,
    /// Whether a process is expected to be alive.
    pub live: bool,
    /// Whether the instance is currently serving traffic.
    pub serving: bool,
}

/// The kernel's lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MihomoStatusDto {
    /// The instance this describes.
    pub instance: String,
    /// Its display name.
    pub name: String,
    /// Lifecycle summary.
    pub state: StatusDto,
    /// The active configuration version, if any.
    pub active_config: Option<String>,
    /// The running kernel build, if known.
    pub build: Option<BuildDto>,
    /// The most recent health observation, if one was taken.
    pub health: Option<HealthDto>,
    /// The last recorded failure reason, if any.
    pub last_failure: Option<String>,
}

/// The running kernel build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildDto {
    /// Version string.
    pub version: String,
    /// Kernel flavor label.
    pub flavor: String,
}

impl From<MihomoStatusView> for MihomoStatusDto {
    fn from(view: MihomoStatusView) -> Self {
        Self {
            instance: view.instance.as_str().to_owned(),
            name: view.name,
            state: StatusDto {
                status: view.status.as_str().to_owned(),
                live: view.status.is_live(),
                serving: view.status.is_serving(),
            },
            active_config: view.active_config.map(|id| id.as_str().to_owned()),
            build: view.running_build.map(|b| BuildDto {
                version: b.version.as_str().to_owned(),
                flavor: format!("{:?}", b.flavor).to_ascii_lowercase(),
            }),
            health: view.health.map(HealthDto::from),
            last_failure: view.last_failure,
        }
    }
}

/// The lazy health answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthDto {
    /// Whether the kernel process is known to be alive.
    pub process_alive: bool,
    /// Whether the control API answered.
    pub controller_reachable: bool,
    /// Whether the active configuration is the one we asked for.
    pub config_loaded: bool,
    /// Whether the inbound proxy port accepts connections.
    pub proxy_port_listening: bool,
    /// Whether every layer passed.
    pub healthy: bool,
    /// Whether the control plane is up while traffic cannot flow.
    pub degraded: bool,
    /// A one-line summary.
    pub summary: String,
}

impl From<proxy_application::ports::types::HealthReport> for HealthDto {
    fn from(report: proxy_application::ports::types::HealthReport) -> Self {
        Self {
            process_alive: report.process_alive,
            controller_reachable: report.controller_reachable,
            config_loaded: report.config_loaded,
            proxy_port_listening: report.proxy_port_listening,
            healthy: report.is_healthy(),
            degraded: report.is_degraded(),
            summary: report.summary(),
        }
    }
}

/// A configuration version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigVersionDto {
    /// Version identifier.
    pub id: String,
    /// The display label, e.g. `v003`.
    pub label: String,
    /// Where the version came from.
    pub source: String,
    /// Content checksum.
    pub checksum: String,
    /// Whether this version is the active one.
    pub active: bool,
    /// When it was created, in Unix seconds.
    pub created_at: i64,
    /// When it was activated, if ever.
    pub activated_at: Option<i64>,
}

impl From<ConfigVersionSummary> for ConfigVersionDto {
    fn from(view: ConfigVersionSummary) -> Self {
        Self {
            id: view.id.as_str().to_owned(),
            label: view.label,
            source: view.source,
            checksum: view.checksum,
            active: view.is_active,
            created_at: view.created_at.as_unix_seconds(),
            activated_at: view.activated_at.map(|t| t.as_unix_seconds()),
        }
    }
}

/// A subscription.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionDto {
    /// Identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Whether scheduled updates are enabled.
    pub enabled: bool,
    /// The schedule interval in seconds, if any.
    pub interval_seconds: Option<u64>,
    /// Whether an update is due now.
    pub is_due: bool,
    /// The outcome of the last update, as a label.
    pub last_update: Option<String>,
}

impl From<SubscriptionSummary> for SubscriptionDto {
    fn from(view: SubscriptionSummary) -> Self {
        Self {
            id: view.id.as_str().to_owned(),
            name: view.name,
            enabled: view.enabled,
            interval_seconds: view.interval_seconds,
            is_due: view.is_due,
            last_update: view.last_update,
        }
    }
}

/// A job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobDto {
    /// Identifier, used to poll for progress.
    pub id: String,
    /// Kind of work.
    pub kind: String,
    /// What it operates on.
    pub target: String,
    /// Current state label.
    pub state: String,
    /// The current step, while running.
    pub step: Option<String>,
    /// A summary or failure reason, once finished.
    pub detail: Option<String>,
    /// Non-fatal shortcomings, once finished successfully.
    pub degradation: Option<String>,
    /// When it was created, in Unix seconds.
    pub created_at: i64,
    /// When it last changed, in Unix seconds.
    pub updated_at: i64,
}

impl From<JobRecord> for JobDto {
    fn from(record: JobRecord) -> Self {
        let (state, step, detail, degradation) = match &record.state {
            JobState::Queued => ("queued".to_owned(), None, None, None),
            JobState::Running { step } => (
                "running".to_owned(),
                Some(step.as_str().to_owned()),
                None,
                None,
            ),
            JobState::Succeeded {
                summary,
                degradation,
            } => (
                "succeeded".to_owned(),
                None,
                Some(summary.clone()),
                degradation.as_ref().map(|d| d.as_str().to_owned()),
            ),
            JobState::Failed { reason } => ("failed".to_owned(), None, Some(reason.clone()), None),
        };

        Self {
            id: record.id.as_str().to_owned(),
            kind: record.kind.as_str().to_owned(),
            target: record.target.label(),
            state,
            step,
            detail,
            degradation,
            created_at: record.created_at.as_unix_seconds(),
            updated_at: record.updated_at.as_unix_seconds(),
        }
    }
}

/// An audit record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditDto {
    /// Entry identifier.
    pub id: String,
    /// The action attempted.
    pub action: String,
    /// Who attempted it, already redacted by the domain.
    pub actor: String,
    /// What it was attempted on.
    pub target: String,
    /// Whether it succeeded.
    pub succeeded: bool,
    /// The failure reason, when it failed.
    pub reason: Option<String>,
    /// When it happened, in Unix seconds.
    pub at: i64,
}

impl From<AuditEntry> for AuditDto {
    fn from(entry: AuditEntry) -> Self {
        Self {
            id: entry.id.as_str().to_owned(),
            action: entry.action.as_str().to_owned(),
            actor: entry.actor.label(),
            target: entry.target.label(),
            succeeded: entry.result.is_success(),
            reason: entry.result.reason().map(ToOwned::to_owned),
            at: entry.at.as_unix_seconds(),
        }
    }
}

/// A capability observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityDto {
    /// Capability name.
    pub kind: String,
    /// One of the five capability states.
    pub status: String,
    /// What was probed, and what was seen.
    pub evidence: String,
}

/// One environment field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentDto {
    /// Distribution family.
    pub os: String,
    /// Distribution version, if known.
    pub os_version: Option<String>,
    /// Machine architecture.
    pub arch: String,
    /// Kernel release, if readable.
    pub kernel: Option<String>,
    /// The init system in use.
    pub init: String,
    /// What kind of environment this is.
    pub container: String,
}

/// The detected environment and its capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilitiesDto {
    /// The environment snapshot.
    pub environment: EnvironmentDto,
    /// The probed capabilities.
    pub capabilities: Vec<CapabilityDto>,
}

impl From<CapabilitiesView> for CapabilitiesDto {
    fn from(view: CapabilitiesView) -> Self {
        Self {
            environment: environment_of(&view.environment),
            capabilities: capabilities_dto(&view.capabilities),
        }
    }
}

/// Maps an environment snapshot.
#[must_use]
pub fn environment_of(env: &SystemEnvironment) -> EnvironmentDto {
    EnvironmentDto {
        os: format!("{:?}", env.os()).to_ascii_lowercase(),
        os_version: env.os_version().map(ToOwned::to_owned),
        arch: format!("{:?}", env.arch()).to_ascii_lowercase(),
        kernel: env.kernel().map(ToOwned::to_owned),
        init: format!("{:?}", env.init()).to_ascii_lowercase(),
        container: format!("{:?}", env.container()).to_ascii_lowercase(),
    }
}

/// Maps a capability set.
fn capabilities_dto(set: &CapabilitySet) -> Vec<CapabilityDto> {
    set.iter()
        .map(|capability| CapabilityDto {
            kind: capability.kind().as_str().to_owned(),
            status: capability.status().as_str().to_owned(),
            evidence: format!(
                "{}: {}",
                capability.evidence().probe,
                capability.evidence().detail
            ),
        })
        .collect()
}

/// A doctor finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingDto {
    /// How serious it is.
    pub severity: String,
    /// A stable identifier for the finding.
    pub code: String,
    /// The human-readable explanation.
    pub message: String,
}

/// The doctor report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorDto {
    /// The overall verdict.
    pub verdict: String,
    /// What was found, including clean checks.
    pub findings: Vec<FindingDto>,
    /// The environment snapshot the report was made against.
    pub environment: EnvironmentDto,
}

/// Pagination and filtering, shared by the list endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct ListQuery {
    /// Maximum entries to return. Clamped to a ceiling by the handler.
    pub limit: Option<usize>,
}

impl ListQuery {
    /// The effective limit, bounded so a client cannot ask for everything.
    #[must_use]
    pub fn effective_limit(&self, default: usize, max: usize) -> usize {
        self.limit.unwrap_or(default).min(max)
    }
}

/// The largest page any list endpoint will return.
///
/// A bound rather than a preference: an unbounded list would let one request pull
/// every audit record into memory.
pub const MAX_PAGE: usize = 500;

/// The default page size when a client does not ask.
pub const DEFAULT_PAGE: usize = 100;

/// A created or updated resource's identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdDto {
    /// The identifier.
    pub id: String,
}

/// A request to create or replace a subscription.
#[derive(Debug, Clone, Deserialize)]
pub struct SubscriptionInput {
    /// Display name.
    pub name: String,
    /// The subscription URL.
    pub url: String,
    /// Optional user agent for the fetch.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Optional schedule interval in seconds.
    #[serde(default)]
    pub schedule_seconds: Option<u64>,
}

/// A request to validate a configuration document.
#[derive(Debug, Clone, Deserialize)]
pub struct ValidateInput {
    /// The document to validate.
    pub body: String,
}

/// The result of validating a document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationDto {
    /// The preflight layer's outcome.
    pub preflight: String,
    /// The syntax layer's outcome.
    pub syntax: String,
    /// The semantic layer's outcome.
    pub semantic: String,
    /// Whether every layer passed or was skipped.
    pub acceptable: bool,
}

/// An error response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorDto {
    /// A stable machine-readable code.
    pub code: String,
    /// A human-readable message, with anything credential-shaped removed.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::mihomo::status::MihomoStatus;
    use proxy_domain::shared::id::MihomoInstanceId;
    use proxy_domain::shared::time::Timestamp;

    fn status_view() -> MihomoStatusView {
        MihomoStatusView {
            instance: MihomoInstanceId::parse("default").expect("valid"),
            name: "default".to_owned(),
            status: MihomoStatus::RUNNING,
            active_config: None,
            running_build: None,
            health: None,
            last_failure: None,
        }
    }

    /// The wire shape is the contract, so it is asserted by name.
    #[test]
    fn a_status_response_has_the_documented_shape() {
        let dto: MihomoStatusDto = status_view().into();
        assert_eq!(dto.instance, "default");
        assert_eq!(dto.state.status, "Running");
        assert!(dto.state.live);
        assert!(dto.state.serving);
        assert!(dto.active_config.is_none());

        let json = serde_json::to_value(&dto).expect("serialisable");
        assert_eq!(json["state"]["status"], "Running");
        assert!(json["state"]["live"].as_bool().expect("bool"));
    }

    /// A version identifier is a string on the wire, not a nested object: the
    /// identifier is opaque to a client.
    #[test]
    fn identifiers_are_flat_strings() {
        let dto = ConfigVersionDto::from(ConfigVersionSummary {
            id: proxy_domain::shared::id::ConfigVersionId::parse("default-003").expect("valid"),
            label: "v003".to_owned(),
            source: "manual".to_owned(),
            checksum: "fnv1a64:0000000000000001".to_owned(),
            is_active: true,
            created_at: Timestamp::from_unix_seconds(1),
            activated_at: Some(Timestamp::from_unix_seconds(2)),
        });
        let json = serde_json::to_value(&dto).expect("serialisable");
        assert_eq!(json["id"], "default-003");
        assert_eq!(json["activated_at"], 2);
    }

    /// A job's state is flattened into a label plus optional detail, so a client
    /// does not have to model the enum.
    #[test]
    fn a_job_state_is_flattened() {
        let record = JobRecord {
            id: proxy_domain::shared::id::JobId::parse("job-1").expect("valid"),
            kind: proxy_application::ports::job_registry::JobKind::MihomoStart,
            target: proxy_application::ports::job_registry::JobTarget::Instance(
                MihomoInstanceId::parse("default").expect("valid"),
            ),
            state: JobState::Succeeded {
                summary: "started".to_owned(),
                degradation: Some(
                    proxy_application::ports::job_registry::Degradation::HealthUnconfirmed {
                        reason: "port closed".to_owned(),
                    },
                ),
            },
            created_at: Timestamp::from_unix_seconds(1),
            updated_at: Timestamp::from_unix_seconds(2),
        };
        let dto = JobDto::from(record);
        assert_eq!(dto.state, "succeeded");
        assert_eq!(dto.detail.as_deref(), Some("started"));
        assert_eq!(dto.degradation.as_deref(), Some("health-unconfirmed"));
        assert_eq!(dto.kind, "mihomo.start");
        assert_eq!(dto.target, "instance:default");
    }

    /// A list limit must be bounded, or one request pulls everything.
    #[test]
    fn list_limits_are_bounded() {
        let unbounded = ListQuery { limit: None };
        assert_eq!(
            unbounded.effective_limit(DEFAULT_PAGE, MAX_PAGE),
            DEFAULT_PAGE
        );

        let greedy = ListQuery {
            limit: Some(usize::MAX),
        };
        assert_eq!(greedy.effective_limit(DEFAULT_PAGE, MAX_PAGE), MAX_PAGE);

        let modest = ListQuery { limit: Some(5) };
        assert_eq!(modest.effective_limit(DEFAULT_PAGE, MAX_PAGE), 5);
    }

    /// An audit entry is exposed with its actor and target already redacted by the
    /// domain, and no free-form detail field exists to leak one.
    #[test]
    fn an_audit_response_carries_no_free_form_detail() {
        let entry = AuditEntry::new(
            proxy_domain::shared::id::AuditEntryId::parse("a1").expect("valid"),
            proxy_domain::audit::AuditAction::SubscriptionUpdate,
            proxy_domain::audit::AuditActor::LocalRoot,
            proxy_domain::audit::AuditTarget::HostFirewall,
            proxy_domain::audit::AuditResult::Success,
            Timestamp::from_unix_seconds(1),
        );
        let json = serde_json::to_value(AuditDto::from(entry)).expect("serialisable");
        assert_eq!(json["action"], "subscription.update");
        assert_eq!(json["actor"], "local:root");
        // Exactly the documented keys: a new one is a deliberate API change.
        let keys: Vec<&str> = json
            .as_object()
            .expect("object")
            .keys()
            .map(|k| k.as_str())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![
                "action",
                "actor",
                "at",
                "id",
                "reason",
                "succeeded",
                "target"
            ]
        );
    }
}
