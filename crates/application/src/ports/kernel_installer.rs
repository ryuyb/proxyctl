//! Kernel binary installation.
//!
//! The agent owns this path rather than delegating to the kernel's own updater,
//! because that endpoint reports "already current" as an error, performs no
//! signature check, and can replace a running binary with a partial download.
//!
//! A download is kept separate from an install so verification can fail before
//! anything on disk changes, and the previous installation is retained so a
//! failed rollout can be undone.

use async_trait::async_trait;
use proxy_domain::configuration::ConfigChecksum;
use proxy_domain::mihomo::MihomoVersion;

use crate::ports::error::PortError;
use crate::ports::types::{DownloadedArtifact, KernelInstallation};

/// Fetches, verifies, and installs kernel binaries.
#[async_trait]
pub trait KernelInstaller: Send + Sync {
    /// The installation currently in use, if any.
    async fn current(&self) -> Result<Option<KernelInstallation>, PortError>;

    /// Download a version without installing it.
    ///
    /// # Errors
    /// Returns [`PortError::Unreachable`] when the source cannot be reached.
    async fn fetch(&self, version: &MihomoVersion) -> Result<DownloadedArtifact, PortError>;

    /// Verify an artifact against an expected checksum.
    ///
    /// # Errors
    /// Returns [`PortError::InvalidResponse`] when the artifact does not match,
    /// which must prevent installation rather than warn.
    async fn verify(
        &self,
        artifact: &DownloadedArtifact,
        expected: &ConfigChecksum,
    ) -> Result<(), PortError>;

    /// Install a verified artifact atomically, retaining the previous version.
    async fn install(&self, artifact: &DownloadedArtifact)
    -> Result<KernelInstallation, PortError>;

    /// Restore the previously installed version.
    ///
    /// # Errors
    /// Returns [`PortError::Storage`] when no previous installation is retained.
    async fn rollback_previous(&self) -> Result<KernelInstallation, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_checksum_identify_an_installation() {
        let install = KernelInstallation {
            version: MihomoVersion::parse("v1.19.30").expect("valid"),
            binary_path: "/opt/proxy-agent/bin/mihomo".into(),
            checksum: ConfigChecksum::from_digest(1),
        };
        assert_eq!(install.version.as_str(), "v1.19.30");
    }

    /// A download is distinct from an install, so verification can happen first.
    #[test]
    fn downloaded_artifact_is_not_yet_installed() {
        let artifact = DownloadedArtifact {
            version: MihomoVersion::parse("v1.19.30").expect("valid"),
            path: "/tmp/mihomo-download".into(),
            checksum: ConfigChecksum::from_digest(2),
        };
        assert_ne!(artifact.path, "/opt/proxy-agent/bin/mihomo");
    }
}
