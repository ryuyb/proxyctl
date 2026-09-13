//! Queries: read-only use cases.
//!
//! Queries deliberately do **not** take the per-instance lock. An activation can
//! hold that lock for seconds while it validates, reloads, and health-checks; if
//! status queries queued behind it, a dashboard would freeze exactly when an
//! operator most needs to watch what is happening. Nothing here mutates state,
//! so there is nothing to serialize against.
//!
//! Queries also do not write audit records: audit exists for state-changing
//! privileged actions, and logging every status poll would bury the signal.

use proxy_domain::configuration::ConfigVersion;
use proxy_domain::mihomo::{MihomoBuild, MihomoInstance, MihomoStatus};
use proxy_domain::shared::id::ConfigVersionId;
use proxy_domain::shared::time::Timestamp;
use proxy_domain::subscription::Subscription;
use proxy_domain::system::capability::CapabilitySet;
use proxy_domain::system::doctor::DoctorReport;
use proxy_domain::system::environment::SystemEnvironment;

use crate::context::AppContext;
use crate::error::ApplicationError;
use crate::ports::job_registry::JobRecord;
use crate::ports::types::HealthReport;

/// A snapshot of one instance.
#[derive(Debug, Clone)]
pub struct MihomoStatusView {
    /// Instance identifier.
    pub instance: proxy_domain::shared::id::MihomoInstanceId,
    /// Display name.
    pub name: String,
    /// Lifecycle state.
    pub status: MihomoStatus,
    /// The configuration version recorded as active.
    pub active_config: Option<ConfigVersionId>,
    /// The kernel build currently running, if the control API answered.
    pub running_build: Option<MihomoBuild>,
    /// The most recent health observation, if one was taken.
    pub health: Option<HealthReport>,
    /// The last recorded failure, if any.
    pub last_failure: Option<String>,
}

impl MihomoStatusView {
    /// Whether the instance is currently serving traffic.
    #[must_use]
    pub const fn is_serving(&self) -> bool {
        self.status.is_serving()
    }
}

/// Reads instance status.
pub struct GetMihomoStatus;

impl GetMihomoStatus {
    /// Builds a status snapshot.
    ///
    /// The health probe is attempted but its failure is not an error: an
    /// unreachable control API is exactly the condition the caller wants to see
    /// reported, so it becomes `health: None` alongside the lifecycle state.
    ///
    /// # Errors
    /// Returns an error only when the active version cannot be read from storage.
    pub async fn execute(
        ctx: &AppContext,
        instance: &MihomoInstance,
    ) -> Result<MihomoStatusView, ApplicationError> {
        let active = ctx.configs.active(&ctx.instance).await?;
        let health = ctx.controller.health_check().await.ok();
        let running_build = ctx.controller.version().await.ok();

        Ok(MihomoStatusView {
            instance: instance.id().clone(),
            name: instance.name().to_owned(),
            status: instance.status(),
            active_config: active.map(|v| v.id().clone()),
            running_build,
            health,
            last_failure: instance.last_failure().map(|f| f.reason.clone()),
        })
    }
}

/// A configuration version as presented to callers.
#[derive(Debug, Clone)]
pub struct ConfigVersionSummary {
    /// Identifier.
    pub id: ConfigVersionId,
    /// Display label, e.g. `v003`.
    pub label: String,
    /// Where it came from.
    pub source: String,
    /// Content checksum.
    pub checksum: String,
    /// Whether it is the active version.
    pub is_active: bool,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it was activated, if ever.
    pub activated_at: Option<Timestamp>,
}

impl From<&ConfigVersion> for ConfigVersionSummary {
    fn from(version: &ConfigVersion) -> Self {
        Self {
            id: version.id().clone(),
            label: version.label(),
            source: version.source().as_str().to_owned(),
            checksum: version.checksum().to_string(),
            is_active: version.is_active(),
            created_at: version.created_at(),
            activated_at: version.activated_at(),
        }
    }
}

/// Lists configuration versions.
pub struct ListConfigs;

impl ListConfigs {
    /// Lists versions for the context's instance, newest first.
    ///
    /// # Errors
    /// Returns an error when storage fails.
    pub async fn execute(
        ctx: &AppContext,
        limit: usize,
    ) -> Result<Vec<ConfigVersionSummary>, ApplicationError> {
        let active = ctx.configs.active(&ctx.instance).await?;
        let active_id = active.as_ref().map(|v| v.id().clone());

        let versions = ctx.configs.list(&ctx.instance, limit).await?;
        Ok(versions
            .iter()
            .map(|version| {
                let mut summary = Self::from(version);
                // The stored record may not carry activation, so trust the
                // pointer rather than the row.
                summary.is_active = active_id.as_ref() == Some(version.id());
                summary
            })
            .collect())
    }

    /// Builds a summary from a version alone.
    #[must_use]
    fn from(version: &ConfigVersion) -> ConfigVersionSummary {
        ConfigVersionSummary::from(version)
    }
}

/// A subscription as presented to callers.
#[derive(Debug, Clone)]
pub struct SubscriptionSummary {
    /// Identifier.
    pub id: proxy_domain::shared::id::SubscriptionId,
    /// Display name.
    pub name: String,
    /// Whether it participates in scheduled updates.
    pub enabled: bool,
    /// How often it updates, in seconds.
    pub interval_seconds: Option<u64>,
    /// The outcome of the last update, as a label.
    pub last_update: Option<String>,
    /// Whether it is due for an update.
    pub is_due: bool,
}

impl SubscriptionSummary {
    /// Projects a subscription, evaluating due-ness against `now`.
    #[must_use]
    pub fn from_subscription(subscription: &Subscription, now: Timestamp) -> Self {
        Self {
            id: subscription.id().clone(),
            name: subscription.name().to_owned(),
            enabled: subscription.is_enabled(),
            interval_seconds: subscription.schedule().map(|s| s.interval.as_seconds()),
            last_update: subscription
                .last_update()
                .map(|record| match &record.outcome {
                    proxy_domain::subscription::UpdateOutcome::Succeeded(_) => {
                        "succeeded".to_owned()
                    }
                    proxy_domain::subscription::UpdateOutcome::Failed(failure) => {
                        format!("failed({})", failure.kind())
                    }
                }),
            is_due: subscription.is_due(now),
        }
    }
}

/// Lists subscriptions.
pub struct ListSubscriptions;

impl ListSubscriptions {
    /// Lists subscriptions with their due-ness evaluated at `now`.
    ///
    /// # Errors
    /// Returns an error when storage fails.
    pub async fn execute(
        ctx: &AppContext,
        now: Timestamp,
    ) -> Result<Vec<SubscriptionSummary>, ApplicationError> {
        let subscriptions = ctx.subscriptions.list().await?;
        Ok(subscriptions
            .iter()
            .map(|s| SubscriptionSummary::from_subscription(s, now))
            .collect())
    }
}

/// Reads runtime capabilities.
pub struct GetCapabilities;

impl GetCapabilities {
    /// Probes the environment and its capabilities.
    ///
    /// # Errors
    /// Returns an error when probing itself fails. A capability that could not be
    /// determined is reported as unknown rather than as an error, because an
    /// incomplete answer is still useful.
    pub async fn execute(ctx: &AppContext) -> Result<CapabilitiesView, ApplicationError> {
        let environment = ctx.capabilities.environment().await?;
        let capabilities = ctx
            .capabilities
            .probe_all(crate::ports::capability_probe::ProbeOptions::default())
            .await?;
        Ok(CapabilitiesView {
            environment,
            capabilities,
        })
    }
}

/// The detected environment and capabilities.
#[derive(Debug, Clone)]
pub struct CapabilitiesView {
    /// The environment snapshot.
    pub environment: SystemEnvironment,
    /// The probed capability set.
    pub capabilities: CapabilitySet,
}

/// Runs diagnostics.
pub struct RunDoctor;

impl RunDoctor {
    /// Produces a diagnostic report.
    ///
    /// The report never fails to describe a degraded environment: reporting that
    /// TUN is unavailable is the point, so a missing capability is data rather
    /// than an error.
    ///
    /// # Errors
    /// Returns an error when the environment cannot be probed at all.
    pub async fn execute(ctx: &AppContext) -> Result<DoctorReport, ApplicationError> {
        let environment = ctx.capabilities.environment().await?;
        Ok(DoctorReport::from_environment(environment))
    }
}

/// Reads recent jobs.
pub struct ListJobs;

impl ListJobs {
    /// Lists recent jobs, newest first.
    ///
    /// # Errors
    /// Returns an error when the registry cannot be read.
    pub async fn execute(
        ctx: &AppContext,
        limit: usize,
    ) -> Result<Vec<JobRecord>, ApplicationError> {
        Ok(ctx.jobs.recent(limit).await?)
    }
}

/// Reads the kernel's proxy groups and nodes.
///
/// # Why this did not exist before
///
/// `MihomoController::proxies` has been implemented since the controller adapter
/// was written, and nothing ever called it. The TUI specification in AGENTS.md
/// lists proxy groups as a panel, so the query exists to serve that — and the
/// absence is a reminder that an implemented port method is not an exposed
/// feature.
///
/// It takes no lock, for the reason in this module's header: reading is not a
/// state change, and a dashboard must not freeze behind an activation.
///
/// # Errors
///
/// Returns [`ApplicationError::Port`] when the kernel is unreachable or answers in
/// a shape this build does not recognise.
pub struct ListProxies;

impl ListProxies {
    /// Reads groups and nodes.
    ///
    /// # Errors
    ///
    /// As [`ListProxies`].
    pub async fn execute(
        ctx: &AppContext,
    ) -> Result<crate::ports::types::ProxyList, ApplicationError> {
        Ok(ctx.controller.proxies().await?)
    }
}

/// Opens the kernel's log stream.
///
/// # Why this is a query and not just a pass-through
///
/// The interface layer must not reach into the context's ports directly, so
/// something in this layer has to own the translation. What the use case
/// contributes is the *rule* about failure: an unavailable stream is reported as
/// an error when it is opened, and a stream that ends later is not an error at
/// all, because a kernel restarting while someone watches logs is an event to
/// observe rather than a fault to report.
///
/// It deliberately does **not** touch the per-instance lock or write an audit
/// record, for the reasons in this module's header: observing is not a
/// state-changing operation, and auditing every `logs -f` would bury the record
/// that matters.
pub struct ObserveLogs;

impl ObserveLogs {
    /// Opens the log stream at `level` or above.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError::Port`] when the stream cannot be opened — the
    /// kernel is not running, or the endpoint is unusable. The level is passed to
    /// the kernel rather than filtered here, because one implementation of the
    /// ordering rule is enough.
    pub async fn execute(
        ctx: &AppContext,
        level: crate::ports::types::LogLevel,
    ) -> Result<
        crate::ports::mihomo_observer::BoxStream<crate::ports::mihomo_observer::LogEntry>,
        ApplicationError,
    > {
        Ok(ctx.observer.logs(level).await?)
    }
}

/// Opens the kernel's traffic stream.
///
/// # Errors
///
/// As [`ObserveLogs`].
pub struct ObserveTraffic;

impl ObserveTraffic {
    /// Opens the traffic stream.
    ///
    /// # Errors
    ///
    /// As [`ObserveLogs::execute`].
    pub async fn execute(
        ctx: &AppContext,
    ) -> Result<
        crate::ports::mihomo_observer::BoxStream<crate::ports::mihomo_observer::TrafficSample>,
        ApplicationError,
    > {
        Ok(ctx.observer.traffic().await?)
    }
}

/// Reads recent audit records.
pub struct ListAuditEntries;

impl ListAuditEntries {
    /// Lists recent audit entries, newest first.
    ///
    /// # Errors
    /// Returns an error when the sink cannot be read.
    pub async fn execute(
        ctx: &AppContext,
        limit: usize,
    ) -> Result<Vec<proxy_domain::audit::AuditEntry>, ApplicationError> {
        Ok(ctx.audit.recent(limit).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::configuration::{ConfigChecksum, ConfigSource, ConfigVersion};
    use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId};

    fn version(sequence: u64, active: bool) -> ConfigVersion {
        let version = ConfigVersion::record(
            ConfigVersionId::parse(format!("cfg-{sequence:03}")).expect("valid"),
            MihomoInstanceId::parse("default").expect("valid"),
            sequence,
            ConfigSource::Manual,
            ConfigChecksum::from_digest(sequence),
            Timestamp::from_unix_seconds(1_700_000_000),
        );
        if active {
            version.activated(now())
        } else {
            version
        }
    }

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_700_000_100)
    }

    #[test]
    fn summary_projects_every_field() {
        let summary = ConfigVersionSummary::from(&version(3, true));
        assert_eq!(summary.label, "v003");
        assert_eq!(summary.source, "manual");
        assert!(summary.is_active);
        assert!(summary.activated_at.is_some());
    }

    #[test]
    fn inactive_version_reports_no_activation() {
        let summary = ConfigVersionSummary::from(&version(1, false));
        assert!(!summary.is_active);
        assert!(summary.activated_at.is_none());
    }

    #[test]
    fn status_view_serving_reflects_state() {
        let view = MihomoStatusView {
            instance: MihomoInstanceId::parse("default").expect("valid"),
            name: "default".to_owned(),
            status: MihomoStatus::RUNNING,
            active_config: None,
            running_build: None,
            health: None,
            last_failure: None,
        };
        assert!(view.is_serving());

        let degraded = MihomoStatusView {
            status: MihomoStatus::DEGRADED,
            ..view
        };
        assert!(degraded.is_serving(), "a degraded instance still serves");
    }
}
