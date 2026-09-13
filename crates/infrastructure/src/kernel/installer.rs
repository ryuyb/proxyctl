//! Kernel binary installation.
//!
//! The agent owns this path rather than delegating to the kernel's own updater,
//! which reports "already current" as an error, performs no signature check, and
//! can replace a running binary with a partial download.
//!
//! # Where the checksum comes from
//!
//! Upstream publishes **no** checksum file: `…-v1.19.30.gz.sha256` returns 404,
//! and the only checksum-shaped asset, `version.txt`, contains just the version
//! string. The expected digest therefore comes from the GitHub release API, which
//! exposes a `digest` field per asset. That value was independently recomputed
//! from the downloaded bytes and matched.
//!
//! This is also why there is no mirror support. A mirror cannot supply a
//! trustworthy digest — its own `sha256sums` is exactly as untrusted as the
//! artifact — so a mirror would mean either not verifying or pretending to.
//!
//! # Verify before anything changes on disk
//!
//! Fetching and installing are separate steps so a verification failure costs
//! nothing: the artifact sits in a temporary file until it has been verified,
//! and only then is it installed. The previous binary is retained so a rollout
//! that breaks the kernel can be undone.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use proxy_application::ports::PortError;
use proxy_application::ports::kernel_installer::KernelInstaller;
use proxy_application::ports::types::{DownloadedArtifact, KernelInstallation};
use proxy_domain::configuration::ConfigChecksum;
use proxy_domain::mihomo::MihomoVersion;

/// Where release metadata comes from, per repository.
pub const RELEASES_API: &str = "https://api.github.com/repos/MetaCubeX/mihomo/releases";

/// Where release assets are downloaded from.
pub const RELEASES_DOWNLOAD: &str = "https://github.com/MetaCubeX/mihomo/releases/download";

/// How long a download may take before it is abandoned.
///
/// The artifact is roughly 17 MB; this bounds a stalled transfer without
/// penalising a slow link.
pub const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a metadata request may take.
pub const METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The file name recording what is installed, beside the binary.
pub const MANIFEST_SUFFIX: &str = ".manifest";

/// Installs kernel binaries from the upstream release.
#[derive(Debug, Clone)]
pub struct GithubKernelInstaller {
    binary_path: PathBuf,
    scratch_dir: PathBuf,
    client: reqwest::Client,
    releases_api: String,
    releases_download: String,
}

impl GithubKernelInstaller {
    /// Creates an installer.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the HTTP client cannot be built.
    pub fn new(
        binary_path: impl Into<PathBuf>,
        scratch_dir: impl Into<PathBuf>,
    ) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            // A user agent is required by the GitHub API, and naming the agent
            // makes the traffic attributable in upstream logs.
            .user_agent(concat!("proxy-agent/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| PortError::Storage(format!("cannot build an HTTP client: {e}")))?;

        Ok(Self {
            binary_path: binary_path.into(),
            scratch_dir: scratch_dir.into(),
            client,
            releases_api: RELEASES_API.to_owned(),
            releases_download: RELEASES_DOWNLOAD.to_owned(),
        })
    }

    /// Overrides the endpoints, for tests.
    #[must_use]
    pub fn with_endpoints(mut self, api: impl Into<String>, download: impl Into<String>) -> Self {
        self.releases_api = api.into();
        self.releases_download = download.into();
        self
    }

    /// The installed binary's path.
    #[must_use]
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    /// The asset name for this host's architecture and a version.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::InvalidResponse`] on an architecture with no known
    /// upstream asset, rather than guessing a name that would 404.
    pub fn asset_name(version: &MihomoVersion, arch: &str) -> Result<String, PortError> {
        // The names are taken from the actual release listing, not assumed.
        // `amd64-compatible` is the conservative build (no CPU v2/v3 instructions),
        // which is the right default for an unknown host.
        let platform = match arch {
            "x86_64" => "linux-amd64-compatible",
            "aarch64" => "linux-arm64",
            other => {
                return Err(PortError::InvalidResponse(format!(
                    "no upstream kernel asset is known for architecture {other}"
                )));
            }
        };
        Ok(format!("mihomo-{platform}-{}.gz", version.as_str()))
    }

    /// This host's architecture, as the asset names spell it.
    #[must_use]
    pub const fn host_arch() -> &'static str {
        #[cfg(target_arch = "x86_64")]
        {
            "x86_64"
        }
        #[cfg(target_arch = "aarch64")]
        {
            "aarch64"
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            "unsupported"
        }
    }

    /// Fetches the expected digest for an asset from the release API.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::UnexpectedStatus`] with the status when the API
    /// refuses — an unauthenticated caller is limited to 60 requests an hour, and
    /// a bare status is not an actionable message — and
    /// [`PortError::InvalidResponse`] when the asset is absent from the release.
    async fn expected_digest(
        &self,
        version: &MihomoVersion,
        asset: &str,
    ) -> Result<String, PortError> {
        let url = format!("{}/tags/{}", self.releases_api, version.as_str());
        let response = tokio::time::timeout(METADATA_TIMEOUT, self.client.get(&url).send())
            .await
            .map_err(|_| PortError::Timeout(METADATA_TIMEOUT))?
            .map_err(|e| PortError::Unreachable(Box::new(e)))?;

        let status = response.status();
        if !status.is_success() {
            // A 403 here is almost always the unauthenticated rate limit (60
            // requests an hour), and a bare status code would not say so.
            if status.as_u16() == 403 {
                return Err(PortError::InvalidResponse(
                    "the release API refused the request, most likely because the \
                     unauthenticated rate limit was exceeded; retry later"
                        .into(),
                ));
            }
            return Err(PortError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }

        let release: serde_json::Value = response.json().await.map_err(|e| {
            PortError::InvalidResponse(format!("release metadata is not JSON: {e}"))
        })?;

        let assets = release
            .get("assets")
            .and_then(|a| a.as_array())
            .ok_or_else(|| {
                PortError::InvalidResponse("release metadata has no assets array".into())
            })?;

        for candidate in assets {
            if candidate.get("name").and_then(|n| n.as_str()) != Some(asset) {
                continue;
            }
            // The digest is the reason this call happens at all.
            let digest = candidate
                .get("digest")
                .and_then(|d| d.as_str())
                .ok_or_else(|| {
                    PortError::InvalidResponse(format!(
                        "the release publishes no digest for {asset}; without one the download \
                     cannot be verified and will not be installed"
                    ))
                })?;
            return Ok(digest.to_owned());
        }

        Err(PortError::InvalidResponse(format!(
            "release {} has no asset named {asset}",
            version.as_str()
        )))
    }

    /// Downloads an asset and verifies it against `expected`.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::InvalidResponse`] when the bytes do not match the
    /// expected digest. That is deliberately an error rather than a warning: an
    /// unverified binary is not installed.
    async fn download_and_verify(
        &self,
        version: &MihomoVersion,
        asset: &str,
        expected: &str,
    ) -> Result<DownloadedArtifact, PortError> {
        tokio::fs::create_dir_all(&self.scratch_dir)
            .await
            .map_err(|e| PortError::Storage(format!("cannot create the scratch directory: {e}")))?;

        let url = format!("{}/{}/{}", self.releases_download, version.as_str(), asset);
        let response = tokio::time::timeout(DOWNLOAD_TIMEOUT, self.client.get(&url).send())
            .await
            .map_err(|_| PortError::Timeout(DOWNLOAD_TIMEOUT))?
            .map_err(|e| PortError::Unreachable(Box::new(e)))?;

        let status = response.status();
        if !status.is_success() {
            return Err(PortError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }

        let compressed = response
            .bytes()
            .await
            .map_err(|e| PortError::Transport(format!("cannot read the download: {e}")))?;

        // Hash the compressed artifact, which is what the published digest
        // covers; hashing the decompressed binary would never match.
        let actual_digest = format!("sha256:{}", sha256_hex(&compressed));

        if !digest_matches(expected, &sha256_hex(&compressed)) {
            return Err(PortError::InvalidResponse(format!(
                "the downloaded {asset} does not match the published digest: \
                 expected {expected}, got {actual_digest}. The artifact was discarded"
            )));
        }

        // Decompress into the scratch area. The compressed form is not kept:
        // only the executable is installed.
        let binary = gunzip_bytes(&compressed, &self.scratch_dir, asset).await?;

        Ok(DownloadedArtifact {
            version: version.clone(),
            path: binary.display().to_string(),
            checksum: ConfigChecksum::parse(actual_digest).map_err(|e| {
                PortError::InvalidResponse(format!("computed an invalid checksum: {e}"))
            })?,
        })
    }
}

#[async_trait]
impl KernelInstaller for GithubKernelInstaller {
    async fn current(&self) -> Result<Option<KernelInstallation>, PortError> {
        if tokio::fs::metadata(&self.binary_path).await.is_err() {
            return Ok(None);
        }

        // The version comes from the binary itself rather than from the manifest,
        // so a binary replaced outside the agent is reported as what it is.
        let Some(version) = self.probe_binary_version().await? else {
            return Ok(None);
        };

        // The checksum comes from the manifest, since re-reading a 46 MB binary
        // on every status query would be wasteful. A missing manifest yields an
        // empty checksum rather than a fabricated one.
        let checksum = match self.read_manifest().await {
            Some((recorded_version, digest)) if recorded_version == version => {
                ConfigChecksum::parse(digest)
                    .map_err(|e| PortError::InvalidResponse(format!("manifest checksum: {e}")))?
            }
            _ => ConfigChecksum::parse("unknown:not-recorded").map_err(|e| {
                PortError::InvalidResponse(format!("placeholder checksum is invalid: {e}"))
            })?,
        };

        Ok(Some(KernelInstallation {
            version,
            binary_path: self.binary_path.display().to_string(),
            checksum,
        }))
    }

    async fn fetch(&self, version: &MihomoVersion) -> Result<DownloadedArtifact, PortError> {
        let asset = Self::asset_name(version, Self::host_arch())?;
        let expected = self.expected_digest(version, &asset).await?;
        self.download_and_verify(version, &asset, &expected).await
    }

    /// Verifies a downloaded artifact against an expected digest.
    ///
    /// # Which bytes the digest covers
    ///
    /// The digest in [`DownloadedArtifact::checksum`] is the one the release
    /// published, which covers the **compressed** artifact — not the decompressed
    /// binary at [`DownloadedArtifact::path`]. This method therefore compares
    /// `expected` against the recorded checksum rather than re-hashing the file:
    /// hashing the decompressed binary would never match a published digest, and
    /// an earlier version of this function did exactly that and so could never
    /// succeed on a real artifact.
    ///
    /// To check the file on disk against a digest, use
    /// [`Self::verify_file_digest`], which is explicit about hashing the file.
    async fn verify(
        &self,
        artifact: &DownloadedArtifact,
        expected: &ConfigChecksum,
    ) -> Result<(), PortError> {
        if !digest_matches(expected.as_str(), artifact.checksum.as_str()) {
            return Err(PortError::InvalidResponse(format!(
                "the artifact records {}, which does not match the expected {}",
                artifact.checksum.as_str(),
                expected.as_str()
            )));
        }
        Ok(())
    }

    async fn install(
        &self,
        artifact: &DownloadedArtifact,
    ) -> Result<KernelInstallation, PortError> {
        // Refuse to install anything that is not executable, so a corrupt or
        // still-compressed file cannot replace a working kernel.
        let probe = tokio::process::Command::new(&artifact.path)
            .arg("-v")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .await;

        match probe {
            Ok(output) if output.status.success() => {}
            Ok(_) => {
                return Err(PortError::InvalidResponse(format!(
                    "the artifact at {} did not run successfully, so it will not be installed",
                    artifact.path
                )));
            }
            Err(e) => {
                return Err(PortError::InvalidResponse(format!(
                    "the artifact at {} is not executable: {e}",
                    artifact.path
                )));
            }
        }

        if let Some(parent) = self.binary_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                PortError::Storage(format!("cannot create {}: {e}", parent.display()))
            })?;
        }

        // Retain the previous binary first, so a rollout that breaks the kernel
        // can be undone. Done before the swap, because after it the original is
        // gone.
        if tokio::fs::metadata(&self.binary_path).await.is_ok() {
            let retained = self.retained_path();
            let _ = tokio::fs::remove_file(&retained).await;
            tokio::fs::copy(&self.binary_path, &retained)
                .await
                .map_err(|e| {
                    PortError::Storage(format!(
                        "cannot retain the current binary at {}: {e}",
                        retained.display()
                    ))
                })?;
        }

        install_binary_atomically(Path::new(&artifact.path), &self.binary_path).await?;

        self.write_manifest(&artifact.version, artifact.checksum.as_str())
            .await?;

        Ok(KernelInstallation {
            version: artifact.version.clone(),
            binary_path: self.binary_path.display().to_string(),
            checksum: artifact.checksum.clone(),
        })
    }

    async fn rollback_previous(&self) -> Result<KernelInstallation, PortError> {
        let retained = self.retained_path();
        if tokio::fs::metadata(&retained).await.is_err() {
            return Err(PortError::Storage(format!(
                "no previous installation is retained at {}",
                retained.display()
            )));
        }

        install_binary_atomically(&retained, &self.binary_path).await?;

        // Report the version actually restored, read back from the binary rather
        // than assumed to be whatever was retained.
        self.current()
            .await?
            .ok_or_else(|| PortError::Storage("the restored binary could not be identified".into()))
    }
}

impl GithubKernelInstaller {
    /// Re-hashes a file on disk and compares it against `expected`.
    ///
    /// Separate from [`KernelInstaller::verify`], which compares recorded
    /// digests, because the two answer different questions:
    ///
    /// * `verify` — "is this the artifact the release published?"
    /// * `verify_file_digest` — "do the bytes on disk still match that?"
    ///
    /// The second is what a caller re-checks before installing from a scratch
    /// path that could have been modified in between.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::InvalidResponse`] on a mismatch, and
    /// [`PortError::Storage`] when the file cannot be read.
    pub async fn verify_file_digest(
        &self,
        path: &Path,
        expected: &ConfigChecksum,
    ) -> Result<(), PortError> {
        let actual = hash_file(path).await?;
        if !digest_matches(expected.as_str(), &actual) {
            return Err(PortError::InvalidResponse(format!(
                "the file at {} hashes to sha256:{actual}, not {}",
                path.display(),
                expected.as_str()
            )));
        }
        Ok(())
    }

    /// The path the previous binary is retained at.
    fn retained_path(&self) -> PathBuf {
        let mut path = self.binary_path.clone();
        path.set_extension("previous");
        path
    }

    /// The path of the manifest recording what is installed.
    fn manifest_path(&self) -> PathBuf {
        let mut path = self.binary_path.clone();
        let name = format!(
            "{}{}",
            self.binary_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            MANIFEST_SUFFIX
        );
        path.set_file_name(name);
        path
    }

    /// Reads the manifest, if present.
    async fn read_manifest(&self) -> Option<(MihomoVersion, String)> {
        let contents = tokio::fs::read_to_string(self.manifest_path()).await.ok()?;
        let mut lines = contents.lines();
        let version = MihomoVersion::parse(lines.next()?).ok()?;
        let digest = lines.next()?.trim().to_owned();
        Some((version, digest))
    }

    /// Records what was installed.
    async fn write_manifest(&self, version: &MihomoVersion, digest: &str) -> Result<(), PortError> {
        let contents = format!("{}\n{digest}\n", version.as_str());
        tokio::fs::write(self.manifest_path(), contents)
            .await
            .map_err(|e| PortError::Storage(format!("cannot write the manifest: {e}")))
    }

    /// Asks the installed binary for its version.
    async fn probe_binary_version(&self) -> Result<Option<MihomoVersion>, PortError> {
        let output = tokio::process::Command::new(&self.binary_path)
            .arg("-v")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .await;

        let output = match output {
            Ok(output) => output,
            // A file that cannot be executed is not an installation. Reporting
            // this as an error would make `current` — and therefore any status
            // query — fail on a host whose binary was replaced by something that
            // is not executable, which is exactly when an operator needs the
            // status to work.
            Err(_) => return Ok(None),
        };

        if !output.status.success() {
            return Ok(None);
        }

        // The banner is `Mihomo Meta v1.19.30 linux arm64 with go1.26.6 ...`, so
        // the version is the first token that looks like one.
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .split_whitespace()
            .find(|token| is_version_token(token))
            .and_then(|token| MihomoVersion::parse(token).ok()))
    }
}

/// Whether a banner token looks like a `vX.Y.Z` version.
fn is_version_token(token: &str) -> bool {
    let Some(rest) = token.strip_prefix('v') else {
        return false;
    };
    let mut parts = rest.split('.');
    let (Some(major), Some(minor), Some(_patch)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && major.chars().all(|c| c.is_ascii_digit())
        && minor.chars().all(|c| c.is_ascii_digit())
}

/// Normalizes a digest to its bare lowercase hex form.
///
/// Accepts either the `sha256:<hex>` form the release API publishes or bare hex,
/// because the values reach this function from several places — the API, a
/// `ConfigChecksum`, and a computed digest — and they do not all carry the
/// prefix. Requiring one specific shape would make a correct digest fail.
fn normalize_digest(value: &str) -> String {
    value
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or_else(|| value.trim())
        .trim()
        .to_ascii_lowercase()
}

/// Whether two digests denote the same value.
///
/// Symmetric: it must not matter which side carries the algorithm prefix. An
/// earlier version only stripped the prefix from the first argument, so a
/// correctly matching pair compared in the other order reported a mismatch.
fn digest_matches(left: &str, right: &str) -> bool {
    normalize_digest(left) == normalize_digest(right)
}

/// Hashes a file, streaming so a large binary does not have to be held in memory.
async fn hash_file(path: &Path) -> Result<String, PortError> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| PortError::Storage(format!("cannot open {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|e| PortError::Storage(format!("cannot read {}: {e}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex_digest(&hasher.finalize()))
}

/// The SHA-256 of `bytes`, hex encoded.
///
/// Distinct from [`hex_digest`], which only encodes. Conflating the two is an
/// easy mistake and a costly one: it produces a plausible-looking value of the
/// wrong length that never matches a real digest.
fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

/// Hex-encodes a digest.
fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Decompresses a gzip stream into the scratch directory.
async fn gunzip_bytes(
    compressed: &[u8],
    scratch: &Path,
    asset: &str,
) -> Result<PathBuf, PortError> {
    let scratch = scratch.to_path_buf();
    let asset = asset.to_owned();
    let bytes = compressed.to_vec();

    tokio::task::spawn_blocking(move || -> Result<PathBuf, PortError> {
        use std::io::Write;

        // Stripped of `.gz`, so the file keeps the asset name the caller expects.
        let name = asset.strip_suffix(".gz").unwrap_or(&asset).to_owned();
        let target = scratch.join(name);

        let mut decoder = flate2::read::GzDecoder::new(bytes.as_slice());
        let mut file = std::fs::File::create(&target)
            .map_err(|e| PortError::Storage(format!("cannot create {}: {e}", target.display())))?;
        std::io::copy(&mut decoder, &mut file).map_err(|e| {
            PortError::InvalidResponse(format!("the downloaded artifact is not valid gzip: {e}"))
        })?;
        file.flush()
            .map_err(|e| PortError::Storage(format!("cannot flush the binary: {e}")))?;

        set_executable(&target)?;
        Ok(target)
    })
    .await
    .map_err(|e| PortError::Storage(format!("decompression task failed: {e}")))?
}

/// Replaces `target` with `source` atomically.
///
/// A same-directory rename, so the target is only ever observed as fully old or
/// fully new. Writing in place would expose a half-written kernel binary, and the
/// running kernel is executing from a path an operator may point at the target.
async fn install_binary_atomically(source: &Path, target: &Path) -> Result<(), PortError> {
    let source = source.to_path_buf();
    let target = target.to_path_buf();
    let directory = target
        .parent()
        .ok_or_else(|| PortError::Storage("the binary path has no parent".into()))?
        .to_path_buf();

    tokio::task::spawn_blocking(move || -> Result<(), PortError> {
        // The temporary lives in the *target* directory, so the rename cannot
        // cross a filesystem boundary and stop being atomic.
        //
        // The name is unique per call, not per process. It used to be
        // `.install-{pid}.tmp`, which is unique only while one install is in
        // flight: both `install` and `rollback` reach here, and two of them
        // overlapping — a rollout while a rollback is running, or two tests in
        // one process — staged to the same path. The second copy then wrote to a
        // file the first was still using, and `set_executable` on it failed with
        // `ETXTBSY` ("Text file busy") for the loser. Measured on CI, where it
        // presented as "the artifact is not executable", which points at the
        // wrong thing entirely.
        //
        // A counter plus the pid keeps the name unique within the process and
        // between processes, and stays predictable enough to reason about. A
        // random suffix would work as well; the counter makes a leak obvious,
        // because a leftover staging file keeps a name that says which call made
        // it.
        static STAGING_SEQUENCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let sequence = STAGING_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = directory.join(format!(".install-{}-{sequence}.tmp", std::process::id()));

        std::fs::copy(&source, &staging).map_err(|e| {
            PortError::Storage(format!(
                "cannot stage the binary at {}: {e}",
                staging.display()
            ))
        })?;
        set_executable_blocking(&staging)?;

        // Flush before the rename, so a crash cannot leave the target name
        // pointing at unwritten data.
        let file = std::fs::File::open(&staging)
            .map_err(|e| PortError::Storage(format!("cannot open the staged binary: {e}")))?;
        file.sync_all().map_err(|e| {
            let _ = std::fs::remove_file(&staging);
            PortError::Storage(format!("cannot flush the staged binary: {e}"))
        })?;
        drop(file);

        std::fs::rename(&staging, &target).map_err(|e| {
            let _ = std::fs::remove_file(&staging);
            PortError::Storage(format!("cannot replace {}: {e}", target.display()))
        })?;
        Ok(())
    })
    .await
    .map_err(|e| PortError::Storage(format!("install task failed: {e}")))?
}

/// Marks a path executable.
fn set_executable(path: &Path) -> Result<(), PortError> {
    set_executable_blocking(path)
}

/// Marks a path executable, blocking.
fn set_executable_blocking(path: &Path) -> Result<(), PortError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| PortError::Storage(format!("cannot make {} executable: {e}", path.display())))
}

#[cfg(test)]
#[path = "installer/tests.rs"]
mod tests;
