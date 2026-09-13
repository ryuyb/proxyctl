//! Configuration validation.
//!
//! Four layers, each with its own failure mode. The layering is not tidiness:
//! each layer exists because the kernel's own checker was measured to miss
//! exactly that class of problem.
//!
//! | Layer | Catches | Why it cannot be delegated |
//! |---|---|---|
//! | L0 preflight | port conflicts, missing geodata | the kernel reports neither before it tries to bind |
//! | L1 syntax | YAML errors | the kernel does catch these, but only after writing state |
//! | L2 semantic | types, enums, references, **unknown fields, and wrong-key values** | `mihomo -t` accepts unknown fields, and accepts a socket path on the address field |
//! | observe | which ports are taken | observation, not judgement |
//!
//! # `mihomo -t` is not a dry run, and that shapes everything
//!
//! Measured on Linux against v1.19.30, a single `-t` on a config with `GEOIP` /
//! `GEOSITE` rules:
//!
//! * downloaded 8.5 MB `geoip.metadb` and 4.2 MB `GeoSite.dat` into the working
//!   directory, taking about four seconds;
//! * reported **success** for a file that did not exist, **creating** it;
//! * accepted a mistyped key (`mixed-portt`) without complaint.
//!
//! So the kernel check is run in a throwaway directory that is removed
//! afterwards, the candidate file is confirmed to exist first, and the result is
//! combined with the field whitelist and the value check in
//! [`values`](crate::validation::values).
//!
//! # The same gap, one level down
//!
//! A *well-formed value on the wrong key* passes `-t` for the same reason a
//! misspelled key does: the kernel accepts the document and then does not do what
//! was meant. `external-controller: /run/mihomo.sock` is the case that made this
//! concrete — measured, `-t` reports `test is successful` and exits `0`, while the
//! kernel at runtime fails to open its control API and keeps running anyway.
//!
//! # Unknown fields warn rather than reject
//!
//! Upstream adds fields between releases. Rejecting an unknown key outright would
//! make a legitimate new config fail on an older agent, so unknown keys are
//! *reported* in the failure reason and the caller decides. A mistyped key is
//! then visible rather than silent, without an upgrade becoming mandatory.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;

use proxy_application::ports::PortError;
use proxy_application::ports::config_validator::{ConfigValidator, PreflightContext};
use proxy_domain::configuration::{ConfigBody, LevelOutcome};

use crate::validation::{values, whitelist};

/// Names of the geo data files the kernel downloads on demand.
pub const GEODATA_FILES: &[&str] = &["geoip.metadb", "GeoSite.dat", "geoip.dat", "geosite.dat"];

/// How long the kernel check may run before it is killed.
///
/// A first run that cannot reach the network retries for a long time; bounding
/// it keeps a validation from hanging a request indefinitely.
pub const KERNEL_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Validates configurations using the kernel plus a field whitelist.
#[derive(Debug, Clone)]
pub struct KernelConfigValidator {
    binary_path: PathBuf,
    /// A directory that survives between calls, used to look for geo data.
    ///
    /// The data directory is where the running kernel keeps its geo files, so
    /// looking there avoids re-downloading them during validation.
    data_dir: PathBuf,
    /// Where throwaway validation directories are created.
    scratch_root: PathBuf,
}

impl KernelConfigValidator {
    /// Creates a validator.
    #[must_use]
    pub fn new(
        binary_path: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        scratch_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            binary_path: binary_path.into(),
            data_dir: data_dir.into(),
            scratch_root: scratch_root.into(),
        }
    }

    /// The kernel binary this validator invokes.
    #[must_use]
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    /// Whether the geo data files are already present in the data directory.
    ///
    /// Public so a caller can build a [`PreflightContext`] without duplicating
    /// the file list.
    #[must_use]
    pub async fn geodata_present(&self) -> bool {
        for name in GEODATA_FILES {
            if tokio::fs::metadata(self.data_dir.join(name)).await.is_ok() {
                return true;
            }
        }
        false
    }

    /// Runs `mihomo -t` against `body` inside a throwaway directory.
    ///
    /// The directory is created fresh and removed afterwards, unconditionally.
    /// Reusing one would let the first validation's downloaded geo data mask a
    /// later download failure, and would accumulate 12.8 MB per run.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the scratch area cannot be prepared.
    /// A *finding* is returned as a [`LevelOutcome`], not an error.
    async fn run_kernel_check(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError> {
        tokio::fs::create_dir_all(&self.scratch_root)
            .await
            .map_err(|e| PortError::Storage(format!("cannot create the scratch root: {e}")))?;

        let scratch = tempfile::Builder::new()
            .prefix("validate-")
            .tempdir_in(&self.scratch_root)
            .map_err(|e| PortError::Storage(format!("cannot create a scratch directory: {e}")))?;
        let dir = scratch.path().to_path_buf();

        // Copy existing geo data in, so validating a geo-referencing config does
        // not re-download files the running kernel already has.
        self.copy_geodata_into(&dir).await;

        let candidate = dir.join("candidate.yaml");
        tokio::fs::write(&candidate, body.as_str())
            .await
            .map_err(|e| PortError::Storage(format!("cannot write the candidate: {e}")))?;

        // Confirm the file exists before invoking the check. `mihomo -t` reports
        // success after *creating* a missing file, so without this the check
        // would pass for a config that was never written.
        if tokio::fs::metadata(&candidate).await.is_err() {
            return Ok(LevelOutcome::Failed(
                "the candidate file could not be written, so the kernel check was not run".into(),
            ));
        }

        let mut command = Command::new(&self.binary_path);
        command
            .arg("-t")
            .arg("-d")
            .arg(&dir)
            .arg("-f")
            .arg(&candidate)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The kernel must not inherit the agent's environment: it would pick
            // up the agent's own config paths.
            .env_clear();

        let output = match tokio::time::timeout(KERNEL_CHECK_TIMEOUT, command.output()).await {
            Err(_) => {
                return Ok(LevelOutcome::Failed(format!(
                    "the kernel check did not finish within {}s; the config may reference \
                     resources it cannot reach",
                    KERNEL_CHECK_TIMEOUT.as_secs()
                )));
            }
            Ok(Err(e)) => {
                return Err(PortError::Storage(format!(
                    "cannot run {}: {e}",
                    self.binary_path.display()
                )));
            }
            Ok(Ok(output)) => output,
        };

        // `scratch` is dropped here, removing the directory and anything the
        // kernel downloaded into it.
        drop(scratch);

        if output.status.success() {
            return Ok(LevelOutcome::Passed);
        }

        // The kernel prints the reason on stderr; the last non-empty line is the
        // most specific one.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .rfind(|line| !line.trim().is_empty())
            .unwrap_or("the kernel rejected the configuration without an explanation")
            .trim()
            .to_owned();

        Ok(LevelOutcome::Failed(format!("mihomo -t: {reason}")))
    }

    /// Copies any existing geo data into a scratch directory.
    ///
    /// Best effort by design: a failure here only means the check may download
    /// what it needs, which is slower but not wrong.
    async fn copy_geodata_into(&self, dir: &Path) {
        for name in GEODATA_FILES {
            let source = self.data_dir.join(name);
            if tokio::fs::metadata(&source).await.is_ok() {
                let _ = tokio::fs::copy(&source, dir.join(name)).await;
            }
        }
    }
}

#[async_trait]
impl ConfigValidator for KernelConfigValidator {
    async fn preflight(
        &self,
        body: &ConfigBody,
        context: &PreflightContext,
    ) -> Result<LevelOutcome, PortError> {
        // Running this before the semantic check is what keeps an offline host
        // from failing validation for a reason unrelated to the document.
        if context.requires_geodata && !context.geodata_present && !context.online {
            return Ok(LevelOutcome::Skipped(
                "the configuration references geo rules, the geo data is not cached, \
                 and the host is offline; the geo check was skipped rather than failed"
                    .into(),
            ));
        }

        if !context.desired_ports.is_empty() {
            let in_use = self.observe_port_usage(&context.desired_ports).await?;
            if !in_use.is_empty() {
                return Ok(LevelOutcome::Failed(format!(
                    "the configuration wants ports that are already in use: {in_use:?}. \
                     Activating it would leave the kernel listening on nothing for those \
                     ports, which is not recoverable without a restart"
                )));
            }
        }

        // A config that needs geo data while offline cannot be checked, but that
        // is a property of the host rather than of the document.
        let _ = body;
        Ok(LevelOutcome::Passed)
    }

    async fn validate_syntax(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError> {
        // Parsed as a bare value rather than into a typed struct: the point is to
        // detect malformed YAML, not to model the schema.
        match serde_yaml::from_str::<serde_yaml::Value>(body.as_str()) {
            Ok(_) => Ok(LevelOutcome::Passed),
            Err(e) => Ok(LevelOutcome::Failed(format!("invalid YAML: {e}"))),
        }
    }

    async fn validate_semantic(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError> {
        // L1 must pass before the key walk, otherwise the walk reports a cascade
        // of nonsense for a document that is not even parseable.
        let syntax = self.validate_syntax(body).await?;
        if !syntax.is_passed() {
            return Ok(syntax);
        }

        let unknown = unknown_fields(body);
        // Values the kernel accepts but does not use. A separate question from an
        // unknown key — this key is spelled correctly and is in the list — so it is
        // a separate check, and both are reported together.
        let mistakes = values::inspect(body);

        let kernel = self.run_kernel_check(body).await?;

        match &kernel {
            // The kernel is authoritative about everything except what it accepts
            // silently, which is what the two checks below exist for.
            LevelOutcome::Failed(reason) => return Ok(LevelOutcome::Failed(reason.clone())),
            LevelOutcome::Skipped(reason) => return Ok(LevelOutcome::Skipped(reason.clone())),
            LevelOutcome::Passed => {}
        }

        if unknown.is_empty() && mistakes.is_empty() {
            return Ok(LevelOutcome::Passed);
        }

        // Both are *warnings* rather than rejections, for the same reason: the
        // kernel runs, so refusing the configuration outright would be more
        // disruptive than the fault. Reported so an operator sees them.
        //
        // A value mistake is listed first, because it breaks a feature outright
        // while an unknown key merely falls back to a default.
        let mut complaints = Vec::new();

        for mistake in &mistakes {
            complaints.push(values::describe(mistake));
        }

        if !unknown.is_empty() {
            // An unknown key: upstream adds fields between releases, so rejecting
            // outright would break a valid new config on an older agent.
            complaints.push(format!(
                "the kernel accepted the configuration, but these keys are not in the \
                 field list for mihomo {}: {}. A misspelled key is ignored by the kernel \
                 and silently falls back to its default. If these are fields added in a \
                 newer release, regenerate the field list",
                whitelist::SOURCE_TAG,
                unknown.join(", ")
            ));
        }

        Ok(LevelOutcome::Failed(complaints.join("; ")))
    }

    async fn observe_port_usage(&self, ports: &[u16]) -> Result<Vec<u16>, PortError> {
        // Probing with a real bind is the only reliable answer: parsing
        // /proc/net/tcp misses sockets in other namespaces and needs a hex parse
        // that is easy to get subtly wrong.
        let mut in_use = Vec::new();
        for port in ports {
            if port_is_bound(*port).await {
                in_use.push(*port);
            }
        }
        Ok(in_use)
    }

    fn requires_geodata(&self, body: &ConfigBody) -> bool {
        // A rules section naming GEOIP/GEOSITE makes the kernel load geo data.
        // Matched as tokens so a rule-provider URL containing the word does not
        // count.
        const MARKERS: [&str; 4] = ["GEOIP,", "GEOSITE,", "geodata-mode", "geox-url"];
        MARKERS.iter().any(|marker| body.as_str().contains(marker))
    }
}

/// Whether a port is already bound on this host.
///
/// Binds and immediately releases. A `false` result means the port was free at
/// the moment of checking, which is the strongest statement available without
/// holding the socket.
async fn port_is_bound(port: u16) -> bool {
    // Bound on all interfaces so an existing wildcard listener is detected; the
    // kernel would fail to bind that port regardless of which address it picks.
    tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .is_err()
}

/// Top-level and nested keys that are not in the whitelist.
///
/// Returns dotted paths so a nested miss is unambiguous (`dns.enable`).
fn unknown_fields(body: &ConfigBody) -> Vec<String> {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(body.as_str()) else {
        return Vec::new();
    };
    let Some(mapping) = value.as_mapping() else {
        return Vec::new();
    };

    let mut unknown = Vec::new();
    for (key, value) in mapping {
        let Some(key) = key.as_str() else {
            // A non-string top-level key is malformed rather than misspelled;
            // the kernel check reports it.
            continue;
        };

        if !whitelist::TOP_LEVEL.contains(&key) {
            unknown.push(key.to_owned());
            continue;
        }

        // Check the nested keys of sections that have a known shape.
        if let Some((_, allowed)) = whitelist::SECTIONS.iter().find(|(name, _)| *name == key)
            && let Some(nested) = value.as_mapping()
        {
            for nested_key in nested.keys() {
                let Some(nested_key) = nested_key.as_str() else {
                    continue;
                };
                if !allowed.contains(&nested_key) {
                    unknown.push(format!("{key}.{nested_key}"));
                }
            }
        }
    }

    unknown.sort();
    unknown
}

#[cfg(test)]
#[path = "validator/tests.rs"]
mod tests;
