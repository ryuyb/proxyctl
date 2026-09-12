//! Unix socket permissions.
//!
//! The kernel creates its controller socket mode `0666` and does not check the
//! secret over that transport, so anything on the host that can reach the socket
//! file has full control of the kernel — including endpoints that restart it.
//! File permissions are therefore the entire access-control boundary, and the
//! kernel does not help establish them.
//!
//! Measured behaviour this module relies on:
//!
//! * The socket is always created `0666`, on every start. Tightening is
//!   therefore not a one-time setup step; a restart undoes it.
//! * The kernel creates the parent directory `0755` **only when it is absent**.
//!   A pre-created directory keeps its own mode, which is how the directory can
//!   be restricted before the socket exists.

use std::path::{Path, PathBuf};

use proxy_application::ports::PortError;

/// Permissions the controller socket should end up with.
///
/// Group-accessible rather than owner-only, so a dedicated client group can use
/// it while unprivileged users cannot. Written as a constant because the value
/// is part of the security model, not a tunable.
pub const SOCKET_MODE: u32 = 0o660;

/// Permissions for the directory holding the socket.
///
/// Owner-only traversal: without execute permission on the directory, other
/// users cannot reach the socket regardless of the socket's own mode.
pub const DIRECTORY_MODE: u32 = 0o750;

/// What was observed about a socket's permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketPermissions {
    /// The socket does not exist yet, which is normal before a first start.
    Absent,
    /// The socket has the expected mode.
    Correct {
        /// The observed mode.
        mode: u32,
    },
    /// The socket exists with a mode that grants more access than intended.
    TooPermissive {
        /// The observed mode.
        mode: u32,
        /// The mode it should be.
        expected: u32,
    },
}

impl SocketPermissions {
    /// Whether the socket is safe to use.
    #[must_use]
    pub const fn is_safe(&self) -> bool {
        matches!(self, Self::Correct { .. } | Self::Absent)
    }
}

/// Creates the parent directory with restrictive permissions if needed.
///
/// # Errors
///
/// Returns [`PortError::Storage`] when the directory cannot be created, and
/// [`PortError::PermissionDenied`] when it cannot be made owner-only. Failing
/// here is preferable to creating a world-traversable directory and tightening
/// it later, because the socket would be reachable in between.
pub async fn ensure_directory(path: &Path) -> Result<(), PortError> {
    if !path.exists() {
        tokio::fs::create_dir_all(path)
            .await
            .map_err(|e| PortError::Storage(format!("cannot create {}: {e}", path.display())))?;
    }
    set_mode(path, DIRECTORY_MODE).await
}

/// Tightens an existing socket to the expected mode.
///
/// # Errors
///
/// Returns [`PortError::PermissionDenied`] when the mode cannot be set. The
/// caller decides whether that is fatal; see
/// [`SocketPermissions::TooPermissive`] for the observed state it should report.
pub async fn tighten_socket(path: &Path) -> Result<(), PortError> {
    set_mode(path, SOCKET_MODE).await
}

/// Reads a socket's current permissions.
///
/// # Errors
///
/// Returns [`PortError::Storage`] when the path cannot be inspected for a reason
/// other than absence.
pub async fn inspect(path: &Path) -> Result<SocketPermissions, PortError> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => {
            let mode = mode_of(&metadata);
            if mode & !SOCKET_MODE == 0 {
                Ok(SocketPermissions::Correct { mode })
            } else {
                Ok(SocketPermissions::TooPermissive {
                    mode,
                    expected: SOCKET_MODE,
                })
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SocketPermissions::Absent),
        Err(e) => Err(PortError::Storage(format!(
            "cannot inspect {}: {e}",
            path.display()
        ))),
    }
}

/// Ensures the socket is as tight as intended, reporting what was found.
///
/// Returns the observed state after attempting to tighten, so a caller can
/// surface a still-permissive socket rather than assuming success.
///
/// # Errors
///
/// Returns [`PortError::Storage`] when the path cannot be inspected.
pub async fn enforce(path: &Path) -> Result<SocketPermissions, PortError> {
    let observed = inspect(path).await?;
    if observed.is_safe() {
        return Ok(observed);
    }

    // Tighten and re-read rather than assuming the chmod took effect.
    if tighten_socket(path).await.is_ok() {
        inspect(path).await
    } else {
        Ok(observed)
    }
}

/// Extracts the permission bits from metadata.
#[cfg(unix)]
fn mode_of(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

/// Extracts the permission bits from metadata.
///
/// On non-unix platforms there is no mode, so the socket is reported as correct:
/// this adapter is only used on Linux, and reporting a spurious problem on a
/// development host would be noise rather than signal.
#[cfg(not(unix))]
fn mode_of(_metadata: &std::fs::Metadata) -> u32 {
    SOCKET_MODE
}

/// Sets a path's permission bits.
#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) -> Result<(), PortError> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|e| PortError::PermissionDenied(format!("cannot chmod {}: {e}", path.display())))
}

/// Sets a path's permission bits.
#[cfg(not(unix))]
async fn set_mode(_path: &Path, _mode: u32) -> Result<(), PortError> {
    Ok(())
}

/// The default socket path for a system install.
#[must_use]
pub fn default_socket_path() -> PathBuf {
    PathBuf::from("/run/proxy-agent/mihomo.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_modes_are_restrictive() {
        assert_eq!(SOCKET_MODE, 0o660, "group access only, never world");
        assert_eq!(DIRECTORY_MODE, 0o750, "owner traversal only");
    }

    #[test]
    fn absent_socket_is_safe() {
        assert!(SocketPermissions::Absent.is_safe());
    }

    #[test]
    fn correct_socket_is_safe() {
        assert!(SocketPermissions::Correct { mode: 0o660 }.is_safe());
        assert!(SocketPermissions::Correct { mode: 0o600 }.is_safe());
    }

    /// The kernel's default, which this module exists to remove.
    #[test]
    fn world_writable_socket_is_not_safe() {
        let observed = SocketPermissions::TooPermissive {
            mode: 0o666,
            expected: 0o660,
        };
        assert!(!observed.is_safe());
    }

    #[tokio::test]
    async fn absent_path_is_reported_as_absent() {
        let dir = tempdir();
        let missing = dir.join("nothing.sock");
        assert_eq!(
            inspect(&missing).await.expect("inspectable"),
            SocketPermissions::Absent
        );
    }

    #[tokio::test]
    async fn directory_is_created_with_restrictive_mode() {
        let dir = tempdir();
        let run_dir = dir.join("run");

        ensure_directory(&run_dir).await.expect("directory created");

        assert!(run_dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&run_dir)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode, DIRECTORY_MODE,
                "the socket directory must be owner-only"
            );
        }
    }

    /// The kernel's own default must be detected as unsafe.
    #[tokio::test]
    async fn permissive_socket_is_detected_and_tightened() {
        let dir = tempdir();
        let sock = dir.join("mihomo.sock");
        std::fs::write(&sock, b"").expect("placeholder");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o666))
                .expect("set permissive");

            let observed = inspect(&sock).await.expect("inspectable");
            assert!(
                matches!(
                    observed,
                    SocketPermissions::TooPermissive { mode: 0o666, .. }
                ),
                "a world-writable socket must be reported, got {observed:?}"
            );

            let after = enforce(&sock).await.expect("enforced");
            assert!(after.is_safe(), "tightening must reach a safe state");
            let mode = std::fs::metadata(&sock)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, SOCKET_MODE);
        }
    }

    /// Re-running enforcement must be idempotent, since it happens on every
    /// start and every health check.
    #[tokio::test]
    async fn enforcement_is_idempotent() {
        let dir = tempdir();
        let sock = dir.join("mihomo.sock");
        std::fs::write(&sock, b"").expect("placeholder");

        let first = enforce(&sock).await.expect("first");
        let second = enforce(&sock).await.expect("second");
        assert_eq!(first, second);
    }

    fn tempdir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "proxyctl-socket-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }
}
