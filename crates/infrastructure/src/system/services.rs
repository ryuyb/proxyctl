//! Init-system observation over `systemctl`.
//!
//! # Why a subprocess rather than a D-Bus client
//!
//! `systemctl` is itself a D-Bus client, so calling it is one process heavier
//! than talking to systemd directly. It is still the right choice here: the
//! dependency baseline in `AGENTS.md` has no D-Bus crate, and this port asks only
//! three read-only questions. Adding a D-Bus stack for those would be a large
//! dependency serving a small need.
//!
//! The cost is real and is paid deliberately: `systemctl`'s output is text, so
//! this adapter parses its *exit status* wherever possible and only consults the
//! text for `is-system-running`, whose states have no exit-code distinction.
//!
//! # Presence is not capability
//!
//! `systemctl` is installed in containers that have no running manager, where
//! unit control is impossible. So the binary existing proves nothing; the
//! question is whether the manager answers, which is what
//! [`ServiceManager::supports_unit_control`] reports.

use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;

use proxy_application::ports::PortError;
use proxy_application::ports::service_manager::ServiceManager;
use proxy_domain::system::environment::InitSystem;

/// How long a `systemctl` query may take.
///
/// A hung D-Bus connection would otherwise hold a doctor run open indefinitely.
pub const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The unit the agent runs as, by convention.
pub const AGENT_UNIT: &str = "proxy-agent.service";

/// Observes the host's init system.
#[derive(Debug, Clone)]
pub struct SystemdServiceManager {
    unit: String,
    systemctl: String,
}

impl SystemdServiceManager {
    /// Creates a manager querying the conventional agent unit.
    #[must_use]
    pub fn new() -> Self {
        Self {
            unit: AGENT_UNIT.to_owned(),
            systemctl: "systemctl".to_owned(),
        }
    }

    /// Creates a manager with explicit values, for tests.
    #[must_use]
    pub fn with_paths(systemctl: impl Into<String>, unit: impl Into<String>) -> Self {
        Self {
            unit: unit.into(),
            systemctl: systemctl.into(),
        }
    }

    /// The unit this manager reports on.
    #[must_use]
    pub fn unit(&self) -> &str {
        &self.unit
    }

    /// Runs `systemctl` with `args`, returning its exit status and stdout.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Unreachable`] only when `systemctl` cannot be run at
    /// all. A non-zero exit is a normal answer and is returned, not raised — the
    /// distinction matters because "the unit is inactive" must not look like a
    /// failure to talk to systemd.
    async fn run(&self, args: &[&str]) -> Result<(bool, String), PortError> {
        let mut command = Command::new(&self.systemctl);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A container frequently has no user session, and a localized
            // `systemctl` would print translated state names. A cleared
            // environment with an explicit PATH keeps the output parseable.
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C");

        let output = match tokio::time::timeout(QUERY_TIMEOUT, command.output()).await {
            Err(_) => {
                // A timeout is its own variant: "it never answered" is a
                // different diagnosis from "it could not be started".
                return Err(PortError::Timeout(QUERY_TIMEOUT));
            }
            Ok(Err(e)) => {
                return Err(PortError::Unreachable(Box::new(e)));
            }
            Ok(Ok(output)) => output,
        };

        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    }
}

impl Default for SystemdServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ServiceManager for SystemdServiceManager {
    async fn detect(&self) -> Result<InitSystem, PortError> {
        match self.run(&["is-system-running"]).await {
            Ok((_, state)) => Ok(classify_system_state(state.trim())),
            Err(_) => {
                // The binary is missing or unrunnable. Inspect the filesystem
                // before concluding there is no init system at all.
                let has_manager = tokio::fs::metadata("/run/systemd/system").await.is_ok();
                Ok(if has_manager {
                    InitSystem::Unknown
                } else {
                    InitSystem::None
                })
            }
        }
    }

    async fn is_agent_service_active(&self) -> Result<bool, PortError> {
        // `is-active` exits zero only for `active`, so the status carries the
        // answer and the text is not consulted.
        match self.run(&["is-active", &self.unit]).await {
            Ok((success, _)) => Ok(success),
            // Without `systemctl` there is no unit to query. The port documents
            // this as the normal container case, so it is `false`, not an error.
            Err(_) => Ok(false),
        }
    }

    async fn supports_unit_control(&self) -> Result<bool, PortError> {
        // The manager must be *reachable*, not merely installed. `list-units`
        // talks to the running manager, so it succeeds only where control is
        // actually possible.
        match self.run(&["list-units", "--no-legend", "--no-pager"]).await {
            Ok((success, _)) => Ok(success),
            Err(_) => Ok(false),
        }
    }
}

/// Maps `systemctl is-system-running` output onto an init system.
///
/// Split out so every branch is unit-testable without systemd.
///
/// `degraded` is the value that matters: it means systemd is up with at least one
/// failed unit. Treating it as "no init system" would disable service management
/// on a working host, and real-machine measurement found a container reporting it
/// purely from unrelated units.
#[must_use]
pub fn classify_system_state(state: &str) -> InitSystem {
    match state.trim() {
        "running" | "degraded" | "starting" | "maintenance" | "stopping" => InitSystem::Systemd,
        // `offline` is an unfinished boot, `unknown` is not systemd at all.
        "offline" | "unknown" => InitSystem::None,
        _ => InitSystem::Unknown,
    }
}

#[cfg(test)]
#[path = "services/tests.rs"]
mod tests;
