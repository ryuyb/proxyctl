//! Merging configuration sources, and recording where each value came from.
//!
//! # Why the provenance is not optional bookkeeping
//!
//! The single most common deployment failure is "I changed the configuration and
//! nothing happened" — because a flag overrode it, because the environment
//! variable was still exported, because the file being edited is not the file
//! being read. A merged value alone cannot answer that; it looks identical in all
//! three cases.
//!
//! So [`Resolved`] carries the source alongside every value, and
//! `--print-config` prints them. That is the whole reason this module exists
//! rather than a chain of `unwrap_or` calls.
//!
//! # Precedence
//!
//! ```text
//! ① explicit argument   one-shot, and a debugger's tool
//! ② environment         how a container or CI injects
//! ③ file                the deployment's stated intent
//! ④ built-in default    what the code does with no input at all
//! ```
//!
//! An explicit argument outranks the file because temporarily overriding one
//! value must not require editing a file that packaging manages. The environment
//! sits between them because it is easier to inject than a file but should not
//! defeat a flag the operator typed on this very command line.

use std::path::Path;

use proxy_domain::shared::id::MihomoInstanceId;

use super::file::{self, FileConfig, SecretState};
use super::{ControllerEndpoint, ConverterConfig, DataPaths, RuntimeConfig};
use crate::BootstrapError;

/// Where a value came from.
///
/// Ordering is not derived from the variant order — the variants are listed in
/// precedence order for readability, but a comparison would be a coincidence
/// rather than a fact about the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// An explicit argument.
    Argument,
    /// An environment variable.
    Environment,
    /// The configuration file.
    File,
    /// The built-in default.
    Default,
}

impl Source {
    /// A short label for `--print-config`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Argument => "argument",
            Self::Environment => "environment",
            Self::File => "file",
            Self::Default => "default",
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A value and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attributed<T> {
    /// The value in effect.
    pub value: T,
    /// Where it came from.
    pub source: Source,
}

impl<T> Attributed<T> {
    /// A value from an explicit argument.
    #[must_use]
    pub const fn argument(value: T) -> Self {
        Self {
            value,
            source: Source::Argument,
        }
    }

    /// A value from the environment.
    #[must_use]
    pub const fn environment(value: T) -> Self {
        Self {
            value,
            source: Source::Environment,
        }
    }

    /// A value from the file.
    #[must_use]
    pub const fn file(value: T) -> Self {
        Self {
            value,
            source: Source::File,
        }
    }

    /// A built-in default.
    #[must_use]
    pub const fn default_value(value: T) -> Self {
        Self {
            value,
            source: Source::Default,
        }
    }
}

/// The inputs to a merge, already gathered.
///
/// Gathered rather than read directly so the merge is a pure function of these
/// values: no environment lookups, no file reads, and therefore a test that can
/// assert the precedence matrix without mutating process state. Reading the
/// environment inside the merge would make these tests order-dependent and
/// racy, which is exactly how a precedence rule ends up untested.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// Instance from an argument.
    pub instance: Option<String>,
    /// Root directory from an argument.
    pub root: Option<String>,
    /// Controller from an argument.
    pub controller: Option<String>,
    /// Kernel binary from an argument.
    pub kernel_binary: Option<String>,
    /// Allow-list from an argument.
    pub subscription_allow: Option<Vec<String>>,
    /// Write probes from an argument.
    pub allow_write_probes: Option<bool>,
    /// The parsed configuration file, when one was read.
    pub file: Option<FileConfig>,
}

/// The outcome of a merge: the composed configuration plus its provenance.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The configuration composition consumes.
    pub config: RuntimeConfig,
    /// Where each user-visible value came from.
    pub provenance: Vec<(String, String, Source)>,
    /// A warning to surface at startup, when the merge found a conflict the
    /// operator must know about.
    pub warning: Option<String>,
}

impl Resolved {
    /// Renders the effective configuration for `--print-config`.
    ///
    /// Column-aligned so the sources can be scanned as a column: the question
    /// being asked is almost always "which of these is not what I expected".
    #[must_use]
    pub fn report(&self, config_path: &Path, loaded: bool) -> String {
        let mut lines = vec![format!(
            "config: {} ({})",
            config_path.display(),
            if loaded {
                "loaded"
            } else {
                "not found; using defaults"
            }
        )];
        let width = self
            .provenance
            .iter()
            .map(|(name, _, _)| name.len())
            .max()
            .unwrap_or(0);
        for (name, value, source) in &self.provenance {
            lines.push(format!(
                "  {name:<width$}  {value:<40}  [{}]",
                source.label()
            ));
        }
        lines.join("\n")
    }
}

/// Merges the gathered inputs into a configuration.
///
/// # Errors
///
/// Returns [`BootstrapError::InvalidConfig`] when a value cannot be used: an
/// invalid instance name, an empty or relative controller path, a converter
/// backend that is not recognised, or a secret that is present but empty.
pub fn merge(inputs: &Inputs) -> Result<Resolved, BootstrapError> {
    let mut provenance = Vec::new();
    let mut warning = None;

    // --- instance ---------------------------------------------------------
    let instance_text = inputs
        .instance
        .clone()
        .map(Attributed::argument)
        .or_else(|| env_string("INSTANCE").map(Attributed::environment))
        .or_else(|| {
            file_string(inputs, |f| {
                f.agent.as_ref().and_then(|a| a.instance.clone())
            })
        })
        .unwrap_or_else(|| Attributed::default_value("default".to_owned()));
    let instance = MihomoInstanceId::parse(&instance_text.value).map_err(|e| {
        BootstrapError::InvalidConfig(format!(
            "invalid instance name {:?} (from {}): {e}",
            instance_text.value, instance_text.source
        ))
    })?;
    provenance.push((
        "instance".to_owned(),
        instance_text.value.clone(),
        instance_text.source,
    ));

    // --- paths ------------------------------------------------------------
    // A root moves every path together. It is an argument-only concept:
    // expressing "root" in the file would duplicate what `[paths]` already says,
    // and two ways to set one thing is two ways to disagree.
    let standard = DataPaths::standard();
    let file_paths = inputs.file.as_ref().and_then(|f| f.paths.clone());

    let configs_dir = match (&inputs.root, &file_paths) {
        (Some(root), _) => Attributed::argument(join(root, "lib/configs")),
        (None, Some(p)) if p.configs_dir.is_some() => {
            Attributed::file(p.configs_dir.clone().unwrap_or_default())
        }
        _ => Attributed::default_value(standard.configs_dir.clone()),
    };
    let state_dir = match (&inputs.root, &file_paths) {
        (Some(root), _) => Attributed::argument(join(root, "lib/state")),
        (None, Some(p)) if p.state_dir.is_some() => {
            Attributed::file(p.state_dir.clone().unwrap_or_default())
        }
        _ => Attributed::default_value(standard.state_dir.clone()),
    };
    let run_dir = match (&inputs.root, &file_paths) {
        (Some(root), _) => Attributed::argument(join(root, "run")),
        (None, Some(p)) if p.run_dir.is_some() => {
            Attributed::file(p.run_dir.clone().unwrap_or_default())
        }
        _ => Attributed::default_value(standard.run_dir.clone()),
    };

    let paths = DataPaths {
        configs_dir: configs_dir.value.clone(),
        state_dir: state_dir.value.clone(),
        run_dir: run_dir.value.clone(),
    };
    provenance.push((
        "paths.configs_dir".to_owned(),
        configs_dir.value,
        configs_dir.source,
    ));
    provenance.push((
        "paths.state_dir".to_owned(),
        state_dir.value,
        state_dir.source,
    ));
    provenance.push(("paths.run_dir".to_owned(), run_dir.value, run_dir.source));

    // --- controller -------------------------------------------------------
    let controller_text = inputs
        .controller
        .clone()
        .map(Attributed::argument)
        .or_else(|| env_string("CONTROLLER").map(Attributed::environment))
        .or_else(|| {
            file_string(inputs, |f| {
                f.controller.as_ref().and_then(|c| c.endpoint.clone())
            })
        })
        .unwrap_or_else(|| {
            // Derived from the run directory rather than a fixed literal, so a
            // root moves the kernel socket too.
            Attributed::default_value(format!(
                "{}/mihomo.sock",
                paths.run_dir.trim_end_matches('/')
            ))
        });
    let controller = parse_controller(&controller_text.value).map_err(|reason| {
        BootstrapError::InvalidConfig(format!(
            "controller {:?} (from {}): {reason}",
            controller_text.value, controller_text.source
        ))
    })?;
    provenance.push((
        "controller".to_owned(),
        controller.describe(),
        controller_text.source,
    ));

    // --- agent socket -----------------------------------------------------
    let socket = inputs
        .file
        .as_ref()
        .and_then(|f| f.agent.as_ref())
        .and_then(|a| a.socket.clone())
        .map(Attributed::file)
        .unwrap_or_else(|| {
            Attributed::default_value(format!(
                "{}/agent.sock",
                paths.run_dir.trim_end_matches('/')
            ))
        });
    provenance.push((
        "agent.socket".to_owned(),
        socket.value.clone(),
        socket.source,
    ));

    // --- kernel -----------------------------------------------------------
    // Everything the kernel owns is derived from one install root: the state
    // directory's parent. That keeps an install and a later atomic rename on one
    // filesystem, and it is what makes a relocation coherent — pointing
    // `paths.state_dir` somewhere else must move the kernel binary too, or the
    // agent would try to create `/usr/lib/proxy-agent` on a host that keeps
    // everything under /srv.
    let parent = paths
        .state_dir
        .trim_end_matches('/')
        .rsplit_once('/')
        .map(|(p, _)| p.to_owned())
        .unwrap_or_else(|| "/var/lib/proxy-agent".to_owned());

    // A root moves the binary too, into `{root}/bin/mihomo`. It has to: the
    // default `/usr/lib/proxy-agent` is a system path, and a development run that
    // kept it would try to create `/usr/lib/proxy-agent` — which either fails or,
    // worse, succeeds against the real installation. `RuntimeConfig::rooted_at`
    // did this before the merge existed, and losing it here was a real regression
    // caught only by a test that runs the whole daemon path.
    let kernel_binary = inputs
        .kernel_binary
        .clone()
        .map(Attributed::argument)
        .or_else(|| env_string("KERNEL_BINARY").map(Attributed::environment))
        .or_else(|| file_string(inputs, |f| f.kernel.as_ref().and_then(|k| k.binary.clone())))
        .or_else(|| {
            inputs
                .root
                .as_ref()
                .map(|root| Attributed::argument(join(root, "bin/mihomo")))
        })
        .or_else(|| {
            // Only when the *layout was relocated*, which is the case exactly
            // when the file names `[paths]`. Leaving the system path
            // `/usr/lib/proxy-agent` in place would have the agent try to create
            // it on a host whose state lives elsewhere — refused, or worse,
            // succeeding against a real installation.
            //
            // The derived location is `{install_root}/bin/mihomo`, where
            // `install_root` is the state directory's parent: the same root the
            // kernel's data directory already uses, so one directory holds
            // everything this agent owns. The default layout does not reach this
            // branch, so a system install keeps the documented `/usr/lib` path.
            inputs
                .file
                .as_ref()
                .and_then(|f| f.paths.as_ref())
                .is_some()
                .then(|| Attributed::file(format!("{parent}/bin/mihomo")))
        })
        .unwrap_or_else(|| Attributed::default_value(super::DEFAULT_KERNEL_BINARY.to_owned()));
    provenance.push((
        "kernel.binary".to_owned(),
        kernel_binary.value.clone(),
        kernel_binary.source,
    ));

    let kernel_data_dir = inputs
        .root
        .as_ref()
        .map(|root| Attributed::argument(join(root, "lib/mihomo")))
        .or_else(|| {
            file_string(inputs, |f| {
                f.kernel.as_ref().and_then(|k| k.data_dir.clone())
            })
        })
        .unwrap_or_else(|| Attributed::default_value(format!("{parent}/mihomo")));
    let scratch_dir = inputs
        .root
        .as_ref()
        .map(|root| Attributed::argument(join(root, "lib/scratch")))
        .or_else(|| {
            file_string(inputs, |f| {
                f.kernel.as_ref().and_then(|k| k.scratch_dir.clone())
            })
        })
        .unwrap_or_else(|| Attributed::default_value(format!("{parent}/scratch")));
    provenance.push((
        "kernel.data_dir".to_owned(),
        kernel_data_dir.value.clone(),
        kernel_data_dir.source,
    ));
    provenance.push((
        "kernel.scratch_dir".to_owned(),
        scratch_dir.value.clone(),
        scratch_dir.source,
    ));

    // --- secret -----------------------------------------------------------
    let (secret, secret_source) = resolve_secret_source(inputs)?;
    provenance.push((
        "kernel.secret".to_owned(),
        // Never printed: a value in a report is a value in a terminal buffer, a
        // scrollback, and a pasted issue. The source alone answers the question
        // the report is asked ("where did this come from").
        match secret {
            Some(_) => "<set>".to_owned(),
            None => "<unset>".to_owned(),
        },
        secret_source,
    ));

    // A file-provided secret overrides whatever the store generated. That is the
    // operator's explicit intent, but it must be said out loud: otherwise an
    // operator who rotates the stored value sees no effect and cannot tell why.
    if secret_source == Source::File {
        warning = Some(
            "kernel.secret is set in the configuration file, so any value already stored in the \
             database is ignored. Remove the field to let the agent manage it."
                .to_owned(),
        );
    }

    // --- converter --------------------------------------------------------
    let converter = resolve_converter(inputs)?;
    provenance.push((
        "converter".to_owned(),
        converter.describe(),
        if inputs
            .file
            .as_ref()
            .and_then(|f| f.converter.as_ref())
            .is_some()
        {
            Source::File
        } else {
            Source::Default
        },
    ));

    // --- security ---------------------------------------------------------
    let allow = inputs
        .subscription_allow
        .clone()
        .map(Attributed::argument)
        .or_else(|| {
            inputs
                .file
                .as_ref()
                .filter(|f| {
                    f.security
                        .as_ref()
                        .and_then(|s| s.subscription_allow.as_ref())
                        .is_some()
                })
                .map(|f| Attributed::file(f.subscription_allow()))
        })
        .unwrap_or_else(|| Attributed::default_value(Vec::new()));
    provenance.push((
        "security.subscription_allow".to_owned(),
        if allow.value.is_empty() {
            "<empty: public destinations only>".to_owned()
        } else {
            allow.value.join(",")
        },
        allow.source,
    ));

    let write_probes = inputs
        .allow_write_probes
        .map(Attributed::argument)
        .or_else(|| {
            inputs
                .file
                .as_ref()
                .filter(|f| {
                    f.security
                        .as_ref()
                        .and_then(|s| s.allow_write_probes)
                        .is_some()
                })
                .map(|f| Attributed::file(f.allow_write_probes()))
        })
        .unwrap_or_else(|| Attributed::default_value(false));
    provenance.push((
        "security.allow_write_probes".to_owned(),
        write_probes.value.to_string(),
        write_probes.source,
    ));

    // Off unless the file says otherwise. There is no argument or environment
    // override: this setting decides whether network activity is disclosed to
    // event subscribers, and a one-shot flag is the wrong shape for a decision
    // that should be visible in the deployment's own configuration.
    let publish_logs = inputs
        .file
        .as_ref()
        .filter(|f| {
            f.security
                .as_ref()
                .and_then(|s| s.publish_mihomo_logs)
                .is_some()
        })
        .map(|f| Attributed::file(f.publish_mihomo_logs()))
        .unwrap_or_else(|| Attributed::default_value(false));
    provenance.push((
        "security.publish_mihomo_logs".to_owned(),
        publish_logs.value.to_string(),
        publish_logs.source,
    ));

    // --- the socket's peer credential -------------------------------------
    let allowed_uid = inputs
        .file
        .as_ref()
        .and_then(|f| f.agent.as_ref())
        .and_then(|a| a.allowed_uid)
        .map(Attributed::file)
        .unwrap_or_else(|| Attributed::default_value(0));
    let allowed_gid = inputs
        .file
        .as_ref()
        .and_then(|f| f.agent.as_ref())
        .and_then(|a| a.allowed_gid)
        .map(Attributed::file)
        .unwrap_or_else(|| Attributed::default_value(0));
    provenance.push((
        "agent.allowed_uid".to_owned(),
        // Zero means "no check"; the socket's file permissions are then the whole
        // boundary, which is the documented default.
        if allowed_uid.value == 0 {
            "<unset: socket permissions only>".to_owned()
        } else {
            allowed_uid.value.to_string()
        },
        allowed_uid.source,
    ));
    provenance.push((
        "agent.allowed_gid".to_owned(),
        if allowed_gid.value == 0 {
            "<unset: socket permissions only>".to_owned()
        } else {
            allowed_gid.value.to_string()
        },
        allowed_gid.source,
    ));

    // --- the network listener ---------------------------------------------
    // Absent means "socket only", which is the default and the safe state.
    let api_bind = inputs
        .file
        .as_ref()
        .and_then(|f| f.api_bind())
        .map(Attributed::file)
        .unwrap_or_else(|| Attributed::default_value(String::new()));
    provenance.push((
        "api.bind".to_owned(),
        if api_bind.value.is_empty() {
            "<unset: unix socket only>".to_owned()
        } else {
            api_bind.value.clone()
        },
        api_bind.source,
    ));

    let cors = inputs
        .file
        .as_ref()
        .map(|f| f.cors_origins())
        .filter(|origins| !origins.is_empty())
        .map(Attributed::file)
        .unwrap_or_else(|| Attributed::default_value(Vec::new()));
    provenance.push((
        "api.cors_origins".to_owned(),
        if cors.value.is_empty() {
            "<empty: no cross-origin requests>".to_owned()
        } else {
            cors.value.join(",")
        },
        cors.source,
    ));

    let config = RuntimeConfig {
        instance,
        controller,
        converter,
        paths,
        allow_write_probes: write_probes.value,
        kernel_binary: kernel_binary.value,
        kernel_data_dir: Some(kernel_data_dir.value),
        scratch_dir: Some(scratch_dir.value),
        subscription_allow: allow.value,
        socket_path: Some(socket.value),
        socket_allowed_uid: nonzero(allowed_uid.value),
        socket_allowed_gid: nonzero(allowed_gid.value),
        mihomo_secret: secret,
        publish_mihomo_logs: publish_logs.value,
        api_bind: (!api_bind.value.is_empty()).then_some(api_bind.value),
        cors_origins: cors.value,
    };

    Ok(Resolved {
        config,
        provenance,
        warning,
    })
}

/// A zero uid/gid means "unset", which is how the rest of the code reads it.
fn nonzero(value: u32) -> Option<u32> {
    if value == 0 { None } else { Some(value) }
}

/// Joins a root with a relative path.
fn join(root: &str, relative: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), relative)
}

/// Reads an environment variable, treating blank as absent.
fn env_string(suffix: &str) -> Option<String> {
    let name = format!("{}{suffix}", super::file::ENV_PREFIX);
    match std::env::var(&name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Reads a string out of the file, when one is present.
fn file_string(
    inputs: &Inputs,
    pick: impl Fn(&FileConfig) -> Option<String>,
) -> Option<Attributed<String>> {
    inputs.file.as_ref().and_then(pick).map(Attributed::file)
}

/// Resolves the kernel secret from the file.
///
/// # Errors
///
/// Returns [`BootstrapError::InvalidConfig`] when the field is present but empty.
/// An empty secret is not "unset": it is a value, and the kernel installs **no**
/// authentication at all when its secret is empty, so it is refused rather than
/// quietly replaced with a generated one.
fn resolve_secret_source(inputs: &Inputs) -> Result<(Option<String>, Source), BootstrapError> {
    let Some(file) = &inputs.file else {
        return Ok((None, Source::Default));
    };
    match file.secret_state() {
        SecretState::Absent => Ok((None, Source::Default)),
        SecretState::Empty => Err(BootstrapError::InvalidConfig(
            "kernel.secret is present but empty. An empty secret is not \"unset\": the kernel \
             installs no authentication at all when its secret is empty, which would expose \
             process control to anything that can reach the port. Remove the field to have the \
             agent generate and store one."
                .to_owned(),
        )),
        SecretState::Provided(value) => Ok((Some(value), Source::File)),
    }
}

/// Resolves the converter from the file.
///
/// # Errors
///
/// Returns [`BootstrapError::InvalidConfig`] when an unknown backend is named, or
/// when a backend is named without a base URL. Both are refused rather than
/// defaulted: silently falling back to "no converter" would leave subscription
/// updates failing for a reason the operator cannot see.
fn resolve_converter(inputs: &Inputs) -> Result<ConverterConfig, BootstrapError> {
    let Some(section) = inputs.file.as_ref().and_then(|f| f.converter.as_ref()) else {
        return Ok(ConverterConfig::None);
    };

    match section.backend.as_deref() {
        // An omitted backend with no other field is an empty section, which means
        // the same as omitting the section: no converter.
        None if section.base_url.is_none() => Ok(ConverterConfig::None),
        None => Err(BootstrapError::InvalidConfig(
            "[converter] sets base_url but no backend; name one, or remove the section".to_owned(),
        )),
        Some("substore") => {
            let base_url = section.base_url.clone().ok_or_else(|| {
                BootstrapError::InvalidConfig(
                    "[converter] backend \"substore\" requires base_url".to_owned(),
                )
            })?;
            Ok(ConverterConfig::External {
                base_url,
                // Absent means `false`. The default is the security-relevant
                // part: the converter backend has no authentication, so anything
                // that can reach it can read and write this agent's subscription
                // records.
                allow_non_loopback: section.allow_non_loopback.unwrap_or(false),
            })
        }
        Some(other) => Err(BootstrapError::InvalidConfig(format!(
            "[converter] backend {other:?} is not recognised; the only supported value is \
             \"substore\""
        ))),
    }
}

/// Interprets a controller value.
///
/// A value containing `/` is a socket path; anything else is `host:port`.
///
/// # Errors
///
/// Returns a reason when the value is empty, or when a socket path is relative.
/// A relative path is refused because this rule would otherwise misread it: a
/// bare word with no `/` is taken as `host:port`, so `mihomo.sock` would become a
/// host name and the failure would surface much later as a connection error.
pub fn parse_controller(value: &str) -> Result<ControllerEndpoint, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("the endpoint must not be empty".to_owned());
    }
    if trimmed.starts_with('/') {
        return Ok(ControllerEndpoint::UnixSocket(trimmed.to_owned()));
    }

    // Anything that is not an absolute path must look like `host:port`. The
    // check is explicit rather than assumed because the alternative — treating
    // any unrecognised value as a loopback address — turns a mistyped socket
    // path (`mihomo.sock`, `./mihomo.sock`) into a host name, and the failure
    // then surfaces much later as an unexplained connection error.
    let looks_like_address = match trimmed.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.parse::<u16>().is_ok(),
        None => false,
    };
    if !looks_like_address {
        return Err(format!(
            "{trimmed:?} is neither an absolute socket path (it must start with `/`) \
             nor a `host:port` address"
        ));
    }

    Ok(ControllerEndpoint::Loopback {
        address: trimmed.to_owned(),
    })
}

/// Reads the environment variables this binary understands.
///
/// Gathered here, in one place, so that the set of variables is a thing that can
/// be read rather than grepped for.
#[must_use]
pub fn inputs_from_environment() -> Inputs {
    Inputs {
        instance: env_string("INSTANCE"),
        root: None,
        controller: env_string("CONTROLLER"),
        kernel_binary: env_string("KERNEL_BINARY"),
        subscription_allow: std::env::var(format!("{}SUBSCRIPTION_ALLOW", super::file::ENV_PREFIX))
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            }),
        allow_write_probes: std::env::var(format!("{}ALLOW_WRITE_PROBES", super::file::ENV_PREFIX))
            .ok()
            .map(|v| {
                // Only an explicit affirmative enables it. Anything else — a
                // typo, `0`, `no`, an empty value — leaves it off, because the
                // failure mode of guessing "yes" is mutating the host.
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            }),
        file: None,
    }
}

/// Loads the file and merges everything.
///
/// # Errors
///
/// Returns [`BootstrapError::InvalidConfig`] when the file cannot be read, has
/// permissions that could leak a secret, does not parse, or contains a value that
/// cannot be used.
pub fn load_and_merge(path: &Path, mut inputs: Inputs) -> Result<Resolved, BootstrapError> {
    let file = file::load(path).map_err(|e| BootstrapError::InvalidConfig(e.to_string()))?;
    inputs.file = file;
    merge(&inputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(inputs: &Inputs) -> Resolved {
        merge(inputs).expect("merge must succeed")
    }

    #[test]
    fn with_no_inputs_at_all_every_value_is_a_default() {
        let resolved = merged(&Inputs::default());
        assert_eq!(resolved.config.instance.as_str(), "default");
        assert_eq!(
            resolved.config.paths.configs_dir,
            "/var/lib/proxy-agent/configs"
        );
        assert_eq!(
            resolved.config.agent_socket_path(),
            "/run/proxy-agent/agent.sock"
        );
        assert_eq!(
            resolved.config.controller,
            ControllerEndpoint::UnixSocket("/run/proxy-agent/mihomo.sock".to_owned())
        );
        assert_eq!(resolved.config.converter, ConverterConfig::None);
        assert!(resolved.config.mihomo_secret.is_none());
        assert!(!resolved.config.allow_write_probes);
        assert!(resolved.warning.is_none());
        assert!(
            resolved
                .provenance
                .iter()
                .all(|(_, _, s)| *s == Source::Default),
            "nothing was configured, so every source must be the default"
        );
    }

    /// The precedence rule, asserted field by field. This is the test that makes
    /// "flag beats file" a fact rather than a claim.
    #[test]
    fn an_argument_outranks_the_file() {
        let file = FileConfig::parse(
            "[agent]\ninstance = \"from-file\"\n\n[kernel]\nbinary = \"/file/mihomo\"\n",
        )
        .expect("parse");
        let inputs = Inputs {
            instance: Some("from-argument".to_owned()),
            kernel_binary: Some("/argument/mihomo".to_owned()),
            file: Some(file),
            ..Inputs::default()
        };
        let resolved = merged(&inputs);
        assert_eq!(resolved.config.instance.as_str(), "from-argument");
        assert_eq!(resolved.config.kernel_binary, "/argument/mihomo");
    }

    #[test]
    fn the_file_supplies_a_value_when_no_argument_does() {
        let file = FileConfig::parse(
            "[agent]\ninstance = \"from-file\"\n\n[kernel]\nbinary = \"/file/mihomo\"\n",
        )
        .expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        assert_eq!(resolved.config.instance.as_str(), "from-file");
        assert_eq!(resolved.config.kernel_binary, "/file/mihomo");
        // And the source is reported as the file, not the default.
        let source = resolved
            .provenance
            .iter()
            .find(|(name, _, _)| name == "kernel.binary")
            .map(|(_, _, s)| *s);
        assert_eq!(source, Some(Source::File));
    }

    /// A relocated layout moves the kernel binary with it.
    ///
    /// This is the case a real run caught: a file naming only `[paths]` left the
    /// binary at `/usr/lib/proxy-agent/mihomo`, so the agent tried to create the
    /// *system* directory on a host that keeps everything under a custom root.
    /// Refused on the test machine, which is the good outcome — the bad one is
    /// succeeding against a real installation.
    #[test]
    fn relocating_the_paths_moves_the_kernel_binary_out_of_the_system_path() {
        let file = FileConfig::parse(
            "[paths]\nconfigs_dir = \"/srv/pc/lib/configs\"\nstate_dir = \"/srv/pc/lib/state\"\n\
             run_dir = \"/srv/pc/run\"\n",
        )
        .expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        assert_eq!(resolved.config.kernel_binary, "/srv/pc/lib/bin/mihomo");
        // The kernel's own directories follow the same root, so one directory
        // holds everything this agent owns.
        assert_eq!(resolved.config.kernel_data_dir(), "/srv/pc/lib/mihomo");
        assert_eq!(resolved.config.scratch_dir(), "/srv/pc/lib/scratch");
    }

    /// A file that does *not* relocate the layout keeps the documented system
    /// path: the derivation must fire on relocation, not on the file's presence.
    #[test]
    fn a_file_that_does_not_relocate_keeps_the_system_kernel_path() {
        let file = FileConfig::parse("[agent]\ninstance = \"edge\"\n").expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        assert_eq!(resolved.config.kernel_binary, "/usr/lib/proxy-agent/mihomo");
    }

    /// An explicit binary wins over the derivation, in both directions.
    #[test]
    fn an_explicit_binary_wins_over_a_relocated_layout() {
        let file = FileConfig::parse(
            "[paths]\nstate_dir = \"/srv/pc/lib/state\"\n\n[kernel]\nbinary = \"/opt/mihomo\"\n",
        )
        .expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        assert_eq!(resolved.config.kernel_binary, "/opt/mihomo");
    }

    /// A root moves the kernel binary too, out of the system path. Without this
    /// a development run tries to create `/usr/lib/proxy-agent`, which is either
    /// refused or — far worse — succeeds against a real installation.
    #[test]
    fn a_root_moves_the_kernel_binary_out_of_the_system_path() {
        let resolved = merged(&Inputs {
            root: Some("/tmp/dev".to_owned()),
            ..Inputs::default()
        });
        assert_eq!(resolved.config.kernel_binary, "/tmp/dev/bin/mihomo");

        // And an explicit binary still wins over the root.
        let explicit = merged(&Inputs {
            root: Some("/tmp/dev".to_owned()),
            kernel_binary: Some("/opt/mihomo".to_owned()),
            ..Inputs::default()
        });
        assert_eq!(explicit.config.kernel_binary, "/opt/mihomo");
    }

    /// A root moves every path together, including the sockets.
    #[test]
    fn a_root_moves_all_paths_and_both_sockets() {
        let resolved = merged(&Inputs {
            root: Some("/tmp/dev".to_owned()),
            ..Inputs::default()
        });
        assert_eq!(resolved.config.paths.configs_dir, "/tmp/dev/lib/configs");
        assert_eq!(resolved.config.paths.state_dir, "/tmp/dev/lib/state");
        assert_eq!(resolved.config.paths.run_dir, "/tmp/dev/run");
        assert_eq!(
            resolved.config.agent_socket_path(),
            "/tmp/dev/run/agent.sock"
        );
        assert_eq!(
            resolved.config.controller,
            ControllerEndpoint::UnixSocket("/tmp/dev/run/mihomo.sock".to_owned())
        );
    }

    /// The loopback controller needs a secret, and the file is the way to provide
    /// one. The warning is what keeps a store-managed value from being silently
    /// overridden.
    #[test]
    fn a_file_secret_is_used_and_warns_that_the_stored_value_is_ignored() {
        let file = FileConfig::parse(
            "[controller]\nendpoint = \"127.0.0.1:9090\"\n\n[kernel]\nsecret = \"hunter2\"\n",
        )
        .expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        assert_eq!(resolved.config.mihomo_secret.as_deref(), Some("hunter2"));
        assert_eq!(
            resolved.config.controller,
            ControllerEndpoint::Loopback {
                address: "127.0.0.1:9090".to_owned()
            }
        );
        let warning = resolved.warning.as_deref().expect("a warning is required");
        assert!(warning.contains("ignored"), "{warning}");
    }

    /// The absent case must stay absent, so the store-generated path still works.
    #[test]
    fn an_absent_secret_is_not_invented() {
        let file =
            FileConfig::parse("[controller]\nendpoint = \"127.0.0.1:9090\"\n").expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        assert!(
            resolved.config.mihomo_secret.is_none(),
            "an absent field must leave resolution to the store"
        );
    }

    /// An empty secret is refused, because it disables kernel authentication.
    #[test]
    fn an_empty_secret_is_refused() {
        for text in ["[kernel]\nsecret = \"\"\n", "[kernel]\nsecret = \"    \"\n"] {
            let file = FileConfig::parse(text).expect("parse");
            let err = merge(&Inputs {
                file: Some(file),
                ..Inputs::default()
            })
            .expect_err("an empty secret must be refused");
            let message = err.to_string();
            assert!(message.contains("empty"), "{message}");
        }
    }

    /// The report never prints the secret itself: a report is read on a terminal,
    /// kept in scrollback, and pasted into issues.
    #[test]
    fn the_report_never_prints_the_secret_value() {
        let file = FileConfig::parse("[kernel]\nsecret = \"SUPERSECRET\"\n").expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        let report = resolved.report(Path::new("/etc/proxy-agent/config.toml"), true);
        assert!(
            !report.contains("SUPERSECRET"),
            "the secret leaked: {report}"
        );
        assert!(report.contains("<set>"), "{report}");
    }

    #[test]
    fn a_relative_socket_path_is_refused() {
        for value in [
            "mihomo.sock",
            "./mihomo.sock",
            "../run/mihomo.sock",
            // An `host:port` with a non-numeric port is a typo, not an address.
            "127.0.0.1:mihomo",
            "127.0.0.1:",
        ] {
            let err = parse_controller(value).expect_err("must be refused");
            assert!(
                err.contains("absolute") || err.contains("host:port"),
                "{value}: {err}"
            );
        }
    }

    #[test]
    fn an_absolute_socket_path_and_a_loopback_address_are_distinguished() {
        assert_eq!(
            parse_controller("/run/proxy-agent/mihomo.sock").expect("ok"),
            ControllerEndpoint::UnixSocket("/run/proxy-agent/mihomo.sock".to_owned())
        );
        assert_eq!(
            parse_controller("127.0.0.1:9090").expect("ok"),
            ControllerEndpoint::Loopback {
                address: "127.0.0.1:9090".to_owned()
            }
        );
        assert!(parse_controller("").is_err());
        assert!(parse_controller("   ").is_err());
    }

    #[test]
    fn a_converter_section_is_recognised_or_refused() {
        let good = FileConfig::parse(
            "[converter]\nbackend = \"substore\"\nbase_url = \"http://127.0.0.1:3001\"\n",
        )
        .expect("parse");
        let resolved = merged(&Inputs {
            file: Some(good),
            ..Inputs::default()
        });
        assert_eq!(
            resolved.config.converter,
            ConverterConfig::External {
                base_url: "http://127.0.0.1:3001".to_owned(),
                allow_non_loopback: false,
            }
        );

        let unknown = FileConfig::parse("[converter]\nbackend = \"clash\"\n").expect("parse");
        let err = merge(&Inputs {
            file: Some(unknown),
            ..Inputs::default()
        })
        .expect_err("an unknown backend must be refused");
        assert!(err.to_string().contains("clash"), "{err}");

        let missing_url =
            FileConfig::parse("[converter]\nbackend = \"substore\"\n").expect("parse");
        let err = merge(&Inputs {
            file: Some(missing_url),
            ..Inputs::default()
        })
        .expect_err("a backend without a URL must be refused");
        assert!(err.to_string().contains("base_url"), "{err}");
    }

    /// The loopback opt-in is `false` unless the file says otherwise, which is
    /// the security-relevant default.
    #[test]
    fn the_converter_loopback_opt_in_defaults_to_refused_and_can_be_set() {
        let opted_in = FileConfig::parse(
            "[converter]\nbackend = \"substore\"\nbase_url = \"http://10.0.0.5:3001\"\n\
             allow_non_loopback = true\n",
        )
        .expect("parse");
        let resolved = merged(&Inputs {
            file: Some(opted_in),
            ..Inputs::default()
        });
        assert_eq!(
            resolved.config.converter,
            ConverterConfig::External {
                base_url: "http://10.0.0.5:3001".to_owned(),
                allow_non_loopback: true,
            }
        );
    }

    #[test]
    fn an_invalid_instance_name_is_refused_and_names_its_source() {
        let err = merge(&Inputs {
            instance: Some(String::new()),
            ..Inputs::default()
        })
        .expect_err("must be refused");
        let message = err.to_string();
        assert!(message.contains("instance"), "{message}");
    }

    /// The allow-list and the probe flag come from the file when no argument does.
    #[test]
    fn security_settings_come_from_the_file() {
        let file = FileConfig::parse(
            "[security]\nsubscription_allow = [\"10.0.0.0/8\"]\nallow_write_probes = true\n",
        )
        .expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        assert_eq!(
            resolved.config.subscription_allow,
            vec!["10.0.0.0/8".to_owned()]
        );
        assert!(resolved.config.allow_write_probes);
    }

    /// An argument allow-list replaces the file's rather than appending to it: a
    /// union would make it impossible to narrow the list from the command line.
    #[test]
    fn an_argument_allow_list_replaces_rather_than_extends() {
        let file = FileConfig::parse("[security]\nsubscription_allow = [\"10.0.0.0/8\"]\n")
            .expect("parse");
        let resolved = merged(&Inputs {
            subscription_allow: Some(vec!["192.168.0.0/16".to_owned()]),
            file: Some(file),
            ..Inputs::default()
        });
        assert_eq!(
            resolved.config.subscription_allow,
            vec!["192.168.0.0/16".to_owned()]
        );
    }

    /// An unset uid/gid means "no peer check", which is what zero maps to.
    #[test]
    fn the_peer_credential_defaults_to_no_check() {
        let resolved = merged(&Inputs::default());
        assert_eq!(resolved.config.socket_allowed_uid, None);
        assert_eq!(resolved.config.socket_allowed_gid, None);

        let file =
            FileConfig::parse("[agent]\nallowed_uid = 1000\nallowed_gid = 1000\n").expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        assert_eq!(resolved.config.socket_allowed_uid, Some(1000));
        assert_eq!(resolved.config.socket_allowed_gid, Some(1000));
    }

    #[test]
    fn the_report_names_every_field_with_its_source() {
        let file = FileConfig::parse("[agent]\ninstance = \"edge\"\n").expect("parse");
        let resolved = merged(&Inputs {
            file: Some(file),
            ..Inputs::default()
        });
        let report = resolved.report(Path::new("/etc/proxy-agent/config.toml"), true);
        assert!(report.contains("/etc/proxy-agent/config.toml"), "{report}");
        assert!(report.contains("loaded"), "{report}");
        assert!(report.contains("instance"), "{report}");
        assert!(report.contains("[file]"), "{report}");
        assert!(report.contains("[default]"), "{report}");
    }

    #[test]
    fn the_report_says_when_no_file_was_found() {
        let resolved = merged(&Inputs::default());
        let report = resolved.report(Path::new("/etc/proxy-agent/config.toml"), false);
        assert!(report.contains("not found"), "{report}");
    }
}
