//! Rolling back to a previously recorded configuration version.
//!
//! This deliberately delegates to [`ActivateConfig`] rather than reimplementing
//! the sequence. A second implementation of "validate, persist, activate,
//! verify, recover" would be the least-exercised code in the system, and the
//! first place a divergence would hide. Rollback is therefore an activation of
//! an existing version — the only differences are where the body comes from and
//! that the previous version is known.
//!
//! Landing a rollback uses a kernel **restart**, not a reload. A reload cannot
//! recover from a configuration that was applied but failed to bind its
//! listeners, which is precisely the state a rollback is usually called to fix.

use proxy_domain::configuration::{ConfigCandidate, ConfigSource};
use proxy_domain::shared::id::ConfigVersionId;
use proxy_domain::shared::time::Timestamp;

use crate::commands::activate_config::{ActivateConfig, ActivateConfigInput, ActivateConfigOutput};
use crate::context::AppContext;
use crate::error::ApplicationError;

/// What to roll back to.
#[derive(Debug)]
pub struct RollbackConfigInput {
    /// The version to restore.
    pub target: ConfigVersionId,
    /// Ports the restored version will bind, for the preflight layer.
    pub desired_ports: Vec<u16>,
    /// Whether the host currently has network access.
    pub online: bool,
}

/// The result of a rollback.
#[derive(Debug)]
pub struct RollbackConfigOutput {
    /// The version active when this returns.
    pub active: ConfigVersionId,
    /// The version that was restored, if it became active.
    pub restored: Option<ConfigVersionId>,
    /// The underlying activation result, for reporting.
    pub activation: ActivateConfigOutput,
}

/// Restores an earlier configuration version.
pub struct RollbackConfig;

impl RollbackConfig {
    /// Rolls back to `input.target`.
    ///
    /// # Errors
    ///
    /// Returns an error when the target version does not exist, or when the
    /// resulting state cannot be established. A rollback that fails to restore
    /// the target but leaves a known version active is reported through
    /// [`RollbackConfigOutput`], since that is a state an operator can act on.
    pub async fn execute(
        ctx: &AppContext,
        input: RollbackConfigInput,
        now: Timestamp,
    ) -> Result<RollbackConfigOutput, ApplicationError> {
        // Read the version being restored. Its body is the candidate.
        let target = ctx.configs.get(&input.target).await?.ok_or_else(|| {
            ApplicationError::NotFound(format!("config version {}", input.target))
        })?;
        let body = ctx.configs.read_body(&target).await?;

        // Remember what is active now, so a failed rollback has somewhere to
        // return to.
        let previous = ctx.configs.active(&ctx.instance).await?;
        let previous_id = previous.as_ref().map(|v| v.id().clone());

        let candidate = ConfigCandidate::new(
            ctx.instance.clone(),
            ConfigSource::Rollback {
                from: previous_id.clone().unwrap_or_else(|| input.target.clone()),
            },
            body,
        );

        let activation = ActivateConfig::execute(
            ctx,
            ActivateConfigInput {
                candidate,
                desired_ports: input.desired_ports,
                requires_geodata: false,
                online: input.online,
                geodata_present: false,
                rollback_to: previous_id,
            },
            now,
        )
        .await?;

        let restored = activation.succeeded.then(|| activation.activated.clone());

        Ok(RollbackConfigOutput {
            active: activation.active.clone(),
            restored,
            activation,
        })
    }
}
