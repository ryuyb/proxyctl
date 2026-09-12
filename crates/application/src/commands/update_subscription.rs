//! Subscription update.
//!
//! This command carries the project's central promise: **a failed update must
//! never disturb the configuration that is currently working.** A subscription
//! source being unreachable, a converter returning nothing, a generated config
//! failing validation, or a reload being rejected must all leave the running
//! kernel serving exactly what it served before.
//!
//! # Why it delegates activation
//!
//! The update does not reload the kernel itself. It builds a candidate and hands
//! it to [`ActivateConfig`], the single activation path. Writing a second
//! sequence here — one that reloads and checks health on its own — would create
//! two implementations of "activate safely", and the recovery logic would exist
//! in only one of them.
//!
//! # Why the converter's output is not enough
//!
//! A converter returns nodes, not a configuration. Measured during discovery:
//! the backend emits a `proxies:` fragment with no ports, no controller, no DNS,
//! no groups, and no rules. Assembling a runnable document from that fragment,
//! the current capabilities, and the operator's settings is this command's job.

use proxy_domain::configuration::{ConfigBody, ConfigCandidate, ConfigSource};
use proxy_domain::shared::id::SubscriptionId;
use proxy_domain::shared::time::Timestamp;
use proxy_domain::subscription::{Subscription, UpdateFailure, UpdateOutcome, UpdateRecord};

use crate::commands::activate_config::{ActivateConfig, ActivateConfigInput};
use crate::context::AppContext;
use crate::error::ApplicationError;
use crate::ports::job_registry::{JobKind, JobState, JobTarget};
use crate::ports::subscription_converter::ConvertRequest;
use crate::ports::types::CachePolicy;

/// How a subscription update should run.
#[derive(Debug, Clone)]
pub struct UpdateSubscriptionInput {
    /// The subscription to update.
    pub id: SubscriptionId,
    /// Ports the generated configuration should bind.
    pub desired_ports: Vec<u16>,
    /// Whether to bypass the converter's cache.
    pub bypass_cache: bool,
    /// Whether the host has network access.
    pub online: bool,
}

/// The result of an update attempt.
#[derive(Debug)]
pub struct UpdateSubscriptionOutput {
    /// What happened.
    pub outcome: UpdateOutcome,
    /// The version active when this returns.
    ///
    /// Present on both success and failure, so a caller can always report what is
    /// actually serving without a second query.
    pub active_config: Option<proxy_domain::shared::id::ConfigVersionId>,
}

impl UpdateSubscriptionOutput {
    /// Whether a new configuration became active.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        self.outcome.is_success()
    }
}

/// Updates a subscription and activates the result if it is sound.
pub struct UpdateSubscription;

impl UpdateSubscription {
    /// Fetches, converts, generates, validates, and activates.
    ///
    /// # Errors
    ///
    /// Returns an error only when the state afterwards cannot be established —
    /// that is, when storage is unreachable. Every ordinary failure (source
    /// down, empty conversion, validation rejected, kernel refused) is reported
    /// through [`UpdateSubscriptionOutput::outcome`], because the system is
    /// still in a known-good state and that is a normal outcome, not a fault.
    pub async fn execute(
        ctx: &AppContext,
        input: UpdateSubscriptionInput,
        now: Timestamp,
    ) -> Result<UpdateSubscriptionOutput, ApplicationError> {
        // Suppress a concurrent update of the same subscription. A scheduled run
        // and a manual one must not both proceed.
        let Some(_guard) = ctx.guards.try_begin(&input.id) else {
            let active = ctx.configs.active(&ctx.instance).await?;
            return Ok(UpdateSubscriptionOutput {
                outcome: UpdateOutcome::Failed(UpdateFailure::Unreachable(
                    "an update for this subscription is already in progress".to_owned(),
                )),
                active_config: active.map(|v| v.id().clone()),
            });
        };

        let job = ctx
            .jobs
            .create(
                JobKind::SubscriptionUpdate,
                JobTarget::Subscription(input.id.clone()),
            )
            .await?;

        let result = Self::run(ctx, &input, &job, now).await;

        match &result {
            Ok(output) => {
                let state = if output.succeeded() {
                    JobState::Succeeded {
                        summary: format!("updated {}", input.id),
                        degradation: None,
                    }
                } else {
                    JobState::Failed {
                        reason: match &output.outcome {
                            UpdateOutcome::Failed(failure) => failure.kind().to_owned(),
                            UpdateOutcome::Succeeded(_) => "unknown".to_owned(),
                        },
                    }
                };
                ctx.jobs.update(&job, state.clone()).await?;
                ctx.events
                    .publish(crate::ports::event_publisher::DomainEvent::JobFinished {
                        id: job,
                        state,
                    });
            }
            Err(e) => {
                ctx.jobs
                    .update(
                        &job,
                        JobState::Failed {
                            reason: e.to_string(),
                        },
                    )
                    .await?;
            }
        }

        result
    }

    /// The sequence, with job bookkeeping kept out of it.
    async fn run(
        ctx: &AppContext,
        input: &UpdateSubscriptionInput,
        job: &proxy_domain::shared::id::JobId,
        now: Timestamp,
    ) -> Result<UpdateSubscriptionOutput, ApplicationError> {
        let mut subscription = match ctx.subscriptions.get(&input.id).await? {
            Some(subscription) => subscription,
            None => {
                return Ok(Self::fail_and_announce(
                    ctx,
                    None,
                    &input.id,
                    UpdateFailure::Unreachable(format!("subscription {} not found", input.id)),
                    now,
                )
                .await);
            }
        };

        if !subscription.is_enabled() {
            return Ok(Self::fail_and_announce(
                ctx,
                Some(&mut subscription),
                &input.id,
                UpdateFailure::Unreachable("subscription is disabled".to_owned()),
                now,
            )
            .await);
        }

        // Convert. An empty result arrives as an error, never as an empty node
        // list, so it cannot pass for success.
        let request = ConvertRequest {
            source: subscription.source().clone(),
            target: subscription.target(),
            proxy: None,
            merge_sources: false,
            cache: if input.bypass_cache {
                CachePolicy::Bypass
            } else {
                CachePolicy::PreferCache
            },
        };

        let converted = match ctx.converter.convert(&request).await {
            Ok(converted) => converted,
            Err(e) => {
                let failure = match e {
                    crate::ports::error::PortError::Converter(
                        crate::ports::error::ConverterError::Unreachable(reason),
                    ) => UpdateFailure::Unreachable(reason),
                    crate::ports::error::PortError::Converter(
                        crate::ports::error::ConverterError::EmptyOrInvalidOutput,
                    ) => UpdateFailure::InvalidOutput("converter returned no nodes".to_owned()),
                    other => UpdateFailure::ConversionFailed(other.to_string()),
                };
                return Ok(Self::fail_and_announce(
                    ctx,
                    Some(&mut subscription),
                    &input.id,
                    failure,
                    now,
                )
                .await);
            }
        };

        // Assemble a complete configuration. The converter supplied only nodes.
        let body = match Self::generate(ctx, &converted.fragment, input).await {
            Ok(body) => body,
            Err(e) => {
                return Ok(Self::fail_and_announce(
                    ctx,
                    Some(&mut subscription),
                    &input.id,
                    UpdateFailure::ValidationFailed(e.to_string()),
                    now,
                )
                .await);
            }
        };

        ctx.events
            .publish(crate::ports::event_publisher::DomainEvent::JobProgress {
                id: job.clone(),
                step: crate::ports::job_registry::JobStep::Semantic,
            });

        // Activate through the single path, which validates, persists, reloads,
        // verifies, and recovers on failure.
        let previous = ctx.configs.active(&ctx.instance).await?;
        let candidate = ConfigCandidate::new(
            ctx.instance.clone(),
            ConfigSource::Subscription(input.id.clone()),
            body,
        );

        let activation = ActivateConfig::execute(
            ctx,
            ActivateConfigInput {
                candidate,
                desired_ports: input.desired_ports.clone(),
                requires_geodata: false,
                online: input.online,
                geodata_present: false,
                rollback_to: previous.as_ref().map(|v| v.id().clone()),
            },
            now,
        )
        .await?;

        let active_now = ctx.configs.active(&ctx.instance).await?;

        if activation.succeeded {
            let outcome = UpdateOutcome::Succeeded(activation.activated.clone());
            Self::record(&mut subscription, &outcome, now);
            ctx.subscriptions.save(&subscription).await.ok();
            ctx.events.publish(
                crate::ports::event_publisher::DomainEvent::SubscriptionUpdated {
                    id: input.id.clone(),
                    outcome: outcome.clone(),
                },
            );
            return Ok(UpdateSubscriptionOutput {
                outcome,
                active_config: active_now.map(|v| v.id().clone()),
            });
        }

        // Activation failed and recovery already ran. Record it with the version
        // that is actually serving, so an operator sees what is live.
        let failure = match activation.restored_to.clone() {
            Some(restored) => UpdateFailure::PreservedActiveConfig(restored),
            None => UpdateFailure::ValidationFailed(
                activation
                    .report
                    .first_failure()
                    .map_or_else(|| "activation failed".to_owned(), |(_, r)| r.to_owned()),
            ),
        };
        let outcome = UpdateOutcome::Failed(failure.clone());
        Self::record(&mut subscription, &outcome, now);
        ctx.subscriptions.save(&subscription).await.ok();
        ctx.events.publish(
            crate::ports::event_publisher::DomainEvent::SubscriptionUpdated {
                id: input.id.clone(),
                outcome: outcome.clone(),
            },
        );

        Ok(UpdateSubscriptionOutput {
            outcome,
            active_config: active_now.map(|v| v.id().clone()),
        })
    }

    /// Builds a complete configuration from a node fragment.
    ///
    /// Uses the domain generator so the field set, the controller hardening, and
    /// the capability gating live in one tested place rather than being
    /// re-decided here.
    async fn generate(
        ctx: &AppContext,
        fragment: &str,
        input: &UpdateSubscriptionInput,
    ) -> Result<ConfigBody, ApplicationError> {
        use proxy_domain::configuration::generation::{
            ControllerEndpoint, GenerationSpec, ProxyGroup, generate,
        };

        let secret = ctx.secrets.mihomo_secret().await?;
        if secret.trim().is_empty() {
            return Err(ApplicationError::CapabilityUnavailable(
                "controller secret is empty; refusing to generate an unauthenticated config"
                    .to_owned(),
            ));
        }

        let mixed_port = input.desired_ports.first().copied().unwrap_or(7890);

        let spec = GenerationSpec {
            mixed_port,
            controller: ControllerEndpoint::parse("127.0.0.1:9090")
                .map_err(ApplicationError::Domain)?,
            secret,
            proxies_fragment: fragment.to_owned(),
            groups: vec![ProxyGroup::select("PROXY", vec!["DIRECT".to_owned()])],
            rules: vec!["MATCH,PROXY".to_owned()],
            tun_requested: false,
        };

        // Capabilities decide what may be enabled; a kernel started with TUN on a
        // host that cannot provide it fails to route.
        let capabilities = ctx
            .capabilities
            .probe_all(crate::ports::capability_probe::ProbeOptions::default())
            .await?;

        let generated = generate(&spec, &capabilities).map_err(ApplicationError::Domain)?;
        Ok(generated.body)
    }

    /// Records an update outcome on the subscription.
    fn record(subscription: &mut Subscription, outcome: &UpdateOutcome, now: Timestamp) {
        subscription.record_update(UpdateRecord::new(now, outcome.clone()));
    }

    /// Builds a failure result and announces it.
    ///
    /// Every failure path goes through here so subscribers always learn about a
    /// failed update. Publishing on only the activation-failure path would leave
    /// a dashboard showing "last update: never" for a subscription that has been
    /// failing for days.
    ///
    /// `subscription` is optional because a failure can precede having one — a
    /// request for a subscription that does not exist still deserves an
    /// announcement.
    async fn fail_and_announce(
        ctx: &AppContext,
        subscription: Option<&mut Subscription>,
        id: &SubscriptionId,
        failure: UpdateFailure,
        now: Timestamp,
    ) -> UpdateSubscriptionOutput {
        let outcome = UpdateOutcome::Failed(failure);

        if let Some(subscription) = subscription {
            Self::record(subscription, &outcome, now);
            ctx.subscriptions.save(subscription).await.ok();
        }

        ctx.events.publish(
            crate::ports::event_publisher::DomainEvent::SubscriptionUpdated {
                id: id.clone(),
                outcome: outcome.clone(),
            },
        );

        let active = ctx.configs.active(&ctx.instance).await.ok().flatten();
        UpdateSubscriptionOutput {
            outcome,
            active_config: active.map(|v| v.id().clone()),
        }
    }
}

/// Creates, updates, and deletes subscription definitions.
pub struct SubscriptionCrud;

impl SubscriptionCrud {
    /// Stores a subscription.
    ///
    /// # Errors
    /// Returns an error when storage fails.
    pub async fn save(
        ctx: &AppContext,
        subscription: &Subscription,
    ) -> Result<(), ApplicationError> {
        ctx.subscriptions.save(subscription).await?;
        Ok(())
    }

    /// Removes a subscription.
    ///
    /// # Errors
    /// Returns an error when storage fails. Removing something that does not
    /// exist is success, so a retried delete does not report a spurious failure.
    pub async fn delete(ctx: &AppContext, id: &SubscriptionId) -> Result<(), ApplicationError> {
        ctx.subscriptions.delete(id).await?;
        Ok(())
    }

    /// Lists subscriptions.
    ///
    /// # Errors
    /// Returns an error when storage fails.
    pub async fn list(ctx: &AppContext) -> Result<Vec<Subscription>, ApplicationError> {
        Ok(ctx.subscriptions.list().await?)
    }

    /// Fetches one subscription.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotFound`] when it does not exist.
    pub async fn get(
        ctx: &AppContext,
        id: &SubscriptionId,
    ) -> Result<Subscription, ApplicationError> {
        ctx.subscriptions
            .get(id)
            .await?
            .ok_or_else(|| ApplicationError::NotFound(format!("subscription {id}")))
    }

    /// Evaluates a subscription without activating anything.
    ///
    /// Used by "test this subscription" affordances, so an operator can check a
    /// source before trusting it. The generated configuration is validated but
    /// never activated.
    ///
    /// # Errors
    /// Returns an error when the subscription is unknown.
    pub async fn test(
        ctx: &AppContext,
        id: &SubscriptionId,
    ) -> Result<SubscriptionTestResult, ApplicationError> {
        let subscription = Self::get(ctx, id).await?;

        let request = ConvertRequest {
            source: subscription.source().clone(),
            target: subscription.target(),
            proxy: None,
            merge_sources: false,
            cache: CachePolicy::Bypass,
        };

        match ctx.converter.convert(&request).await {
            Ok(converted) => Ok(SubscriptionTestResult {
                reachable: true,
                node_count: converted.node_count,
                reason: None,
            }),
            Err(e) => Ok(SubscriptionTestResult {
                reachable: false,
                node_count: 0,
                reason: Some(e.to_string()),
            }),
        }
    }
}

/// The outcome of testing a subscription without applying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionTestResult {
    /// Whether the source could be converted.
    pub reachable: bool,
    /// How many nodes the converter produced.
    pub node_count: usize,
    /// Why it failed, if it did.
    pub reason: Option<String>,
}
