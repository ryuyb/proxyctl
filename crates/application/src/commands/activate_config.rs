//! Configuration activation.
//!
//! This is the **only** path by which a configuration becomes active. Rollback
//! reuses it rather than reimplementing the sequence, because a second
//! implementation of recovery would be the least-exercised code in the system
//! and would drift from the first.
//!
//! # Sequence and why each step is where it is
//!
//! ```text
//! lock                 serialize against other lifecycle work
//! preflight (L0)       ports free, geo data obtainable
//! syntax (L1)
//! semantic (L2)        kernel check plus field allow-list
//! validate             typestate gate: unvalidated candidates cannot proceed
//! save version         write before switching, so the pointer never dangles
//! set_active           atomic pointer switch
//! reload               ask the kernel to load it
//! health check (L3)    a reload that was accepted has not necessarily taken effect
//! ```
//!
//! The order of `save` and `set_active` is deliberate. Saving first means a
//! failure to switch leaves an unused version on disk, which is harmless.
//! Switching first would risk an active pointer to a version that was never
//! written.
//!
//! # Recovery
//!
//! Any failure after `set_active` triggers recovery, which restores the previous
//! version **by restarting the kernel**. Reloading is not used: a partially
//! applied configuration can leave the kernel unable to bind its listeners, and
//! a subsequent reload will not repair that — only a fresh process will.
//!
//! Recovery re-reads the active version from storage instead of trusting what
//! this function read earlier, so a storage fault produces an accurate report
//! rather than a false claim of success.

use proxy_domain::configuration::{ConfigCandidate, ConfigVersion, Unvalidated, ValidationReport};
use proxy_domain::shared::time::Timestamp;

use crate::context::AppContext;
use crate::error::ApplicationError;
use crate::ports::config_validator::PreflightContext;
use crate::ports::job_registry::{Degradation, JobKind, JobState, JobStep, JobTarget};
use crate::ports::mihomo_controller::ReloadRequest;
use crate::ports::types::{HealthReport, ReloadOutcome};

/// What to activate.
#[derive(Debug)]
pub struct ActivateConfigInput {
    /// A candidate that has not yet been validated.
    pub candidate: ConfigCandidate<Unvalidated>,
    /// Ports the candidate expects to bind, used by the preflight layer.
    pub desired_ports: Vec<u16>,
    /// Whether the candidate needs geo data files.
    pub requires_geodata: bool,
    /// Whether the host currently has network access.
    pub online: bool,
    /// Whether geo data is already present locally.
    pub geodata_present: bool,
    /// Version to restore if activation fails.
    ///
    /// Treated as an intention, not as truth: recovery verifies what is actually
    /// active before reporting an outcome.
    pub rollback_to: Option<proxy_domain::shared::id::ConfigVersionId>,
}

impl ActivateConfigInput {
    /// Builds an input with no rollback target and a simple preflight.
    #[must_use]
    pub fn new(candidate: ConfigCandidate<Unvalidated>, desired_ports: Vec<u16>) -> Self {
        Self {
            candidate,
            desired_ports,
            requires_geodata: false,
            online: true,
            geodata_present: false,
            rollback_to: None,
        }
    }
}

/// The result of an activation attempt.
#[derive(Debug)]
pub struct ActivateConfigOutput {
    /// The version that is active when this returns.
    pub active: proxy_domain::shared::id::ConfigVersionId,
    /// The version this call tried to activate.
    pub activated: proxy_domain::shared::id::ConfigVersionId,
    /// Whether the candidate ended up active.
    pub succeeded: bool,
    /// Whether recovery ran.
    pub rolled_back: bool,
    /// The version observed active after recovery, if it ran.
    pub restored_to: Option<proxy_domain::shared::id::ConfigVersionId>,
    /// The validation report.
    pub report: ValidationReport,
    /// Kernel acceptance of the reload, if reached.
    pub reload: Option<ReloadOutcome>,
    /// Health when checked, if reached.
    pub health: Option<HealthReport>,
    /// Non-fatal shortcomings.
    pub degradation: Option<Degradation>,
}

/// How recovery ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Recovery {
    /// The previous version was restored and confirmed.
    Restored {
        to: proxy_domain::shared::id::ConfigVersionId,
    },
    /// No version was available to restore.
    NoTarget,
    /// Restoration could not be confirmed; the observed value is what is real.
    Unconfirmed {
        observed: Option<proxy_domain::shared::id::ConfigVersionId>,
    },
}

/// Activates a configuration version.
pub struct ActivateConfig;

impl ActivateConfig {
    /// Runs the activation sequence.
    ///
    /// # Errors
    ///
    /// Returns an error only when the resulting state cannot be established —
    /// that is, when storage is unreachable so no version can be confirmed
    /// active. Failures that leave the previous version serving are reported
    /// through [`ActivateConfigOutput`] instead, because they are expected
    /// outcomes rather than faults.
    pub async fn execute(
        ctx: &AppContext,
        input: ActivateConfigInput,
        now: Timestamp,
    ) -> Result<ActivateConfigOutput, ApplicationError> {
        // Serialize against other lifecycle work on this instance.
        let _lock = ctx.locks.acquire(&ctx.instance).await;

        let job = ctx
            .jobs
            .create(
                JobKind::ConfigActivate,
                JobTarget::Instance(ctx.instance.clone()),
            )
            .await?;

        let result = Self::run(ctx, &input, &job, now).await;

        match &result {
            Ok(output) if output.succeeded => {
                let state = JobState::Succeeded {
                    summary: format!("activated {}", output.activated),
                    degradation: output.degradation.clone(),
                };
                ctx.jobs.update(&job, state.clone()).await?;
                ctx.events
                    .publish(crate::ports::event_publisher::DomainEvent::JobFinished {
                        id: job.clone(),
                        state,
                    });
            }
            Ok(output) => {
                let reason = match output.restored_to.as_ref() {
                    Some(to) => format!("activation failed; restored {to}"),
                    None => "activation failed; no version could be restored".to_owned(),
                };
                let state = JobState::Failed { reason };
                ctx.jobs.update(&job, state.clone()).await?;
                ctx.events
                    .publish(crate::ports::event_publisher::DomainEvent::JobFinished {
                        id: job,
                        state,
                    });
            }
            Err(e) => {
                let state = JobState::Failed {
                    reason: e.to_string(),
                };
                ctx.jobs.update(&job, state.clone()).await?;
                ctx.events
                    .publish(crate::ports::event_publisher::DomainEvent::JobFinished {
                        id: job,
                        state,
                    });
            }
        }

        result
    }

    /// The sequence itself, factored out so job bookkeeping stays in one place.
    async fn run(
        ctx: &AppContext,
        input: &ActivateConfigInput,
        job: &proxy_domain::shared::id::JobId,
        now: Timestamp,
    ) -> Result<ActivateConfigOutput, ApplicationError> {
        let advance = |step: JobStep| {
            ctx.events
                .publish(crate::ports::event_publisher::DomainEvent::JobProgress {
                    id: job.clone(),
                    step,
                })
        };

        // ---- Layers 0-2: static validation ---------------------------------
        advance(JobStep::Preflight);
        let report = validate_static(ctx, input).await?;

        // Record the failure before the typestate gate consumes the candidate, so
        // the caller can see which layer rejected it.
        //
        // A rejected validation is not an error: nothing was mutated, so there is
        // no state to recover and any previous version is untouched. Returning an
        // error here would force every caller to special-case the most ordinary
        // outcome there is — an operator submitting a config with a typo.
        if !report.is_acceptable() {
            let rejected = proxy_domain::shared::id::ConfigVersionId::parse(candidate_label(
                &input.candidate,
            ))?;

            // Report whatever is active; if nothing is, say so by echoing the
            // rejected candidate, which is accurate for a first-ever activation.
            let active = match ctx.configs.active(&ctx.instance).await? {
                Some(version) => version.id().clone(),
                None => rejected.clone(),
            };

            return Ok(ActivateConfigOutput {
                active,
                activated: rejected,
                succeeded: false,
                rolled_back: false,
                restored_to: None,
                report,
                reload: None,
                health: None,
                degradation: None,
            });
        }

        // Gate: only a validated candidate reaches the mutation steps.
        let validated = input.candidate.clone().validate(report.clone())?;
        let body = validated.body().clone();
        let source = validated.source().clone();

        // ---- Persist -------------------------------------------------------
        advance(JobStep::Persist);
        let sequence = ctx.configs.next_sequence(&ctx.instance).await?;
        let checksum = body.checksum();
        let version = ConfigVersion::record(
            proxy_domain::shared::id::ConfigVersionId::parse(format!(
                "{}-{sequence:03}",
                ctx.instance.as_str()
            ))?,
            ctx.instance.clone(),
            sequence,
            source,
            checksum,
            now,
        );
        ctx.configs.save(&version, &body).await?;

        // ---- Switch the pointer -------------------------------------------
        advance(JobStep::Activate);
        if let Err(e) = ctx.configs.set_active(&ctx.instance, version.id()).await {
            // Nothing has been switched yet from the kernel's perspective, but
            // the pointer may be in an unknown state, so recover by observation.
            return Ok(Self::recover(ctx, version.id(), input, report, None, None, e.into()).await);
        }

        // ---- Reload --------------------------------------------------------
        advance(JobStep::Reload);
        let reload = match ctx.controller.reload(ReloadRequest::Payload(body)).await {
            Ok(outcome) => outcome,
            Err(e) => {
                return Ok(
                    Self::recover(ctx, version.id(), input, report, None, None, e.into()).await,
                );
            }
        };

        if let ReloadOutcome::Rejected { http_status } = reload {
            return Ok(Self::recover(
                ctx,
                version.id(),
                input,
                report,
                Some(reload),
                None,
                ApplicationError::MihomoReloadFailed(format!(
                    "kernel rejected the configuration (status {http_status})"
                )),
            )
            .await);
        }

        // ---- Health check --------------------------------------------------
        // An accepted reload is not an effective one: the kernel's apply step
        // reports nothing, so the only way to know is to probe.
        advance(JobStep::HealthCheck);
        let health = match ctx.controller.health_check().await {
            Ok(health) => health,
            Err(e) => {
                return Ok(Self::recover(
                    ctx,
                    version.id(),
                    input,
                    report,
                    Some(reload),
                    None,
                    e.into(),
                )
                .await);
            }
        };

        if !health.is_healthy() {
            return Ok(Self::recover(
                ctx,
                version.id(),
                input,
                report,
                Some(reload),
                Some(health),
                ApplicationError::ConfigActivationFailed(
                    "health check failed after reload".to_owned(),
                ),
            )
            .await);
        }

        // ---- Success -------------------------------------------------------
        let degradation = Self::audit(ctx, &version, now).await;
        ctx.events.publish(
            crate::ports::event_publisher::DomainEvent::ConfigActivated {
                instance: ctx.instance.clone(),
                version: version.id().clone(),
            },
        );

        Ok(ActivateConfigOutput {
            active: version.id().clone(),
            activated: version.id().clone(),
            succeeded: true,
            rolled_back: false,
            restored_to: None,
            report,
            reload: Some(reload),
            health: Some(health),
            degradation,
        })
    }

    /// Restores the previous version after a failure.
    #[allow(clippy::too_many_arguments)]
    async fn recover(
        ctx: &AppContext,
        attempted: &proxy_domain::shared::id::ConfigVersionId,
        input: &ActivateConfigInput,
        report: ValidationReport,
        reload: Option<ReloadOutcome>,
        health: Option<HealthReport>,
        cause: ApplicationError,
    ) -> ActivateConfigOutput {
        let outcome = Self::rollback(ctx, input.rollback_to.clone()).await;

        let (rolled_back, restored_to, active) = match outcome {
            Recovery::Restored { to } => (true, Some(to.clone()), to),
            Recovery::NoTarget => (false, None, attempted.clone()),
            Recovery::Unconfirmed { observed } => {
                let active = observed.clone().unwrap_or_else(|| attempted.clone());
                (false, observed, active)
            }
        };

        if rolled_back {
            if let Some(to) = &restored_to {
                ctx.events.publish(
                    crate::ports::event_publisher::DomainEvent::ConfigRolledBack {
                        instance: ctx.instance.clone(),
                        to: to.clone(),
                    },
                );
            }
        }

        let degradation = Some(Degradation::HealthUnconfirmed {
            reason: cause.to_string(),
        });

        ActivateConfigOutput {
            active,
            activated: attempted.clone(),
            succeeded: false,
            rolled_back,
            restored_to,
            report,
            reload,
            health,
            degradation,
        }
    }

    /// Restores a version **by observation**.
    ///
    /// The requested target is only a hint; what actually becomes active is
    /// determined by re-reading storage afterwards.
    async fn rollback(
        ctx: &AppContext,
        intent: Option<proxy_domain::shared::id::ConfigVersionId>,
    ) -> Recovery {
        let observed = match ctx.configs.active(&ctx.instance).await {
            Ok(active) => active,
            Err(_) => return Recovery::Unconfirmed { observed: None },
        };

        let Some(target) = intent.or_else(|| observed.as_ref().map(|v| v.id().clone())) else {
            return Recovery::NoTarget;
        };

        if ctx
            .configs
            .set_active(&ctx.instance, &target)
            .await
            .is_err()
        {
            // Storage is unreachable. Report what is observed rather than
            // claiming a restoration that cannot be verified.
            return Recovery::Unconfirmed {
                observed: observed.map(|v| v.id().clone()),
            };
        }

        // Restart rather than reload: a reload cannot repair a partially applied
        // configuration.
        if Self::restart_kernel(ctx).await.is_err() {
            let confirmed = ctx
                .configs
                .active(&ctx.instance)
                .await
                .ok()
                .flatten()
                .map(|v| v.id().clone());
            return Recovery::Unconfirmed {
                observed: confirmed,
            };
        }

        match ctx.configs.active(&ctx.instance).await {
            Ok(Some(active)) if active.id() == &target => Recovery::Restored { to: target },
            Ok(other) => Recovery::Unconfirmed {
                observed: other.map(|v| v.id().clone()),
            },
            Err(_) => Recovery::Unconfirmed { observed: None },
        }
    }

    /// Restarts the kernel process.
    async fn restart_kernel(ctx: &AppContext) -> Result<(), ApplicationError> {
        let start = ctx.start_options().ok_or_else(|| {
            // Reachable during activation recovery, so the advice differs from
            // `StartMihomo`'s: a configuration *is* being activated, which means one
            // exists, so the missing piece is the kernel binary rather than a
            // document to add.
            ApplicationError::InvalidState(
                "the kernel cannot be restarted because its binary path is not \
                 configured. Set kernel.binary in the configuration, or install a \
                 kernel with `proxyctl mihomo update <VERSION>`"
                    .to_owned(),
            )
        })?;

        if let Some(handle) = ctx.current_handle() {
            ctx.process
                .stop(&handle, crate::DEFAULT_STOP_TIMEOUT)
                .await
                .map_err(ApplicationError::from)?;
        }

        let handle = ctx
            .process
            .start(&start)
            .await
            .map_err(ApplicationError::from)?;
        ctx.remember_process(handle, start);
        Ok(())
    }

    /// Writes an audit record, returning a degradation if it fails.
    ///
    /// Audit failure does not fail the operation: the change already happened and
    /// is legitimate, so the honest report is "succeeded, record missing".
    async fn audit(
        ctx: &AppContext,
        version: &ConfigVersion,
        now: Timestamp,
    ) -> Option<Degradation> {
        use proxy_domain::audit::{AuditAction, AuditActor, AuditEntry, AuditResult, AuditTarget};
        use proxy_domain::shared::id::AuditEntryId;

        let id = match AuditEntryId::parse(format!("{}-{}", version.id(), now.as_unix_seconds())) {
            Ok(id) => id,
            Err(_) => {
                return Some(Degradation::AuditUnavailable {
                    reason: "could not construct an audit id".to_owned(),
                });
            }
        };

        let entry = AuditEntry::new(
            id,
            AuditAction::ConfigActivate,
            AuditActor::LocalRoot,
            AuditTarget::Config(version.id().clone()),
            AuditResult::Success,
            now,
        );

        match ctx.audit.record(entry).await {
            Ok(()) => None,
            Err(e) => Some(Degradation::AuditUnavailable {
                reason: e.to_string(),
            }),
        }
    }
}

/// A reporting label for a candidate that was never persisted.
///
/// A candidate has no version id until it is saved, so failures before `save`
/// have to name something. Deriving the label from the checksum keeps it stable
/// and non-empty.
fn candidate_label(candidate: &ConfigCandidate<Unvalidated>) -> String {
    format!("candidate-{}", candidate.checksum())
}

/// Runs the three static validation layers and returns their combined report.
///
/// Extracted so `StoreConfig` applies exactly the same gate as activation. A second
/// copy would be a second definition of "acceptable", and the two would drift the
/// first time a layer is added or reordered — with the divergence appearing only as
/// a document that stores but will not activate.
pub(crate) async fn validate_static(
    ctx: &AppContext,
    input: &ActivateConfigInput,
) -> Result<ValidationReport, ApplicationError> {
    let preflight_ctx = PreflightContext {
        desired_ports: input.desired_ports.clone(),
        requires_geodata: input.requires_geodata,
        geodata_present: input.geodata_present,
        online: input.online,
    };
    let preflight = ctx
        .validator
        .preflight(input.candidate.body(), &preflight_ctx)
        .await?;
    let syntax = ctx
        .validator
        .validate_syntax(input.candidate.body())
        .await?;
    let semantic = ctx
        .validator
        .validate_semantic(input.candidate.body())
        .await?;

    Ok(ValidationReport::static_layers(preflight, syntax, semantic))
}
