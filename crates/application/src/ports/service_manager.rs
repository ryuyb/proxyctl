//! Init-system integration.
//!
//! Narrow on purpose. The agent is the lifecycle authority for the kernel, so
//! this port does not start or stop a kernel unit — doing so would create two
//! supervisors competing over one process. What it provides is the ability to
//! observe whether the agent itself is supervised, which decides whether a
//! missing agent process will be restarted by the host.

use async_trait::async_trait;
use proxy_domain::system::environment::InitSystem;

use crate::ports::error::PortError;

/// Reports how the host supervises services.
#[async_trait]
pub trait ServiceManager: Send + Sync {
    /// Which init system is present.
    async fn detect(&self) -> Result<InitSystem, PortError>;

    /// Whether the agent's own unit is currently active.
    ///
    /// Returns `Ok(false)` when there is no unit to query, which is the normal
    /// case inside a container without an init system.
    async fn is_agent_service_active(&self) -> Result<bool, PortError>;

    /// Whether unit control is available at all.
    ///
    /// Containers frequently have the binary but no running manager, so the
    /// presence of `systemctl` is not sufficient to conclude control is
    /// possible.
    async fn supports_unit_control(&self) -> Result<bool, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_systems_are_distinguishable() {
        assert_ne!(InitSystem::Systemd, InitSystem::None);
        assert_ne!(InitSystem::OpenRc, InitSystem::Unknown);
    }
}
