//! Live verification against the real GitHub release.
//!
//! Ignored by default: it downloads ~17 MB and consumes a share of the
//! unauthenticated API rate limit (60 requests an hour), so it is run
//! deliberately rather than on every `cargo test`.

use proxy_application::ports::kernel_installer::KernelInstaller;
use proxy_domain::mihomo::MihomoVersion;

#[tokio::test]
#[ignore = "downloads ~17 MB from the real GitHub release"]
async fn fetch_installs_and_identifies_a_real_release() {
    let dir = tempfile::tempdir().expect("dir");
    let binary = dir.path().join("mihomo");
    let installer = proxy_infrastructure::kernel::GithubKernelInstaller::new(
        &binary,
        dir.path().join("scratch"),
    )
    .expect("installer");

    let version = MihomoVersion::parse("v1.19.30").expect("version");

    // 1. Metadata + download + digest verification + decompression.
    let artifact = installer
        .fetch(&version)
        .await
        .expect("fetch a real release");
    assert!(std::path::Path::new(&artifact.path).exists());
    assert!(
        artifact.checksum.as_str().starts_with("sha256:"),
        "a verified artifact must carry a digest: {}",
        artifact.checksum.as_str()
    );
    eprintln!(
        "fetched: {} -> {}",
        artifact.path,
        artifact.checksum.as_str()
    );

    // 2. The digest must be reproducible from the artifact.
    installer
        .verify(&artifact, &artifact.checksum)
        .await
        .expect("the artifact must re-verify");

    // 3. Install it and read the version back from the binary itself.
    // A real 46 MB binary: the install probe runs it with `-v`, so this also
    // proves the artifact is a working executable and not merely well-formed.
    let installed = match installer.install(&artifact).await {
        Ok(installed) => installed,
        Err(e) => panic!("install failed for {}: {e}", artifact.path),
    };
    assert_eq!(installed.version.as_str(), "v1.19.30");

    let current = installer.current().await.expect("current").expect("some");
    assert_eq!(current.version.as_str(), "v1.19.30");
    eprintln!("installed and identified: {}", current.binary_path);
}

/// A second run over the same target must retain the previous binary, so a
/// rollout can be undone.
#[tokio::test]
#[ignore = "downloads ~17 MB from the real GitHub release"]
async fn installing_twice_retains_a_rollback_target() {
    let dir = tempfile::tempdir().expect("dir");
    let binary = dir.path().join("mihomo");
    let installer = proxy_infrastructure::kernel::GithubKernelInstaller::new(
        &binary,
        dir.path().join("scratch"),
    )
    .expect("installer");
    let version = MihomoVersion::parse("v1.19.30").expect("version");

    let artifact = installer.fetch(&version).await.expect("fetch");
    installer.install(&artifact).await.expect("first install");
    installer.install(&artifact).await.expect("second install");

    let restored = installer.rollback_previous().await.expect("rollback");
    assert_eq!(restored.version.as_str(), "v1.19.30");
}

/// A version that does not exist upstream must be reported, not silently ignored.
#[tokio::test]
#[ignore = "queries the real GitHub release API"]
async fn a_nonexistent_version_is_reported() {
    let dir = tempfile::tempdir().expect("dir");
    let installer = proxy_infrastructure::kernel::GithubKernelInstaller::new(
        dir.path().join("mihomo"),
        dir.path().join("scratch"),
    )
    .expect("installer");

    let bogus = MihomoVersion::parse("v0.0.1-does-not-exist").expect("version");
    assert!(
        installer.fetch(&bogus).await.is_err(),
        "a nonexistent release must fail rather than produce an artifact"
    );
}
