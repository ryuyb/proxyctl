//! Kernel lifecycle commands.
//!
//! These are the operations that decide whether a process should exist, and they
//! are the reason the per-instance lock is not optional: two concurrent starts
//! would each observe a stopped instance and each spawn a kernel.
//!
//! # The decision lives in the aggregate
//!
//! Whether a start request should spawn a process is answered by
//! [`MihomoInstance::begin_start`], not by inspecting state here. The aggregate
//! knows that `Starting -> Starting` is illegal and reports
//! [`StartDecision::AlreadyStarting`], so a duplicate request cannot produce a
//! second process even if two callers race past the lock.
//!
//! # Readiness is not sleep
//!
//! After spawning, the command polls the control API until it answers or a
//! deadline passes. It deliberately does not `sleep` for a fixed interval: the
//! kernel's startup time varies with configuration size, and a fixed wait is
//! either too short (flaky) or too long (slow). A process that binds its
//! controller but fails to bind its proxy port is reported as degraded rather
//! than running, because that state looks healthy to an API-only check.

use std::time::Duration;

use proxy_domain::mihomo::{MihomoInstance, MihomoStatus};
use proxy_domain::shared::id::MihomoInstanceId;
use proxy_domain::shared::time::Timestamp;

use crate::context::AppContext;
use crate::error::ApplicationError;
use crate::ports::job_registry::{JobKind, JobState, JobTarget};
use crate::ports::mihomo_controller::ReloadRequest;
use crate::ports::process_manager::AllowedSignal;
use crate::ports::types::{HealthReport, ReloadOutcome};

/// How often to poll for readiness.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long to wait for the kernel to answer after spawning.
///
/// Generous, because a kernel with large geo data or many providers can take a
/// while, but bounded so a hung start surfaces as a failure instead of hanging
/// the caller forever.
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// What happened when a start was requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// A process was spawned and became healthy.
    Started {
        /// The process id.
        pid: u32,
        /// How long readiness took.
        ready_after: Duration,
    },
    /// A process was spawned but did not become fully healthy.
    Degraded {
        /// The process id.
        pid: u32,
        /// The health report observed.
        health: HealthReport,
    },
    /// A start was already in flight; nothing was spawned.
    AlreadyStarting,
    /// The instance was already serving; nothing was spawned.
    AlreadyRunning,
    /// A stop was in flight; the request was refused rather than raced.
    BusyStopping,
}

impl StartOutcome {
    /// Whether this outcome spawned a process.
    #[must_use]
    pub const fn spawned(&self) -> bool {
        matches!(self, Self::Started { .. } | Self::Degraded { .. })
    }
}

/// Loads the aggregate for the context's instance, under the caller's lock.
///
/// A first-ever start has no recorded state; a fresh aggregate is correct there,
/// not an error.
async fn load_instance(ctx: &AppContext) -> Result<MihomoInstance, ApplicationError> {
    match ctx.instances.load(&ctx.instance).await? {
        Some(instance) => Ok(instance),
        None => MihomoInstance::new(ctx.instance.clone(), ctx.instance.as_str())
            .map_err(ApplicationError::Domain),
    }
}

/// Persists the aggregate before the lock is released.
async fn save_instance(
    ctx: &AppContext,
    instance: &MihomoInstance,
) -> Result<(), ApplicationError> {
    ctx.instances.save(instance).await?;
    Ok(())
}

/// Starts the kernel.
pub struct StartMihomo;

impl StartMihomo {
    /// Spawns the kernel if it is not already running.
    ///
    /// # Contract
    ///
    /// The aggregate is loaded **after** the lock is acquired and saved before it
    /// is released. That ordering is what makes the duplicate-spawn guard real: a
    /// caller-supplied aggregate would let two callers each observe `Stopped`,
    /// both decide to spawn, and produce two kernels even though the lock
    /// serialized the calls.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be spawned, readiness times out,
    /// or state cannot be loaded. A duplicate request is not an error; it is
    /// reported as [`StartOutcome::AlreadyStarting`] or
    /// [`StartOutcome::AlreadyRunning`].
    pub async fn execute(
        ctx: &AppContext,
        now: Timestamp,
    ) -> Result<StartOutcome, ApplicationError> {
        let _lock = ctx.locks.acquire(&ctx.instance).await;

        let mut instance = load_instance(ctx).await?;

        // Ask the aggregate what to do. This is the duplicate-spawn guard, and it
        // now reads state that is shared rather than a copy the caller held.
        match instance.begin_start() {
            proxy_domain::mihomo::StartDecision::AlreadyStarting => {
                return Ok(StartOutcome::AlreadyStarting);
            }
            proxy_domain::mihomo::StartDecision::AlreadyRunning => {
                return Ok(StartOutcome::AlreadyRunning);
            }
            proxy_domain::mihomo::StartDecision::BusyStopping => {
                return Ok(StartOutcome::BusyStopping);
            }
            proxy_domain::mihomo::StartDecision::Spawn => {}
        }

        let job = ctx
            .jobs
            .create(
                JobKind::MihomoStart,
                JobTarget::Instance(instance.id().clone()),
            )
            .await?;

        let options = ctx.start_options().ok_or_else(|| {
            ApplicationError::InvalidState("no start options configured; cannot spawn".to_owned())
        })?;

        // Move to Starting and persist before spawning, so a concurrent request
        // arriving now sees a start in flight rather than a stopped instance.
        instance.transition(MihomoStatus::STARTING)?;
        save_instance(ctx, &instance).await?;

        let handle = match ctx.process.start(&options).await {
            Ok(handle) => handle,
            Err(e) => {
                instance.note_failure(proxy_domain::mihomo::FailureRecord::new(
                    format!("spawn failed: {e}"),
                    now,
                ));
                let _ = instance.transition(MihomoStatus::FAILED);
                save_instance(ctx, &instance).await?;
                ctx.jobs
                    .update(
                        &job,
                        JobState::Failed {
                            reason: e.to_string(),
                        },
                    )
                    .await?;
                return Err(e.into());
            }
        };
        ctx.remember_process(handle, options);

        let started_at = std::time::Instant::now();
        let health = Self::await_ready(ctx, DEFAULT_READY_TIMEOUT).await;

        let outcome = match health {
            Some(health) if health.is_healthy() => {
                instance.transition(MihomoStatus::RUNNING)?;
                instance.clear_failure();
                StartOutcome::Started {
                    pid: handle.pid,
                    ready_after: started_at.elapsed(),
                }
            }
            Some(health) => {
                // The control plane answered but something else is wrong. Keep
                // the process alive so an operator can inspect it.
                instance.transition(MihomoStatus::DEGRADED)?;
                StartOutcome::Degraded {
                    pid: handle.pid,
                    health,
                }
            }
            None => {
                instance.note_failure(proxy_domain::mihomo::FailureRecord::new(
                    format!("not ready within {DEFAULT_READY_TIMEOUT:?}"),
                    now,
                ));
                instance.transition(MihomoStatus::FAILED)?;
                save_instance(ctx, &instance).await?;
                let _ = ctx.process.stop(&handle, crate::DEFAULT_STOP_TIMEOUT).await;
                ctx.clear_handle();
                ctx.jobs
                    .update(
                        &job,
                        JobState::Failed {
                            reason: "controller did not become reachable".to_owned(),
                        },
                    )
                    .await?;
                return Err(ApplicationError::InvalidState(
                    "kernel did not become ready within the deadline".to_owned(),
                ));
            }
        };

        save_instance(ctx, &instance).await?;

        let summary = match &outcome {
            StartOutcome::Started { pid, .. } => format!("started pid {pid}"),
            StartOutcome::Degraded { pid, .. } => format!("pid {pid} degraded"),
            other => format!("{other:?}"),
        };
        ctx.jobs
            .update(
                &job,
                JobState::Succeeded {
                    summary,
                    degradation: None,
                },
            )
            .await?;

        Ok(outcome)
    }

    /// Polls the control API until it answers, or the deadline passes.
    async fn await_ready(ctx: &AppContext, timeout: Duration) -> Option<HealthReport> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Ok(health) = ctx.controller.health_check().await {
                return Some(health);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(READY_POLL_INTERVAL).await;
        }
    }
}

/// Stops the kernel.
pub struct StopMihomo;

impl StopMihomo {
    /// Stops the kernel gracefully, escalating to a kill on timeout.
    ///
    /// # Errors
    /// Returns an error only when the process cannot be stopped. Stopping an
    /// already-stopped instance is success.
    pub async fn execute(
        ctx: &AppContext,
        now: Timestamp,
    ) -> Result<StopOutcome, ApplicationError> {
        let _lock = ctx.locks.acquire(&ctx.instance).await;

        let mut instance = load_instance(ctx).await?;

        if !instance.status().is_live() {
            return Ok(StopOutcome::AlreadyStopped);
        }

        let job = ctx
            .jobs
            .create(
                JobKind::MihomoStop,
                JobTarget::Instance(instance.id().clone()),
            )
            .await?;

        instance.transition(MihomoStatus::STOPPING)?;
        save_instance(ctx, &instance).await?;

        let forced = match ctx.current_handle() {
            Some(handle) => {
                // Ask the control plane to exit first so it can flush state;
                // ignore failure, since the process may already be gone.
                let _ = ctx.controller.shutdown().await;
                let status = ctx
                    .process
                    .stop(&handle, crate::DEFAULT_STOP_TIMEOUT)
                    .await?;
                ctx.clear_handle();
                status.forced
            }
            // No handle means the agent did not spawn it, or already forgot it.
            // Nothing to stop, but the state should reflect reality rather than
            // stay stuck in a live state.
            None => false,
        };

        instance.transition(MihomoStatus::STOPPED)?;
        save_instance(ctx, &instance).await?;

        let summary = format!("stopped (forced: {forced})");
        let state = JobState::Succeeded {
            summary,
            degradation: None,
        };
        ctx.events
            .publish(crate::ports::event_publisher::DomainEvent::JobFinished {
                id: job.clone(),
                state: state.clone(),
            });
        ctx.jobs.update(&job, state).await?;

        let _ = now;
        Ok(StopOutcome::Stopped { forced })
    }
}

/// The result of a stop request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The kernel was stopped.
    Stopped {
        /// Whether a forced kill was needed.
        forced: bool,
    },
    /// Nothing was running.
    AlreadyStopped,
}

/// Restarts the kernel.
pub struct RestartMihomo;

impl RestartMihomo {
    /// Stops and starts the kernel.
    ///
    /// # Contract
    ///
    /// Implemented as stop-then-start rather than as a kernel self-restart
    /// request. A self-restart replaces the process image in place, which the
    /// agent cannot observe and which discards the state the agent tracks.
    ///
    /// # Errors
    /// Propagates stop and start failures.
    pub async fn execute(
        ctx: &AppContext,
        now: Timestamp,
    ) -> Result<StartOutcome, ApplicationError> {
        let _lock = ctx.locks.acquire(&ctx.instance).await;

        let mut instance = load_instance(ctx).await?;

        if instance.status().is_live() {
            if let Some(handle) = ctx.current_handle() {
                instance.transition(MihomoStatus::STOPPING)?;
                save_instance(ctx, &instance).await?;
                let _ = ctx.controller.shutdown().await;
                ctx.process
                    .stop(&handle, crate::DEFAULT_STOP_TIMEOUT)
                    .await?;
                ctx.clear_handle();
            }
            instance.transition(MihomoStatus::STOPPED)?;
            save_instance(ctx, &instance).await?;
        }

        // The lock is already held, so the start half is called directly.
        // Calling `StartMihomo::execute` would try to acquire the same
        // non-reentrant lock and deadlock.
        Self::start_locked(ctx, &mut instance, now).await
    }

    /// The start half, assuming the caller already holds the instance lock.
    async fn start_locked(
        ctx: &AppContext,
        instance: &mut MihomoInstance,
        now: Timestamp,
    ) -> Result<StartOutcome, ApplicationError> {
        let options = ctx.start_options().ok_or_else(|| {
            ApplicationError::InvalidState("no start options configured; cannot spawn".to_owned())
        })?;

        instance.transition(MihomoStatus::STARTING)?;
        save_instance(ctx, instance).await?;

        let handle = match ctx.process.start(&options).await {
            Ok(handle) => handle,
            Err(e) => {
                instance.transition(MihomoStatus::FAILED)?;
                save_instance(ctx, instance).await?;
                return Err(e.into());
            }
        };
        ctx.remember_process(handle, options);

        let outcome = match StartMihomo::await_ready(ctx, DEFAULT_READY_TIMEOUT).await {
            Some(health) if health.is_healthy() => {
                instance.transition(MihomoStatus::RUNNING)?;
                instance.clear_failure();
                StartOutcome::Started {
                    pid: handle.pid,
                    ready_after: Duration::ZERO,
                }
            }
            Some(health) => {
                instance.transition(MihomoStatus::DEGRADED)?;
                StartOutcome::Degraded {
                    pid: handle.pid,
                    health,
                }
            }
            None => {
                instance.note_failure(proxy_domain::mihomo::FailureRecord::new(
                    "not ready after restart",
                    now,
                ));
                instance.transition(MihomoStatus::FAILED)?;
                save_instance(ctx, instance).await?;
                let _ = ctx.process.stop(&handle, crate::DEFAULT_STOP_TIMEOUT).await;
                ctx.clear_handle();
                return Err(ApplicationError::InvalidState(
                    "kernel did not become ready after restart".to_owned(),
                ));
            }
        };

        save_instance(ctx, instance).await?;
        Ok(outcome)
    }
}

/// Reloads configuration in place, keeping the process.
pub struct ReloadMihomo;

impl ReloadMihomo {
    /// Reloads the active configuration.
    ///
    /// # Contract
    ///
    /// This reloads the version already recorded as active; it does not generate
    /// or validate a new one. Use [`ActivateConfig`](super::ActivateConfig) to
    /// change configuration, and this to re-apply what should already be running
    /// — for example after the kernel drifted or was started with defaults.
    ///
    /// A reload that the kernel accepts is followed by a health check, because
    /// acceptance and effect are not the same thing.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotFound`] when no version is active.
    pub async fn execute(
        ctx: &AppContext,
        _now: Timestamp,
    ) -> Result<ReloadOutcome, ApplicationError> {
        let _lock = ctx.locks.acquire(&ctx.instance).await;

        let instance = load_instance(ctx).await?;

        if !instance.status().is_serving() {
            return Err(ApplicationError::InvalidState(format!(
                "cannot reload while {}",
                instance.status().as_str()
            )));
        }

        let active = ctx
            .configs
            .active(&ctx.instance)
            .await?
            .ok_or_else(|| ApplicationError::NotFound("no active config version".to_owned()))?;
        let body = ctx.configs.read_body(&active).await?;

        let job = ctx
            .jobs
            .create(
                JobKind::MihomoReload,
                JobTarget::Config(active.id().clone()),
            )
            .await?;

        let outcome = ctx.controller.reload(ReloadRequest::Payload(body)).await?;

        if let ReloadOutcome::Rejected { http_status } = outcome {
            ctx.jobs
                .update(
                    &job,
                    JobState::Failed {
                        reason: format!("kernel rejected reload (status {http_status})"),
                    },
                )
                .await?;
            return Ok(outcome);
        }

        // Acceptance is not effect.
        let health = ctx.controller.health_check().await?;
        let degradation = if health.is_healthy() {
            None
        } else {
            Some(crate::ports::job_registry::Degradation::HealthUnconfirmed {
                reason: health.summary(),
            })
        };

        ctx.jobs
            .update(
                &job,
                JobState::Succeeded {
                    summary: format!("reloaded {}", active.id()),
                    degradation,
                },
            )
            .await?;

        Ok(outcome)
    }
}

/// Sends an allowed signal to the kernel.
///
/// Only [`AllowedSignal`] values are accepted, so the signals whose default
/// disposition is to kill the kernel cannot be sent by mistake.
///
/// # Errors
/// Returns an error when no process is running.
pub async fn signal_kernel(
    ctx: &AppContext,
    signal: AllowedSignal,
) -> Result<(), ApplicationError> {
    let handle = ctx
        .current_handle()
        .ok_or_else(|| ApplicationError::InvalidState("no kernel process is running".to_owned()))?;
    ctx.process.signal(&handle, signal).await?;
    Ok(())
}

/// The identifier this module's commands operate on.
///
/// Exposed for callers that need to key their own state by instance.
#[must_use]
pub fn instance_id(ctx: &AppContext) -> MihomoInstanceId {
    ctx.instance.clone()
}
