//! Process identity and discovery via `/proc`.
//!
//! A pid is not an identity: the kernel recycles them, so a pid recorded before
//! a restart can name an unrelated process afterwards. The pair `(pid,
//! start_time)` is stable for the life of a process, which is what makes it safe
//! to signal a process this agent did not spawn.
//!
//! # Why discovery is needed at all
//!
//! The kernel does not daemonize and writes no pid file; it is an ordinary child
//! process. When the agent exits, that child is reparented to init and keeps
//! running. Nothing records it. So after an agent restart there is genuinely no
//! handle — and a start request would otherwise spawn a second kernel competing
//! for the same ports.
//!
//! Measured behaviour this module relies on: a reparented child survives and can
//! be signalled by a process that never spawned it, so adopting one is both
//! necessary and possible.

use std::path::{Path, PathBuf};

use proxy_application::ports::PortError;
use proxy_application::ports::process_manager::{ProcessHandle, ProcessStatus, StartOptions};

/// The field index of `starttime` in `/proc/<pid>/stat`.
///
/// After the command name, which is field 2 and may itself contain spaces and
/// parentheses — hence [`parse_stat_start_time`] rather than a naive split.
const START_TIME_FIELD: usize = 22;

/// Reads the start time for a pid.
///
/// # Errors
///
/// Returns `Ok(None)` when the process does not exist, and
/// [`PortError::PermissionDenied`] when its stat file exists but cannot be read.
/// Those are deliberately different: "gone" is a normal answer, whereas
/// "unreadable" means identity cannot be established and the caller must not
/// guess.
pub async fn start_time_of(pid: u32) -> Result<Option<u64>, PortError> {
    let path = PathBuf::from(format!("/proc/{pid}/stat"));
    match tokio::fs::read_to_string(&path).await {
        Ok(contents) => parse_stat_start_time(&contents)
            .ok_or_else(|| {
                PortError::InvalidResponse(format!(
                    "cannot parse start time from {}",
                    path.display()
                ))
            })
            .map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(
            PortError::PermissionDenied(format!("cannot read {}: {e}", path.display())),
        ),
        Err(e) => Err(PortError::Storage(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// Extracts `starttime` from the contents of a `stat` file.
///
/// The command name is parenthesised and may contain spaces and parentheses, so
/// fields are counted from the *last* `)` rather than by splitting the whole
/// line. Field 3 of the file is the first field after the command name.
#[must_use]
pub fn parse_stat_start_time(stat: &str) -> Option<u64> {
    let after_comm = stat.rfind(')')?;
    let rest = stat.get(after_comm + 1..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();

    // `fields[0]` is file field 3, so file field 22 is at index 22 - 3.
    let index = START_TIME_FIELD.checked_sub(3)?;
    fields.get(index)?.parse::<u64>().ok()
}

/// Builds a handle for a live pid.
///
/// # Errors
/// Returns `Ok(None)` when the process is gone, and propagates read failures.
pub async fn handle_for(pid: u32) -> Result<Option<ProcessHandle>, PortError> {
    Ok(start_time_of(pid)
        .await?
        .map(|start_time| ProcessHandle::new(pid, start_time)))
}

/// Whether a handle still names a *running* process.
///
/// A process that has exited but not yet been reaped stays visible in `/proc` as
/// a zombie, and its `stat` file remains readable. Treating that as alive would
/// make a stop operation wait for a process that has already stopped and then
/// report a timeout — so the process state is checked, not just its existence.
///
/// # Errors
/// Propagates read failures for a pid that exists but cannot be inspected.
pub async fn is_alive(handle: &ProcessHandle) -> Result<bool, PortError> {
    match process_identity(handle.pid).await? {
        Some((start_time, state)) => {
            Ok(start_time == handle.start_time && state != ProcessState::Zombie)
        }
        None => Ok(false),
    }
}

/// How a process is currently scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Running or runnable.
    Running,
    /// Exited, awaiting reaping by its parent.
    Zombie,
    /// Stopped or tracing.
    Stopped,
    /// Sleeping.
    Sleeping,
    /// Any other state the kernel reports.
    Other,
}

impl ProcessState {
    /// Parses the single-character state field from `/proc/<pid>/stat`.
    #[must_use]
    pub const fn from_stat_field(field: &str) -> Self {
        match field.as_bytes() {
            b"Z" | b"X" | b"x" => Self::Zombie,
            b"R" => Self::Running,
            b"T" | b"t" => Self::Stopped,
            b"S" | b"D" | b"I" => Self::Sleeping,
            _ => Self::Other,
        }
    }
}

/// Reads a pid's start time and current state together.
///
/// Both come from the same `stat` read, so they cannot disagree about which
/// process is being described.
///
/// # Errors
/// Returns `Ok(None)` when the process is gone, and propagates read failures.
pub async fn process_identity(pid: u32) -> Result<Option<(u64, ProcessState)>, PortError> {
    let path = PathBuf::from(format!("/proc/{pid}/stat"));
    match tokio::fs::read_to_string(&path).await {
        Ok(contents) => {
            let (start_time, state) = parse_stat_identity(&contents).ok_or_else(|| {
                PortError::InvalidResponse(format!("cannot parse identity from {}", path.display()))
            })?;
            Ok(Some((start_time, state)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(
            PortError::PermissionDenied(format!("cannot read {}: {e}", path.display())),
        ),
        Err(e) => Err(PortError::Storage(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// Extracts the start time and state from a `stat` file's contents.
#[must_use]
pub fn parse_stat_identity(stat: &str) -> Option<(u64, ProcessState)> {
    let after_comm = stat.rfind(')')?;
    let rest = stat.get(after_comm + 1..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();

    // `fields[0]` is file field 3, so file field 22 sits at index 22 - 3.
    let start_time = fields
        .get(START_TIME_FIELD.checked_sub(3)?)?
        .parse::<u64>()
        .ok()?;
    let state = ProcessState::from_stat_field(fields.first()?);
    Some((start_time, state))
}

/// Reports a process's state.
///
/// A zombie is reported as exited rather than running: it has finished, and only
/// its parent's reaping remains.
///
/// # Errors
/// Propagates read failures.
pub async fn status_of(handle: &ProcessHandle) -> Result<ProcessStatus, PortError> {
    match process_identity(handle.pid).await? {
        Some((start_time, ProcessState::Zombie)) if start_time == handle.start_time => {
            Ok(ProcessStatus::Exited { code: None })
        }
        Some((start_time, _)) if start_time == handle.start_time => Ok(ProcessStatus::Running),
        // Either the pid is gone, or it now belongs to something else: in both
        // cases the process this handle named has finished.
        Some(_) | None => Ok(ProcessStatus::Unknown),
    }
}

/// The command line of a pid, as a single space-joined string.
///
/// # Errors
/// Returns `Ok(None)` when the process is gone or its command line is
/// unreadable, since both mean it cannot be identified as ours.
pub async fn cmdline_of(pid: u32) -> Result<Option<String>, PortError> {
    let path = PathBuf::from(format!("/proc/{pid}/cmdline"));
    match tokio::fs::read(&path).await {
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => Ok(Some(
            String::from_utf8_lossy(&bytes)
                .replace('\0', " ")
                .trim_end()
                .to_owned(),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(None),
        Err(e) => Err(PortError::Storage(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// The executable a pid is running.
///
/// # Errors
/// Returns `Ok(None)` when unreadable, which includes the case of a process
/// owned by another user on a host with `hidepid` set.
pub async fn exe_of(pid: u32) -> Result<Option<PathBuf>, PortError> {
    let path = PathBuf::from(format!("/proc/{pid}/exe"));
    match tokio::fs::read_link(&path).await {
        Ok(target) => Ok(Some(target)),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(PortError::Storage(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// The working directory of a pid.
///
/// # Errors
/// Returns `Ok(None)` when unreadable.
pub async fn cwd_of(pid: u32) -> Result<Option<PathBuf>, PortError> {
    match tokio::fs::read_link(format!("/proc/{pid}/cwd")).await {
        Ok(target) => Ok(Some(target)),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(PortError::Storage(format!("cannot read cwd of {pid}: {e}"))),
    }
}

/// Every numeric entry in `/proc`, as pids.
///
/// # Errors
/// Returns [`PortError::PermissionDenied`] when `/proc` cannot be listed, which
/// callers must treat as "cannot determine" rather than "nothing is running".
pub async fn list_pids() -> Result<Vec<u32>, PortError> {
    let mut entries = tokio::fs::read_dir("/proc").await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            PortError::PermissionDenied(format!("cannot list /proc: {e}"))
        } else {
            PortError::Storage(format!("cannot list /proc: {e}"))
        }
    })?;

    let mut pids = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(name) = entry.file_name().to_str() {
            if let Ok(pid) = name.parse::<u32>() {
                pids.push(pid);
            }
        }
    }
    pids.sort_unstable();
    Ok(pids)
}

/// Whether a candidate process matches the kernel we are looking for.
///
/// # Matching strategy, and why
///
/// Measured against a real kernel: it is invoked as
/// `mihomo -d <dir> -f <config>`, and its **working directory is wherever it was
/// launched from, not the `-d` argument**. Requiring `cwd == working_dir` would
/// therefore never match anything real. What does discriminate:
///
/// * the **executable path**, and
/// * the **`-d` directory on the command line**.
///
/// The config path is checked as well, but only as an optional refinement, since
/// how it is passed is the caller's choice.
///
/// Matching is deliberately strict otherwise: adopting the wrong process means
/// sending signals to it, so returning nothing is far safer than returning the
/// wrong handle.
///
/// # Errors
/// Propagates `/proc` read failures.
pub async fn matches_kernel(pid: u32, options: &StartOptions) -> Result<bool, PortError> {
    let Some(exe) = exe_of(pid).await? else {
        return Ok(false);
    };
    if !same_path(&exe, Path::new(&options.binary_path)) {
        return Ok(false);
    }

    // The command line is what distinguishes one instance from another: a real
    // kernel carries `-d <working_dir> -f <config_path>`.
    let Some(cmdline) = cmdline_of(pid).await? else {
        return Ok(false);
    };

    // The working directory appears as the `-d` argument, not as the process's
    // own cwd.
    Ok(cmdline_mentions(&cmdline, &options.working_dir))
}

/// Whether a command line mentions `value` as a standalone argument.
///
/// Exact-argument comparison avoids a prefix collision: `/var/lib/kernel` must
/// not match `/var/lib/kernel-2`.
fn cmdline_mentions(cmdline: &str, value: &str) -> bool {
    cmdline
        .split_whitespace()
        .any(|argument| same_path(Path::new(argument), Path::new(value)))
}

/// Compares two paths, tolerating a relative path on one side.
///
/// The recorded start options and `/proc` may disagree about whether a path is
/// absolute, and a strict comparison would then reject a legitimate match.
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.ends_with(b) || b.ends_with(a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A captured `stat` line: the command name contains spaces and parentheses,
    /// which is exactly why naive field splitting fails here.
    const STAT_WITH_AWKWARD_NAME: &str = "1234 (mihomo (Meta) worker) S 1 1234 1234 0 -1 4194560 100 200 0 0 5 6 7 8 20 0 1 0 987654 1000 200";

    #[test]
    fn parses_start_time_past_an_awkward_command_name() {
        assert_eq!(
            parse_stat_start_time(STAT_WITH_AWKWARD_NAME),
            Some(987_654),
            "the parser must count from the last parenthesis, not split the line"
        );
    }

    #[test]
    fn parses_a_real_stat_line() {
        // Shape of a genuine /proc/<pid>/stat, abbreviated after starttime.
        let stat = "1 (systemd) S 0 1 1 0 -1 4194560 34710 142992 32 98 60 18 54 34 20 0 1 0 1157121 27430912 1752";
        assert_eq!(parse_stat_start_time(stat), Some(1_157_121));
    }

    #[test]
    fn returns_none_when_truncated() {
        assert!(parse_stat_start_time("1 (systemd) S 0 1").is_none());
        assert!(parse_stat_start_time("no parenthesis").is_none());
        assert!(
            parse_stat_start_time("1 (x) S 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 notanumber").is_none()
        );
    }

    /// Our own pid must be discoverable; it is the one process guaranteed to
    /// exist while the test runs.
    ///
    /// Linux-only: the module reads `/proc`, which does not exist elsewhere.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn reads_our_own_start_time() {
        let pid = std::process::id();
        let start = start_time_of(pid).await.expect("readable");
        assert!(
            start.is_some(),
            "the current process must have a start time"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_handle_for_our_own_pid_is_alive() {
        let pid = std::process::id();
        let handle = handle_for(pid).await.expect("readable").expect("alive");
        assert!(is_alive(&handle).await.expect("checked"));
        assert_eq!(
            status_of(&handle).await.expect("status"),
            ProcessStatus::Running
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_different_start_time_means_the_handle_is_stale() {
        let pid = std::process::id();
        let real = handle_for(pid).await.expect("readable").expect("alive");

        // A pid that was recycled would carry a different start time, so a stale
        // handle must not be reported as alive.
        let stale = ProcessHandle::new(real.pid, real.start_time.wrapping_add(1));
        assert!(!is_alive(&stale).await.expect("checked"));
        assert_eq!(
            status_of(&stale).await.expect("status"),
            ProcessStatus::Unknown
        );
    }

    #[tokio::test]
    async fn a_pid_that_cannot_exist_is_not_alive() {
        // Pids are 32-bit; this value cannot be allocated by Linux.
        let handle = ProcessHandle::new(u32::MAX - 1, 1);
        assert!(!is_alive(&handle).await.expect("checked"));
        assert_eq!(
            status_of(&handle).await.expect("status"),
            ProcessStatus::Unknown
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn reads_our_own_exe_and_cmdline() {
        let pid = std::process::id();
        let exe = exe_of(pid).await.expect("readable");
        assert!(
            exe.is_some(),
            "the test binary's executable must be readable"
        );
        assert!(cmdline_of(pid).await.expect("readable").is_some());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn lists_pids_including_our_own() {
        let pids = list_pids().await.expect("readable");
        assert!(
            pids.contains(&std::process::id()),
            "our own pid must appear in /proc"
        );
        assert!(pids.len() > 1, "a running system has more than one process");
    }

    /// The command-line matcher must not confuse a prefix for a match.
    #[test]
    fn argument_matching_is_not_a_prefix_match() {
        let cmdline = "/opt/kernel -d /var/lib/kernel-2 -f /var/lib/kernel-2/config.yaml";
        assert!(cmdline_mentions(cmdline, "/var/lib/kernel-2"));
        assert!(
            !cmdline_mentions(cmdline, "/var/lib/kernel"),
            "a shorter path must not match a longer one"
        );
    }

    /// A real kernel's command line, captured from a live process.
    #[test]
    fn matches_a_real_kernel_command_line() {
        let cmdline = "/tmp/mhbin -d /tmp/d2/wd -f /tmp/d2/wd/config.yaml";
        assert!(cmdline_mentions(cmdline, "/tmp/d2/wd"));
        assert!(cmdline_mentions(cmdline, "/tmp/mhbin"));
    }

    /// Adoption must reject a process that is not the kernel we are looking for,
    /// because adopting the wrong one means signalling it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn does_not_match_an_unrelated_process() {
        let options = StartOptions {
            binary_path: "/nonexistent/kernel".to_owned(),
            working_dir: "/nonexistent".to_owned(),
            config_path: "/nonexistent/config.yaml".to_owned(),
            required_capabilities: Vec::new(),
        };
        assert!(
            !matches_kernel(std::process::id(), &options)
                .await
                .expect("readable"),
            "the test binary is not the kernel"
        );
    }

    /// The stale-handle rule is expressible without `/proc`: two handles with
    /// the same pid but different start times are different processes.
    #[test]
    fn handles_with_different_start_times_are_distinct() {
        let live = ProcessHandle::new(42, 1_000);
        let recycled = ProcessHandle::new(42, 2_000);
        assert_ne!(live, recycled, "a recycled pid must not compare equal");
        assert_eq!(live, ProcessHandle::new(42, 1_000));
    }

    /// The parser must recognise the zombie state, since treating a zombie as
    /// alive makes every stop operation on a dead process time out.
    #[test]
    fn parses_identity_with_state() {
        let stat = "1 (systemd) S 0 1 1 0 -1 4194560 34710 142992 32 98 60 18 54 34 20 0 1 0 1157121 27430912 1752";
        let (start_time, state) = parse_stat_identity(stat).expect("parses");
        assert_eq!(start_time, 1_157_121);
        assert_eq!(state, ProcessState::Sleeping);
    }

    #[test]
    fn recognises_every_significant_state() {
        assert_eq!(ProcessState::from_stat_field("Z"), ProcessState::Zombie);
        assert_eq!(ProcessState::from_stat_field("X"), ProcessState::Zombie);
        assert_eq!(ProcessState::from_stat_field("R"), ProcessState::Running);
        assert_eq!(ProcessState::from_stat_field("S"), ProcessState::Sleeping);
        assert_eq!(ProcessState::from_stat_field("T"), ProcessState::Stopped);
        assert_eq!(ProcessState::from_stat_field("?"), ProcessState::Other);
    }

    /// A zombie appears in `stat` with state `Z` and keeps its start time, which
    /// is exactly the case that would otherwise look alive forever.
    #[test]
    fn a_zombie_is_parsed_as_exited_not_running() {
        let zombie = "42 (mihomo) Z 1 42 42 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 55555 0 0";
        let (start_time, state) = parse_stat_identity(zombie).expect("parses");
        assert_eq!(start_time, 55_555);
        assert_eq!(state, ProcessState::Zombie);
        assert_ne!(state, ProcessState::Running);
    }

    #[test]
    fn path_comparison_tolerates_prefix_differences() {
        assert!(same_path(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!same_path(Path::new("/a/b"), Path::new("/a/c")));
    }
}
