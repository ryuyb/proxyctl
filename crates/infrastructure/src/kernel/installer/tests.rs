//! Tests for the kernel installer.
//!
//! The network paths are exercised against a local HTTP server rather than the
//! real GitHub API: the release data changes, the rate limit is shared, and a
//! test suite that depends on a third party is a test suite that fails for
//! reasons unrelated to the code.

use super::*;
use proxy_application::ports::kernel_installer::KernelInstaller;

fn version() -> MihomoVersion {
    MihomoVersion::parse("v1.19.30").expect("valid")
}

/// Serves canned responses on a loopback port and returns its base URL.
///
/// A hand-rolled server rather than a test framework: the responses are static
/// bytes, so a full HTTP server crate would be a large dependency for a few
/// hundred bytes of routing.
///
/// It loops per connection and reads until the request headers are complete.
/// Reading once and answering was the earlier shape, and it silently served the
/// *previous* request's body whenever the client reused the connection — the
/// harness appeared to work while testing nothing.
async fn serve(
    release_json: String,
    assets: std::collections::HashMap<String, Vec<u8>>,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));

    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let release_json = release_json.clone();
            let assets = assets.clone();

            tokio::spawn(async move {
                // Accumulate until the blank line that ends the headers, so a
                // request arriving in several segments is still read whole.
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    match socket.read(&mut buffer).await {
                        Ok(0) => return,
                        Ok(n) => request.extend_from_slice(&buffer[..n]),
                        Err(_) => return,
                    }
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }

                let text = String::from_utf8_lossy(&request).into_owned();
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();

                let (status, body): (&str, Vec<u8>) = if path.contains("/tags/") {
                    ("200 OK", release_json.clone().into_bytes())
                } else if let Some(name) = path.rsplit('/').next() {
                    match assets.get(name) {
                        Some(bytes) => ("200 OK", bytes.clone()),
                        None => ("404 Not Found", b"not found".to_vec()),
                    }
                } else {
                    ("404 Not Found", b"not found".to_vec())
                };

                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: \
                     application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (base, handle)
}

/// Builds a gzip-compressed payload.
fn gzip(contents: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(contents).expect("write");
    encoder.finish().expect("finish")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

/// A shell script that behaves like `mihomo -v` for the install probe.
fn fake_binary_banner() -> Vec<u8> {
    b"#!/bin/sh\necho \"Mihomo Meta v1.19.30 linux arm64\"\n".to_vec()
}

/// Writes an executable and guarantees the write is closed before it is run.
///
/// # Why this exists rather than a bare `write` + `chmod`
///
/// The installer probes an artifact by executing it, and on Linux `exec` fails with
/// `ETXTBSY` ("Text file busy") when *any* process still holds that file open for
/// writing. `tokio::fs::write` returns once the bytes are handed over, so a test
/// that writes and immediately proceeds can race: the probe fires while the write
/// side is still open, and the install fails with an error that says nothing about
/// the real cause.
///
/// It is intermittent by nature — it needs the write handle to still be open when
/// the probe runs — which is why it surfaced on a busy CI runner and never locally.
/// Measured on CI: `the artifact at /tmp/.tmpRNSuCd/artifact is not executable:
/// Text file busy (os error 26)`.
///
/// The write is done through a `std::fs::File` that is explicitly dropped, and
/// `sync_all` is called first, so the descriptor is closed and the contents are on
/// disk before the path is handed to anything that might execute it.
fn write_executable(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write as _;

    let mut file = std::fs::File::create(path).expect("create");
    file.write_all(bytes).expect("write");
    file.sync_all().expect("sync");
    drop(file);

    set_executable(path).expect("chmod");
}

fn release_json(asset: &str, digest: Option<&str>) -> String {
    let digest_field = match digest {
        Some(d) => format!(r#","digest":"{d}""#),
        None => String::new(),
    };
    format!(
        r#"{{"tag_name":"v1.19.30","assets":[{{"name":"{asset}","size":1234{digest_field}}}]}}"#
    )
}

// ------------------------------------------------------------ pure helpers

#[test]
fn asset_names_match_the_upstream_listing() {
    // Taken from the actual release listing, not assumed.
    assert_eq!(
        GithubKernelInstaller::asset_name(&version(), "x86_64").expect("name"),
        "mihomo-linux-amd64-compatible-v1.19.30.gz"
    );
    assert_eq!(
        GithubKernelInstaller::asset_name(&version(), "aarch64").expect("name"),
        "mihomo-linux-arm64-v1.19.30.gz"
    );
}

/// An unknown architecture must be an error rather than a guessed name, which
/// would 404 with a confusing message.
#[test]
fn an_unknown_architecture_is_rejected() {
    let err =
        GithubKernelInstaller::asset_name(&version(), "sparc64").expect_err("must be rejected");
    assert!(err.to_string().contains("sparc64"), "{err}");
}

#[test]
fn the_host_architecture_is_known() {
    assert!(matches!(
        GithubKernelInstaller::host_arch(),
        "x86_64" | "aarch64"
    ));
}

/// The API publishes `sha256:<hex>` and a caller may hold either form.
#[test]
fn digests_match_with_or_without_the_algorithm_prefix() {
    let hex = "58896873736d28628f66de3677c8654fa0f180662523148e136cff4f6e890069";
    assert!(digest_matches(hex, hex));
    assert!(digest_matches(&format!("sha256:{hex}"), hex));
    assert!(!digest_matches(hex, "0000"));
    assert!(!digest_matches("sha256:abcd", "abce"));
}

#[test]
fn digest_comparison_is_case_insensitive() {
    // Hex casing is a presentation detail; a differing case is not a mismatch.
    assert!(digest_matches("ABCD", "abcd"));
    assert!(digest_matches("sha256:ABCD", "abcd"));
}

#[test]
fn version_tokens_are_recognized_in_a_banner() {
    assert!(is_version_token("v1.19.30"));
    assert!(is_version_token("v1.0.0"));
    // The banner contains other dotted tokens that must not be mistaken for one.
    assert!(!is_version_token("Meta"));
    assert!(!is_version_token("go1.26.6"));
    assert!(!is_version_token("v"));
    assert!(!is_version_token("vabc.1.2"));
    assert!(!is_version_token("1.2.3"));
}

#[test]
fn hex_encoding_is_lowercase_and_padded() {
    assert_eq!(hex_digest(&[0x00, 0x0f, 0xff]), "000fff");
    assert_eq!(hex_digest(&[]), "");
}

// ------------------------------------------------------------- no install yet

#[tokio::test]
async fn current_is_none_when_nothing_is_installed() {
    let dir = tempfile::tempdir().expect("dir");
    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer");
    assert!(installer.current().await.expect("current").is_none());
}

#[tokio::test]
async fn rollback_fails_when_nothing_is_retained() {
    let dir = tempfile::tempdir().expect("dir");
    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer");
    let err = installer
        .rollback_previous()
        .await
        .expect_err("nothing to roll back to");
    assert!(
        err.to_string().contains("no previous installation"),
        "{err}"
    );
}

// ------------------------------------------------------------ fetch and verify

/// The happy path: metadata, download, digest match, decompress.
#[tokio::test]
async fn fetch_downloads_verifies_and_decompresses() {
    let dir = tempfile::tempdir().expect("dir");
    let asset = GithubKernelInstaller::asset_name(&version(), GithubKernelInstaller::host_arch())
        .expect("asset");
    let payload = gzip(&fake_binary_banner());
    // The digest covers the compressed artifact, which is what the release API
    // publishes.
    let digest = sha256_hex(&payload);

    let mut assets = std::collections::HashMap::new();
    assets.insert(asset.clone(), payload);
    let (base, server) = serve(
        release_json(&asset, Some(&format!("sha256:{digest}"))),
        assets,
    )
    .await;

    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer")
            .with_endpoints(format!("{base}/repos/api"), format!("{base}/download"));

    let artifact = installer.fetch(&version()).await.expect("fetch");
    assert_eq!(artifact.version, version());
    assert!(std::path::Path::new(&artifact.path).exists());
    // The decompressed file must be the script, not the gzip stream.
    let contents = tokio::fs::read_to_string(&artifact.path)
        .await
        .expect("read");
    assert!(contents.starts_with("#!/bin/sh"), "must be decompressed");
    assert_eq!(artifact.checksum.as_str(), format!("sha256:{digest}"));

    server.abort();
}

/// The case the whole verification exists for: a digest that does not match
/// must prevent the artifact from being produced at all.
#[tokio::test]
async fn a_digest_mismatch_is_refused() {
    let dir = tempfile::tempdir().expect("dir");
    let asset = GithubKernelInstaller::asset_name(&version(), GithubKernelInstaller::host_arch())
        .expect("asset");
    let payload = gzip(&fake_binary_banner());
    // A well-formed but wrong digest.
    let wrong = "0".repeat(64);

    let mut assets = std::collections::HashMap::new();
    assets.insert(asset.clone(), payload);
    let (base, server) = serve(
        release_json(&asset, Some(&format!("sha256:{wrong}"))),
        assets,
    )
    .await;

    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer")
            .with_endpoints(format!("{base}/repos/api"), format!("{base}/download"));

    let err = installer
        .fetch(&version())
        .await
        .expect_err("a mismatched digest must fail");
    assert!(
        err.to_string()
            .contains("does not match the published digest"),
        "the error must explain the mismatch: {err}"
    );

    server.abort();
}

/// Without a published digest there is nothing to verify against, so the fetch
/// must fail rather than install unverified bytes.
#[tokio::test]
async fn an_asset_without_a_published_digest_is_refused() {
    let dir = tempfile::tempdir().expect("dir");
    let asset = GithubKernelInstaller::asset_name(&version(), GithubKernelInstaller::host_arch())
        .expect("asset");
    let payload = gzip(&fake_binary_banner());

    let mut assets = std::collections::HashMap::new();
    assets.insert(asset.clone(), payload);
    // No digest field at all.
    let (base, server) = serve(release_json(&asset, None), assets).await;

    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer")
            .with_endpoints(format!("{base}/repos/api"), format!("{base}/download"));

    let err = installer
        .fetch(&version())
        .await
        .expect_err("an unverifiable artifact must not be fetched");
    assert!(err.to_string().contains("no digest"), "{err}");

    server.abort();
}

/// A version whose asset does not exist must be reported clearly.
#[tokio::test]
async fn a_missing_asset_is_reported() {
    let dir = tempfile::tempdir().expect("dir");
    let asset = GithubKernelInstaller::asset_name(&version(), GithubKernelInstaller::host_arch())
        .expect("asset");
    // Announce a different asset than the one that will be requested.
    let (base, server) = serve(
        release_json("mihomo-linux-something-else.gz", Some("sha256:abcd")),
        std::collections::HashMap::new(),
    )
    .await;

    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer")
            .with_endpoints(format!("{base}/repos/api"), format!("{base}/download"));

    let err = installer.fetch(&version()).await.expect_err("must fail");
    assert!(err.to_string().contains("no asset named"), "{err}");
    let _ = asset;

    server.abort();
}

/// An unreachable API must be an error, not a silent success.
#[tokio::test]
async fn an_unreachable_api_is_reported() {
    let dir = tempfile::tempdir().expect("dir");
    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer")
            // Port 1 on loopback refuses connections.
            .with_endpoints("http://127.0.0.1:1/api", "http://127.0.0.1:1/download");

    let err = installer.fetch(&version()).await.expect_err("must fail");
    assert!(matches!(err, PortError::Unreachable(_)), "{err:?}");
}

/// A truncated gzip stream must be rejected, not written out as a binary.
#[tokio::test]
async fn corrupt_gzip_is_rejected() {
    let dir = tempfile::tempdir().expect("dir");
    let asset = GithubKernelInstaller::asset_name(&version(), GithubKernelInstaller::host_arch())
        .expect("asset");
    // Valid gzip bytes, truncated: the digest matches, so this isolates the
    // decompression path.
    let mut payload = gzip(&fake_binary_banner());
    payload.truncate(payload.len() / 2);
    let digest = sha256_hex(&payload);

    let mut assets = std::collections::HashMap::new();
    assets.insert(asset.clone(), payload);
    let (base, server) = serve(
        release_json(&asset, Some(&format!("sha256:{digest}"))),
        assets,
    )
    .await;

    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer")
            .with_endpoints(format!("{base}/repos/api"), format!("{base}/download"));

    let err = installer.fetch(&version()).await.expect_err("must fail");
    assert!(err.to_string().contains("not valid gzip"), "{err}");

    server.abort();
}

// ---------------------------------------------------------------- verification

/// `verify` compares the recorded digest against the expectation, which is what
/// the release published. It must not re-hash the decompressed binary: the
/// published digest covers the compressed artifact, so hashing the file could
/// never match — an earlier version did that and could never succeed on a real
/// artifact.
#[tokio::test]
async fn verify_compares_the_recorded_digest() {
    let dir = tempfile::tempdir().expect("dir");
    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer");
    let digest = "55cc8c9b6f5f66ff47f39a34ec9122ac3662670b374fbdc0f000da279bd0c6aa";

    let artifact = DownloadedArtifact {
        version: version(),
        path: dir.path().join("whatever").display().to_string(),
        checksum: ConfigChecksum::parse(format!("sha256:{digest}")).expect("checksum"),
    };

    // Both forms of the expectation must be accepted.
    installer
        .verify(&artifact, &ConfigChecksum::parse(digest).expect("checksum"))
        .await
        .expect("a bare hex expectation must match");
    installer
        .verify(
            &artifact,
            &ConfigChecksum::parse(format!("sha256:{digest}")).expect("checksum"),
        )
        .await
        .expect("a prefixed expectation must match");
}

#[tokio::test]
async fn verify_rejects_a_differing_expectation() {
    let dir = tempfile::tempdir().expect("dir");
    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer");
    let artifact = DownloadedArtifact {
        version: version(),
        path: dir.path().join("whatever").display().to_string(),
        checksum: ConfigChecksum::parse("sha256:aaaa").expect("checksum"),
    };

    let err = installer
        .verify(&artifact, &ConfigChecksum::parse("bbbb").expect("checksum"))
        .await
        .expect_err("a mismatch must fail");
    assert!(
        err.to_string().contains("does not match the expected"),
        "{err}"
    );
}

/// Re-hashing whatever is on disk is a separate question, with its own method.
#[tokio::test]
async fn verify_file_digest_hashes_the_file_on_disk() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("artifact");
    tokio::fs::write(&path, b"payload").await.expect("write");
    let digest = sha256_hex(b"payload");

    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer");

    installer
        .verify_file_digest(
            &path,
            &ConfigChecksum::parse(digest.clone()).expect("checksum"),
        )
        .await
        .expect("the matching file must verify");

    let err = installer
        .verify_file_digest(&path, &ConfigChecksum::parse("0000").expect("checksum"))
        .await
        .expect_err("a mismatched file must fail");
    assert!(err.to_string().contains("hashes to"), "{err}");
}

#[tokio::test]
async fn verify_file_digest_reports_a_missing_file() {
    let dir = tempfile::tempdir().expect("dir");
    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer");
    assert!(
        installer
            .verify_file_digest(
                &dir.path().join("absent"),
                &ConfigChecksum::parse("00").expect("checksum")
            )
            .await
            .is_err()
    );
}

// ------------------------------------------------------------------- install

/// An install must not run a non-executable artifact.
#[tokio::test]
async fn install_refuses_a_non_executable_artifact() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("artifact");
    tokio::fs::write(&path, b"not a binary at all")
        .await
        .expect("write");

    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer");
    let artifact = DownloadedArtifact {
        version: version(),
        path: path.display().to_string(),
        checksum: ConfigChecksum::parse("sha256:0000").expect("checksum"),
    };

    let err = installer
        .install(&artifact)
        .await
        .expect_err("a non-executable must not be installed");
    assert!(err.to_string().contains("not executable"), "{err}");
    assert!(
        tokio::fs::metadata(dir.path().join("mihomo"))
            .await
            .is_err(),
        "nothing may be installed"
    );
}

/// An install must be atomic: the target must never be a partial file.
#[tokio::test]
async fn install_is_atomic_and_records_a_manifest() {
    let dir = tempfile::tempdir().expect("dir");
    let artifact_path = dir.path().join("artifact");
    write_executable(&artifact_path, &fake_binary_banner());

    let target = dir.path().join("mihomo");
    let installer =
        GithubKernelInstaller::new(&target, dir.path().join("scratch")).expect("installer");
    let artifact = DownloadedArtifact {
        version: version(),
        path: artifact_path.display().to_string(),
        checksum: ConfigChecksum::parse("sha256:abcd").expect("checksum"),
    };

    let installed = installer.install(&artifact).await.expect("install");
    assert_eq!(installed.version, version());
    assert!(target.exists(), "the binary must be in place");

    // No staging file may be left behind.
    let mut entries = tokio::fs::read_dir(dir.path()).await.expect("read_dir");
    while let Some(entry) = entries.next_entry().await.expect("entry") {
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".install-"),
            "a staging file leaked: {name}"
        );
    }

    // The manifest records the version, and `current` reads it back.
    let current = installer.current().await.expect("current").expect("some");
    assert_eq!(current.version, version());
}

/// Installing over an existing binary must retain the old one, so a broken
/// rollout can be undone.
#[tokio::test]
async fn install_retains_the_previous_binary() {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("mihomo");
    write_executable(&target, b"#!/bin/sh\necho old\n");

    let artifact_path = dir.path().join("artifact");
    write_executable(&artifact_path, &fake_binary_banner());

    let installer =
        GithubKernelInstaller::new(&target, dir.path().join("scratch")).expect("installer");
    let artifact = DownloadedArtifact {
        version: version(),
        path: artifact_path.display().to_string(),
        checksum: ConfigChecksum::parse("sha256:abcd").expect("checksum"),
    };

    installer.install(&artifact).await.expect("install");

    let retained = tokio::fs::read_to_string(installer.retained_path())
        .await
        .expect("the previous binary must be retained");
    assert!(
        retained.contains("old"),
        "the retained file must be the old one"
    );
    assert!(
        std::path::Path::new(&artifact.path).exists(),
        "the artifact itself is not consumed by installing"
    );
}

/// A rollback must restore the retained binary.
#[tokio::test]
async fn rollback_restores_the_retained_binary() {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("mihomo");

    // An "old" binary that reports a different version, so the restored one is
    // identifiable.
    write_executable(
        &target,
        b"#!/bin/sh\necho \"Mihomo Meta v1.18.0 linux arm64\"\n",
    );

    let artifact_path = dir.path().join("artifact");
    write_executable(&artifact_path, &fake_binary_banner());

    let installer =
        GithubKernelInstaller::new(&target, dir.path().join("scratch")).expect("installer");
    installer
        .install(&DownloadedArtifact {
            version: version(),
            path: artifact_path.display().to_string(),
            checksum: ConfigChecksum::parse("sha256:abcd").expect("checksum"),
        })
        .await
        .expect("install");

    assert_eq!(
        installer
            .current()
            .await
            .expect("current")
            .expect("some")
            .version
            .as_str(),
        "v1.19.30"
    );

    let restored = installer.rollback_previous().await.expect("rollback");
    assert_eq!(
        restored.version.as_str(),
        "v1.18.0",
        "the rollback must restore the retained version"
    );
}

/// `current` reads the version from the binary, not the manifest, so a binary
/// replaced outside the agent is reported as what it actually is.
#[tokio::test]
async fn current_reports_the_binary_not_a_stale_manifest() {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("mihomo");
    write_executable(&target, &fake_binary_banner());

    let installer =
        GithubKernelInstaller::new(&target, dir.path().join("scratch")).expect("installer");

    // Write a manifest claiming a different version.
    installer
        .write_manifest(
            &MihomoVersion::parse("v9.9.9").expect("valid"),
            "sha256:abcd",
        )
        .await
        .expect("manifest");

    let current = installer.current().await.expect("current").expect("some");
    assert_eq!(
        current.version.as_str(),
        "v1.19.30",
        "the binary is authoritative about its own version"
    );
}

/// A binary that cannot be run must not be reported as an installation.
#[tokio::test]
async fn current_is_none_for_an_unrunnable_binary() {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("mihomo");
    tokio::fs::write(&target, b"this is not executable")
        .await
        .expect("write");

    let installer =
        GithubKernelInstaller::new(&target, dir.path().join("scratch")).expect("installer");
    assert!(installer.current().await.expect("current").is_none());
}

/// The manifest path sits beside the binary, so it follows a relocated install.
#[tokio::test]
async fn the_manifest_lives_beside_the_binary() {
    let dir = tempfile::tempdir().expect("dir");
    let installer =
        GithubKernelInstaller::new(dir.path().join("mihomo"), dir.path().join("scratch"))
            .expect("installer");
    assert_eq!(
        installer.manifest_path(),
        dir.path().join("mihomo.manifest")
    );
    assert_eq!(
        installer.retained_path(),
        dir.path().join("mihomo.previous")
    );
}

/// Regression guard for the comparison itself, using the exact values from the
/// failing integration path.
///
/// The comparison must be symmetric: an earlier version stripped the algorithm
/// prefix from the first argument only, so a genuinely matching pair reported a
/// mismatch when the arguments arrived in the other order. Both orders and both
/// shapes are asserted here, because that combination is what was missed.
#[test]
fn digest_matches_is_symmetric_across_the_two_forms() {
    let bare = "55cc8c9b6f5f66ff47f39a34ec9122ac3662670b374fbdc0f000da279bd0c6aa";
    let prefixed = format!("sha256:{bare}");

    assert!(digest_matches(&prefixed, bare), "prefixed vs bare");
    assert!(digest_matches(bare, &prefixed), "bare vs prefixed");
    assert!(digest_matches(&prefixed, &prefixed));
    assert!(digest_matches(bare, bare));

    assert!(!digest_matches(&prefixed, "00"));
    assert!(!digest_matches("00", &prefixed));
}
