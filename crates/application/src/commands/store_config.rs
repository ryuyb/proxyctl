//! Storing a configuration document as a version, without contacting the kernel.
//!
//! # Why this is separate from `ActivateConfig`
//!
//! `ActivateConfig` switches a *running* kernel to a new document: it persists the
//! version, moves the active pointer, reloads, and health-checks, with a rollback if
//! any step fails. Every one of those steps assumes a kernel exists.
//!
//! For a first deployment it does not. There is nothing to reload and no health to
//! check, so going through that path fails at the reload and treats it as a
//! rollback — reporting "stored, but rejected" for a document that was valid and
//! *had* been stored. Found by running it: `config add` could never activate a first
//! configuration, which was the one thing it existed to do.
//!
//! So the two differ in what they touch, not in how much they trust: both validate
//! through the same three layers, and only this one stops before the kernel.

use proxy_domain::configuration::{ConfigBody, ConfigSource, ConfigVersion};
use proxy_domain::shared::id::{ConfigVersionId, JobId};
use proxy_domain::shared::time::Timestamp;

use crate::AppContext;
use crate::commands::activate_config::ActivateConfigInput;
use crate::error::ApplicationError;
use crate::ports::job_registry::{JobKind, JobState, JobTarget};

/// The result of storing a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreConfigOutput {
    /// The version the document was stored as.
    ///
    /// Reported even when the document was rejected: it is the identifier that
    /// *would* have been used, and naming it is more useful than silence when the
    /// caller is told which version it is not.
    pub stored: ConfigVersionId,
    /// The version active after this call. `None` only when nothing is active, which
    /// is the state a first rejected store leaves behind.
    pub active: Option<ConfigVersionId>,
    /// Whether the document was accepted and is now the active version.
    pub succeeded: bool,
    /// A one-line reason when it was not accepted.
    pub reason: Option<String>,
}

/// Stores a configuration document and makes it the active version.
///
/// # What it does not do
///
/// It does not reload the kernel. An active pointer is what `StartMihomo` reads to
/// decide what to launch, so a stored-and-active version is exactly what a start
/// needs; reloading here would require a running kernel, which is the case this
/// exists to enable.
///
/// A deployment that wants the running kernel switched to a new document should
/// `activate` it, which does reload and can roll back.
pub struct StoreConfig;

impl StoreConfig {
    /// Validates, stores, and activates.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] only when the resulting state cannot be
    /// established — storage unreachable, so nothing can be confirmed either way. A
    /// document that fails validation is a *result*, not an error: an operator
    /// submitting a configuration with a typo is the most ordinary thing that
    /// happens here, and an error would force every caller to special-case it.
    pub async fn execute(
        ctx: &AppContext,
        input: ActivateConfigInput,
        now: Timestamp,
    ) -> Result<StoreConfigOutput, ApplicationError> {
        // Serialized against other configuration work on this instance, and held
        // across the sequence allocation so two stores cannot take the same number.
        let _lock = ctx.locks.acquire(&ctx.instance).await;

        let job = ctx
            .jobs
            .create(
                JobKind::ConfigActivate,
                JobTarget::Instance(ctx.instance.clone()),
            )
            .await?;

        let result = Self::run(ctx, &input, &job, now).await;

        let state = match &result {
            Ok(output) if output.succeeded => JobState::Succeeded {
                summary: format!("stored and activated {}", output.stored),
                degradation: None,
            },
            Ok(output) => JobState::Failed {
                reason: output
                    .reason
                    .clone()
                    .unwrap_or_else(|| "the document was rejected".to_owned()),
            },
            Err(e) => JobState::Failed {
                reason: e.to_string(),
            },
        };
        ctx.jobs.update(&job, state.clone()).await?;
        ctx.events
            .publish(crate::ports::event_publisher::DomainEvent::JobFinished { id: job, state });

        result
    }

    /// The sequence itself, so the job bookkeeping stays in one place.
    async fn run(
        ctx: &AppContext,
        input: &ActivateConfigInput,
        _job: &JobId,
        now: Timestamp,
    ) -> Result<StoreConfigOutput, ApplicationError> {
        // The same static gate `activate` applies, through the same validator
        // calls, so a document cannot be storable but not activatable.
        let report = super::activate_config::validate_static(ctx, input).await?;

        // Allocated before the gate is consulted so a rejected document can report
        // the identifier it would have had. The counter advances either way, which
        // is the cost of an unambiguous answer: a gap in the sequence is visible,
        // whereas a reused number would let a rejected label collide with a stored
        // one.
        let sequence = ctx.configs.next_sequence(&ctx.instance).await?;
        let stored = ConfigVersionId::parse(format!("{}-{sequence:03}", ctx.instance.as_str()))?;

        if !report.is_acceptable() {
            // Nothing is written. A rejected document must leave no trace in the
            // version list, or a later `activate` could select it.
            let active = ctx.configs.active(&ctx.instance).await?;
            let reason = match report.first_failure() {
                Some((level, reason)) => format!("{level:?}: {reason}"),
                None => "the document was rejected".to_owned(),
            };

            return Ok(StoreConfigOutput {
                stored,
                active: active.map(|version| version.id().clone()),
                succeeded: false,
                reason: Some(reason),
            });
        }

        // Only a validated candidate reaches the mutation steps, and the typestate
        // is what enforces it rather than a comment.
        let validated = input.candidate.clone().validate(report)?;
        let body: ConfigBody = validated.body().clone();
        let source: ConfigSource = validated.source().clone();

        let version = ConfigVersion::record(
            stored.clone(),
            ctx.instance.clone(),
            sequence,
            source,
            body.checksum(),
            now,
        );
        ctx.configs.save(&version, &body).await?;

        // The pointer is what `StartMihomo` reads, so this is the step that makes the
        // document reachable by a start.
        ctx.configs.set_active(&ctx.instance, version.id()).await?;

        Ok(StoreConfigOutput {
            stored,
            active: Some(version.id().clone()),
            succeeded: true,
            reason: None,
        })
    }
}
