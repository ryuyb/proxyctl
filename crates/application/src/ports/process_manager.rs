//! Process supervision.
//!
//! This port manages *the agent's own child process*. The lifecycle authority is
//! the agent itself: the kernel is spawned, stopped, and signalled through this
//! trait, and systemd supervises the agent rather than the kernel.
//!
//! Managing a kernel that lives in its own unit is a different capability, see
//! [`ServiceManager`](super::service_manager).

use std::time::Duration;

use async_trait::async_trait;
use proxy_domain::system::capability::CapabilityKind;

use crate::ports::error::PortError;

/// A signal the agent is permitted to send.
///
/// Deliberately closed. The kernel only handles `SIGTERM`/`SIGINT` for graceful
/// shutdown and `SIGHUP` for reload; `SIGUSR1` and `SIGUSR2` are not registered,
/// so their default disposition kills the process. Making the signal a closed
/// enum means that mistake cannot be expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedSignal {
    /// Request graceful shutdown.
    Term,
    /// Force termination after a graceful stop timed out.
    Kill,
}

/// How to launch the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOptions {
    /// Absolute path to the kernel binary.
    pub binary_path: String,
    /// Working directory (`-d`). Must exist and be writable.
    pub working_dir: String,
    /// Configuration file the kernel reads at startup.
    pub config_path: String,
    /// Capabilities the kernel is expected to have.
    ///
    /// Declarative only: privileges are granted by the supervisor (ambient
    /// capabilities in the unit file). The agent must not attempt to raise them
    /// at runtime.
    pub required_capabilities: Vec<CapabilityKind>,
}

/// A handle to a spawned process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessHandle {
    /// Operating-system process id.
    pub pid: u32,
}

/// Whether the process is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Running.
    Running,
    /// Exited normally.
    Exited {
        /// Exit code, or `None` when terminated by a signal.
        code: Option<i32>,
    },
    /// Terminated by a signal.
    Signalled {
        /// Signal number.
        signal: i32,
    },
    /// Not found; it may never have started or was reaped already.
    Unknown,
}

/// How the process finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    /// Exit code, or `None` when terminated by a signal.
    pub code: Option<i32>,
    /// Whether the stop required a forced kill.
    pub forced: bool,
}

/// Spawns and controls the kernel process.
#[async_trait]
pub trait ProcessManager: Send + Sync {
    /// Spawn the kernel.
    ///
    /// # Errors
    /// Returns [`PortError::Storage`] when the binary or working directory is
    /// missing, and [`PortError::PermissionDenied`] when the binary is not
    /// executable.
    async fn start(&self, options: &StartOptions) -> Result<ProcessHandle, PortError>;

    /// Stop the process, escalating to a kill after `timeout`.
    ///
    /// Stopping an already-exited process is success, so a caller recovering
    /// from an earlier failure does not have to distinguish the cases.
    async fn stop(
        &self,
        handle: &ProcessHandle,
        timeout: Duration,
    ) -> Result<ExitStatus, PortError>;

    /// Query the process state.
    async fn status(&self, handle: &ProcessHandle) -> Result<ProcessStatus, PortError>;

    /// Send an allowed signal.
    async fn signal(&self, handle: &ProcessHandle, signal: AllowedSignal) -> Result<(), PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_term_and_kill_are_expressible() {
        // Compile-time guarantee: there is no variant for SIGUSR1/SIGHUP.
        let signals = [AllowedSignal::Term, AllowedSignal::Kill];
        assert_eq!(signals.len(), 2);
    }

    #[test]
    fn process_handle_is_copyable_for_reuse() {
        let handle = ProcessHandle { pid: 42 };
        let copy = handle;
        assert_eq!(handle.pid, copy.pid);
    }

    #[test]
    fn status_distinguishes_exit_from_signal() {
        assert_ne!(
            ProcessStatus::Exited { code: Some(0) },
            ProcessStatus::Signalled { signal: 9 }
        );
        assert_ne!(ProcessStatus::Running, ProcessStatus::Unknown);
    }

    #[test]
    fn exit_status_records_forced_kill() {
        let graceful = ExitStatus {
            code: Some(0),
            forced: false,
        };
        let forced = ExitStatus {
            code: None,
            forced: true,
        };
        assert!(!graceful.forced);
        assert!(forced.forced);
    }

    #[test]
    fn start_options_are_declarative() {
        let options = StartOptions {
            binary_path: "/opt/proxy-agent/bin/mihomo".into(),
            working_dir: "/var/lib/proxy-agent/mihomo".into(),
            config_path: "/var/lib/proxy-agent/configs/v001.yaml".into(),
            required_capabilities: vec![CapabilityKind::NetAdmin],
        };
        assert_eq!(options.required_capabilities.len(), 1);
    }
}
