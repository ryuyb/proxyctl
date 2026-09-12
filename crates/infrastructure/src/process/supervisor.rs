//! Child-process supervision.
//!
//! The agent is the lifecycle authority: it spawns the kernel, signals it, and
//! reaps it. systemd supervises the *agent*, not the kernel, so there is exactly
//! one supervisor per process and no competing restart decisions.
//!
//! # Adoption
//!
//! The kernel outlives the agent. It does not daemonize and writes no pid file,
//! so when the agent exits its child is reparented and nothing records it. A
//! restarted agent therefore has no handle, and a naive start request would spawn
//! a second kernel fighting for the same ports.
//!
//! [`SupervisedChildProcess::discover`] closes that gap by locating a running
//! kernel from the start options. The handle it returns carries a start time, so
//! a recycled pid cannot be mistaken for the kernel later.
//!
//! # Reaping
//!
//! Only a process this agent spawned can become a zombie *of this agent*. Those
//! are reaped with `wait`; an adopted process belongs to init, so there is
//! nothing to reap and waiting on it is impossible.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use proxy_application::ports::PortError;
use proxy_application::ports::process_manager::{
    AllowedSignal, ExitStatus, ProcessHandle, ProcessManager, ProcessStatus, StartOptions,
};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::procinfo;

/// How long to wait after `SIGTERM` before escalating to `SIGKILL`.
///
/// The kernel stops in well under a second when idle, so this is generous; it
/// exists so a kernel wedged on a stuck socket cannot hold shutdown forever.
pub const DEFAULT_GRACEFUL_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the stop loop checks whether the process has exited.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Supervises the kernel as a child process.
pub struct SupervisedChildProcess {
    /// Children this agent spawned, so they can be reaped.
    ///
    /// An adopted process is absent here deliberately: it is not our child, so
    /// there is nothing to reap and waiting would fail.
    children: Mutex<HashMap<u32, Child>>,
}

impl Default for SupervisedChildProcess {
    fn default() -> Self {
        Self::new()
    }
}

impl SupervisedChildProcess {
    /// Creates a supervisor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
        }
    }

    /// Reaps a child if this agent spawned it, without blocking on an adopted one.
    async fn reap(&self, pid: u32) -> Option<std::process::ExitStatus> {
        let child = {
            let mut children = self.children.lock().await;
            children.remove(&pid)
        };
        match child {
            Some(mut child) => child.wait().await.ok(),
            None => None,
        }
    }

    /// Waits until the process is gone, or the deadline passes.
    ///
    /// Liveness is read from `/proc` rather than from a `Child`, so this works
    /// for adopted processes too.
    async fn wait_until_gone(&self, handle: &ProcessHandle, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match procinfo::is_alive(handle).await {
                Ok(false) => return true,
                // An unreadable /proc is not evidence the process is gone, so
                // keep waiting rather than reporting a successful stop.
                Err(_) => {}
                Ok(true) => {}
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Resolves a graceful timeout for a stop call.
    fn effective_timeout(timeout: Duration) -> Duration {
        if timeout.is_zero() {
            DEFAULT_GRACEFUL_TIMEOUT
        } else {
            timeout
        }
    }
}

/// Sends a signal to a pid.
///
/// `ESRCH` means the process is already gone, which is the outcome the caller
/// wanted, so it is reported as success rather than as a failure.
fn send_signal(pid: u32, signal: AllowedSignal) -> Result<(), PortError> {
    let signal = match signal {
        AllowedSignal::Term => nix::sys::signal::Signal::SIGTERM,
        AllowedSignal::Kill => nix::sys::signal::Signal::SIGKILL,
    };

    // A pid never exceeds `i32::MAX`, so this conversion cannot lose information.
    let Ok(pid) = i32::try_from(pid) else {
        return Err(PortError::InvalidResponse(format!(
            "pid {pid} is out of range"
        )));
    };

    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal) {
        Ok(()) => Ok(()),
        Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(e) => Err(PortError::PermissionDenied(format!(
            "cannot signal pid {pid}: {e}"
        ))),
    }
}

#[async_trait]
impl ProcessManager for SupervisedChildProcess {
    async fn start(&self, options: &StartOptions) -> Result<ProcessHandle, PortError> {
        // Fail with a named reason rather than a generic spawn error, since both
        // are configuration mistakes an operator can fix.
        if !std::path::Path::new(&options.binary_path).exists() {
            return Err(PortError::Storage(format!(
                "kernel binary not found: {}",
                options.binary_path
            )));
        }
        if !std::path::Path::new(&options.working_dir).is_dir() {
            return Err(PortError::Storage(format!(
                "working directory not found: {}",
                options.working_dir
            )));
        }

        // The kernel reads its configuration from a file at startup; the agent
        // passes the working directory and the file separately, matching how the
        // kernel expects to be invoked.
        let child = Command::new(&options.binary_path)
            .arg("-d")
            .arg(&options.working_dir)
            .arg("-f")
            .arg(&options.config_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::PermissionDenied => PortError::PermissionDenied(format!(
                    "cannot execute {}: {e}",
                    options.binary_path
                )),
                _ => PortError::Storage(format!("cannot spawn {}: {e}", options.binary_path)),
            })?;

        let pid = child
            .id()
            .ok_or_else(|| PortError::Storage("spawned process has no id".to_owned()))?;

        // Read the start time immediately: it is the identity that survives an
        // agent restart, and a handle without one cannot be verified later.
        let start_time = procinfo::start_time_of(pid).await?.ok_or_else(|| {
            PortError::Storage(format!("pid {pid} vanished immediately after spawn"))
        })?;

        self.children.lock().await.insert(pid, child);

        Ok(ProcessHandle::new(pid, start_time))
    }

    async fn stop(
        &self,
        handle: &ProcessHandle,
        timeout: Duration,
    ) -> Result<ExitStatus, PortError> {
        // Already gone is success: a caller recovering from an earlier failure
        // should not have to distinguish the cases.
        if !procinfo::is_alive(handle).await? {
            self.reap(handle.pid).await;
            return Ok(ExitStatus {
                code: None,
                forced: false,
            });
        }

        let timeout = Self::effective_timeout(timeout);

        // Ask politely first, so the kernel can flush state.
        send_signal(handle.pid, AllowedSignal::Term)?;

        if self.wait_until_gone(handle, timeout).await {
            let status = self.reap(handle.pid).await;
            return Ok(ExitStatus {
                code: status.and_then(|s| s.code()),
                forced: false,
            });
        }

        // Escalate. `SIGKILL` cannot be blocked by the kernel.
        send_signal(handle.pid, AllowedSignal::Kill)?;
        let gone = self.wait_until_gone(handle, DEFAULT_GRACEFUL_TIMEOUT).await;
        let status = self.reap(handle.pid).await;

        if !gone {
            return Err(PortError::Timeout(timeout + DEFAULT_GRACEFUL_TIMEOUT));
        }

        Ok(ExitStatus {
            code: status.and_then(|s| s.code()),
            forced: true,
        })
    }

    async fn status(&self, handle: &ProcessHandle) -> Result<ProcessStatus, PortError> {
        procinfo::status_of(handle).await
    }

    async fn signal(&self, handle: &ProcessHandle, signal: AllowedSignal) -> Result<(), PortError> {
        // Verify identity before signalling: a recycled pid would otherwise
        // receive a signal meant for the kernel.
        if !procinfo::is_alive(handle).await? {
            return Err(PortError::InvalidResponse(format!(
                "pid {} is not the process this handle refers to",
                handle.pid
            )));
        }
        send_signal(handle.pid, signal)
    }

    async fn discover(&self, options: &StartOptions) -> Result<Option<ProcessHandle>, PortError> {
        let pids = procinfo::list_pids().await?;

        for pid in pids {
            // Skip our own children: they are already tracked and would be found
            // again only to be adopted pointlessly.
            if self.children.lock().await.contains_key(&pid) {
                continue;
            }
            if procinfo::matches_kernel(pid, options).await? {
                if let Some(handle) = procinfo::handle_for(pid).await? {
                    return Ok(Some(handle));
                }
            }
        }

        Ok(None)
    }

    async fn is_alive(&self, handle: &ProcessHandle) -> Result<bool, PortError> {
        procinfo::is_alive(handle).await
    }
}

/// A supervisor shared between callers.
pub type SharedSupervisor = Arc<SupervisedChildProcess>;

#[cfg(test)]
mod tests {
    use super::*;

    fn options_for(binary: &str, dir: &str, config: &str) -> StartOptions {
        StartOptions {
            binary_path: binary.to_owned(),
            working_dir: dir.to_owned(),
            config_path: config.to_owned(),
            required_capabilities: Vec::new(),
        }
    }

    /// The pre-flight checks read the filesystem only, so they hold anywhere.
    #[tokio::test]
    async fn starting_a_missing_binary_reports_storage_not_a_bare_error() {
        let supervisor = SupervisedChildProcess::new();
        let options = options_for("/nonexistent/kernel", "/tmp", "/tmp/config.yaml");

        let err = supervisor
            .start(&options)
            .await
            .expect_err("missing binary");

        assert!(matches!(err, PortError::Storage(_)));
        assert!(err.to_string().contains("not found"), "got {err}");
    }

    /// The pre-flight checks run before any spawn, so a bad directory is caught
    /// without depending on `/proc` or on a specific binary existing.
    #[tokio::test]
    async fn starting_with_a_missing_working_directory_is_rejected() {
        let supervisor = SupervisedChildProcess::new();

        // Borrow a binary that certainly exists on the host under test, so the
        // only problem is the working directory.
        let Some(existing_binary) = std::env::current_exe().ok().filter(|path| path.exists())
        else {
            return;
        };
        let options = options_for(
            &existing_binary.to_string_lossy(),
            "/nonexistent/dir",
            "/tmp/config.yaml",
        );

        let err = supervisor.start(&options).await.expect_err("bad directory");
        assert!(
            err.to_string().contains("working directory"),
            "the working directory should be the reported problem, got {err}"
        );
    }

    #[tokio::test]
    async fn stopping_an_unknown_process_succeeds() {
        let supervisor = SupervisedChildProcess::new();

        // Nothing was ever spawned with this pid, so stopping it is a no-op that
        // reports success.
        let handle = ProcessHandle::new(u32::MAX - 1, 1);
        let status = supervisor
            .stop(&handle, Duration::from_millis(50))
            .await
            .expect("stopping a gone process succeeds");

        assert!(!status.forced);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn signalling_a_stale_handle_is_refused() {
        let supervisor = SupervisedChildProcess::new();
        let pid = std::process::id();
        let real = procinfo::handle_for(pid)
            .await
            .expect("readable")
            .expect("alive");

        // Same pid, different start time: a recycled pid.
        let stale = ProcessHandle::new(real.pid, real.start_time.wrapping_add(1));
        let err = supervisor
            .signal(&stale, AllowedSignal::Term)
            .await
            .expect_err("must refuse to signal a recycled pid");

        assert!(matches!(err, PortError::InvalidResponse(_)));
        assert!(err.to_string().contains("not the process"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn is_alive_uses_the_start_time_not_just_the_pid() {
        let supervisor = SupervisedChildProcess::new();
        let pid = std::process::id();
        let real = procinfo::handle_for(pid)
            .await
            .expect("readable")
            .expect("alive");

        assert!(supervisor.is_alive(&real).await.expect("checked"));
        assert!(
            !supervisor
                .is_alive(&ProcessHandle::new(
                    real.pid,
                    real.start_time.wrapping_add(1)
                ))
                .await
                .expect("checked")
        );
    }

    /// Discovery must decline to adopt an unrelated process, since adopting the
    /// wrong one means signalling it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn discovery_declines_a_process_that_is_not_the_kernel() {
        let supervisor = SupervisedChildProcess::new();
        let options = options_for(
            "/nonexistent/kernel",
            "/nonexistent",
            "/nonexistent/config.yaml",
        );

        let found = supervisor.discover(&options).await.expect("discovery runs");
        assert!(found.is_none(), "an unrelated process must not be adopted");
    }

    #[test]
    fn a_zero_timeout_gets_a_default_rather_than_hanging_or_skipping() {
        assert_eq!(
            SupervisedChildProcess::effective_timeout(Duration::ZERO),
            DEFAULT_GRACEFUL_TIMEOUT
        );
        assert_eq!(
            SupervisedChildProcess::effective_timeout(Duration::from_secs(3)),
            Duration::from_secs(3)
        );
    }
}
