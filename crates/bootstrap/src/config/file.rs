//! The deployment configuration file.
//!
//! # Why this is a separate layer from [`crate::RuntimeConfig`]
//!
//! [`RuntimeConfig`](crate::RuntimeConfig) is the *composed* form: every field is
//! resolved, and nothing is optional. This module is the *file* form: every
//! section may be absent, and the interesting work is deciding what absence means.
//!
//! Keeping them apart is what lets the merge be tested without a filesystem, and
//! what keeps "the file has no `[kernel]` section" from becoming a question the
//! composition root has to answer.
//!
//! # Unknown fields are refused
//!
//! `deny_unknown_fields` is on for every struct. A mistyped key — `socket_path`
//! where the schema says `socket` — would otherwise be ignored, and the operator
//! would believe their change took effect. That is the worst kind of
//! misconfiguration, because the system looks healthy while ignoring intent.
//!
//! This is deliberately *stricter* than how the kernel's own configuration is
//! treated (ADR-008): `mihomo -t` accepts unknown fields and we only report them.
//! The difference is ownership. That is someone else's schema, so we have no
//! standing to reject it; this is ours, so we do.
//!
//! # Absence versus an empty value
//!
//! A section or field that is *absent* means "not configured". A field that is
//! *present* but empty is a value, and the two are not conflated — see
//! [`FileConfig::secret_state`], where the distinction decides whether the kernel
//! gets a generated secret or none at all.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The `/etc/proxy-agent/config.toml` path.
///
/// The one place this constant is defined. The systemd unit, the packaging
/// scripts, and the loader all name the same file, and a second literal would be
/// a second answer to "where is the configuration".
pub const DEFAULT_CONFIG_PATH: &str = "/etc/proxy-agent/config.toml";

/// The environment-variable prefix.
///
/// Every variable this binary reads starts with it, so a deployment can be
/// inspected by listing one prefix rather than by reading the source.
pub const ENV_PREFIX: &str = "PROXYCTL_";

/// A configuration file, as written on disk.
///
/// Every section is optional. An empty file is valid and yields all defaults,
/// which is what makes "I only want to change one field" work without copying a
/// full example.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    /// The agent's own settings.
    #[serde(default)]
    pub agent: Option<AgentSection>,
    /// Where state lives.
    #[serde(default)]
    pub paths: Option<PathsSection>,
    /// The kernel binary and its directories.
    #[serde(default)]
    pub kernel: Option<KernelSection>,
    /// How to reach the kernel's control API.
    #[serde(default)]
    pub controller: Option<ControllerSection>,
    /// How to reach the subscription converter.
    #[serde(default)]
    pub converter: Option<ConverterSection>,
    /// Outbound and probe policy.
    #[serde(default)]
    pub security: Option<SecuritySection>,
    /// The network listener.
    #[serde(default)]
    pub api: Option<ApiSection>,
}

/// The `[api]` section.
///
/// Omitting the whole section keeps the agent on the unix socket only, which is
/// the documented default: a socket cannot be reached from another machine, so it
/// cannot be exposed by accident. Listening on a port is an explicit act.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSection {
    /// The address to listen on, such as `0.0.0.0:8765` or `127.0.0.1:8765`.
    pub bind: Option<String>,
    /// Origins permitted to call the API from a browser.
    ///
    /// Empty means no cross-origin request is allowed, which is the safe default:
    /// a same-origin page still works, and a browser blocks the rest. Entries are
    /// matched exactly; there is no wildcard, because a wildcard here means "any
    /// site may drive this agent".
    pub cors_origins: Option<Vec<String>>,
}

/// The `[agent]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    /// Which instance to compose for.
    pub instance: Option<String>,
    /// Where the agent accepts management requests.
    pub socket: Option<String>,
    /// A uid the socket's peer credential must present.
    pub allowed_uid: Option<u32>,
    /// A gid the socket's peer credential must present.
    pub allowed_gid: Option<u32>,
}

/// The `[paths]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathsSection {
    /// Immutable configuration versions.
    pub configs_dir: Option<String>,
    /// Mutable state, including the active-version pointer and the database.
    pub state_dir: Option<String>,
    /// Runtime sockets and lock files.
    pub run_dir: Option<String>,
}

/// The `[kernel]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelSection {
    /// Absolute path to the kernel binary.
    pub binary: Option<String>,
    /// The kernel's data directory.
    pub data_dir: Option<String>,
    /// Where temporary artifacts are created.
    pub scratch_dir: Option<String>,
    /// The kernel controller's shared secret.
    ///
    /// Three states, and they differ (see [`FileConfig::secret_state`]):
    /// absent means "generate one if needed", present-and-empty is **refused**
    /// because an empty secret disables kernel authentication entirely, and
    /// present-and-non-empty is used as given.
    pub secret: Option<String>,
}

/// The `[controller]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerSection {
    /// The endpoint. A value containing `/` is a socket path; otherwise it is
    /// `host:port`.
    pub endpoint: Option<String>,
}

/// The `[converter]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConverterSection {
    /// The converter backend. Only `substore` is recognised; omitting the whole
    /// section means no converter, which is a supported deployment.
    pub backend: Option<String>,
    /// Base URL of the converter service.
    pub base_url: Option<String>,
    /// Whether a non-loopback base URL is acceptable.
    ///
    /// Defaults to `false`, and that default is the security-relevant part: the
    /// converter backend has no authentication, so anything able to reach it can
    /// read and write this agent's subscription records.
    pub allow_non_loopback: Option<bool>,
}

/// The `[security]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecuritySection {
    /// Outbound hosts or CIDR blocks a subscription fetch may reach.
    pub subscription_allow: Option<Vec<String>>,
    /// Whether to run capability probes that write to the host.
    pub allow_write_probes: Option<bool>,
    /// Whether the agent reads the kernel's logs continuously and republishes
    /// them as events.
    ///
    /// Off by default, and the default is the important part. Turning it on means
    /// the agent reads and redacts every kernel log line whether or not anyone is
    /// watching, and that anyone permitted to subscribe to the event stream sees
    /// the agent's network activity — which hosts, which DNS answers, which rules
    /// matched — even after credentials are stripped. That is a real disclosure,
    /// so it takes an explicit opt-in rather than arriving switched on.
    pub publish_mihomo_logs: Option<bool>,
}

/// What the file says about the kernel secret.
///
/// A three-way distinction rather than `Option<String>`, because the middle case
/// is the dangerous one and must not be reachable by accident. Collapsing
/// "absent" and "present but empty" into `None` would silently turn a user's
/// empty value into "generate one", and collapsing them the other way would let
/// an empty value through to the kernel, where it disables authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretState {
    /// The field is absent: generate a secret from the store if one is needed.
    Absent,
    /// The field is present and empty. Refused, not defaulted.
    Empty,
    /// The field is present with a value.
    Provided(String),
}

impl FileConfig {
    /// Parses a configuration file.
    ///
    /// # Errors
    ///
    /// Returns [`FileConfigError::Syntax`] with the parser's message, which
    /// includes the line and column, so a typo can be found without guessing.
    pub fn parse(text: &str) -> Result<Self, FileConfigError> {
        toml::from_str(text).map_err(|e| FileConfigError::Syntax {
            reason: e.to_string(),
        })
    }

    /// Reads and parses a file.
    ///
    /// # Errors
    ///
    /// Returns [`FileConfigError::Unreadable`] when the file cannot be read and
    /// [`FileConfigError::Syntax`] when it cannot be parsed. A *missing* file is
    /// not an error here — see [`load`].
    pub fn read(path: &Path) -> Result<Self, FileConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| FileConfigError::Unreadable {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        Self::parse(&text)
    }

    /// What the file says about the kernel secret.
    #[must_use]
    pub fn secret_state(&self) -> SecretState {
        match self.kernel.as_ref().and_then(|k| k.secret.as_deref()) {
            None => SecretState::Absent,
            Some(value) if value.trim().is_empty() => SecretState::Empty,
            Some(value) => SecretState::Provided(value.to_owned()),
        }
    }

    /// The `[security]` allow-list, or an empty one.
    #[must_use]
    pub fn subscription_allow(&self) -> Vec<String> {
        self.security
            .as_ref()
            .and_then(|s| s.subscription_allow.clone())
            .unwrap_or_default()
    }

    /// Whether write-capable probes are enabled.
    ///
    /// Defaults to `false`: a write probe can create devices or touch firewall
    /// state, so it needs an explicit opt-in.
    #[must_use]
    pub fn allow_write_probes(&self) -> bool {
        self.security
            .as_ref()
            .and_then(|s| s.allow_write_probes)
            .unwrap_or(false)
    }

    /// Whether kernel logs are republished as events.
    ///
    /// Defaults to `false`: see the field's own documentation for why the default
    /// matters more than the setting.
    #[must_use]
    pub fn publish_mihomo_logs(&self) -> bool {
        self.security
            .as_ref()
            .and_then(|s| s.publish_mihomo_logs)
            .unwrap_or(false)
    }

    /// The address to listen on, when the deployment asked for one.
    #[must_use]
    pub fn api_bind(&self) -> Option<String> {
        self.api
            .as_ref()
            .and_then(|a| a.bind.clone())
            .filter(|b| !b.trim().is_empty())
    }

    /// The permitted browser origins.
    #[must_use]
    pub fn cors_origins(&self) -> Vec<String> {
        self.api
            .as_ref()
            .and_then(|a| a.cors_origins.clone())
            .unwrap_or_default()
    }
}

/// Why a configuration file could not be used.
#[derive(Debug, thiserror::Error)]
pub enum FileConfigError {
    /// The file could not be read.
    #[error("cannot read {path}: {reason}")]
    Unreadable {
        /// The path that was tried.
        path: String,
        /// Why it failed.
        reason: String,
    },

    /// The file is not valid TOML, or has a key the schema does not define.
    #[error("invalid configuration: {reason}")]
    Syntax {
        /// The parser's message, which names the line and column.
        reason: String,
    },

    /// The file's permissions permit more access than a file holding a secret may.
    #[error(
        "refusing {path}: mode {mode:o} grants access to group or other, and this file may hold \
         the kernel secret. Set it to 0600 (chmod 600 {path})"
    )]
    Permissions {
        /// The path that was checked.
        path: String,
        /// The mode that was found.
        mode: u32,
    },
}

/// Reads a configuration file, applying the permission check.
///
/// # Why a missing file is not an error
///
/// A development run rooted at a temporary directory should not have to create a
/// file. And for a system deployment, the unit passes `--config` explicitly: if
/// that path is wrong, the failure is an unreadable file rather than a missing
/// one, which is reported either way. Treating absence as fatal would break the
/// first case to catch nothing in the second.
///
/// # Errors
///
/// Returns [`FileConfigError::Unreadable`] when the file exists and cannot be
/// read, [`FileConfigError::Permissions`] when its mode is too permissive, and
/// [`FileConfigError::Syntax`] when it does not parse.
pub fn load(path: &Path) -> Result<Option<FileConfig>, FileConfigError> {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            check_permissions(path, &metadata)?;
            FileConfig::read(path).map(Some)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FileConfigError::Unreadable {
            path: path.display().to_string(),
            reason: e.to_string(),
        }),
    }
}

/// Refuses a configuration file that any group or other bit is set on.
///
/// # Why this is a refusal rather than a warning
///
/// The file can hold `mihomo_secret` (ADR-010 D8), and in a loopback deployment
/// that value is the only credential protecting process control: the kernel does
/// not authenticate over its socket, and it installs **no** authentication at all
/// when the secret is empty. A world-readable file containing it therefore hands
/// kernel control to any local user.
///
/// When the secret is absent the file is not itself a credential store, but the
/// check still applies: a mode that *would* leak a secret is indistinguishable
/// from one that would leak it after the operator adds the field, and a rule that
/// depends on the current contents is a rule nobody can verify at a glance.
///
/// The one exception is the process's own uid: a file we can read anyway.
#[cfg(unix)]
fn check_permissions(path: &Path, metadata: &std::fs::Metadata) -> Result<(), FileConfigError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode() & 0o777;
    // Any group or other bit at all: read, write, or execute. `0600` and `0400`
    // pass; `0640`, `0644`, and `0666` do not.
    if mode & 0o077 != 0 {
        return Err(FileConfigError::Permissions {
            path: path.display().to_string(),
            mode,
        });
    }
    Ok(())
}

/// Windows has no comparable mode bits, so the check does not apply there.
///
/// Present rather than absent so the crate stays portable: the agent targets
/// Linux, but the loader is compiled in tests on other platforms.
#[cfg(not(unix))]
fn check_permissions(_path: &Path, _metadata: &std::fs::Metadata) -> Result<(), FileConfigError> {
    Ok(())
}

/// The path a configuration file should be read from, in precedence order.
///
/// The explicit argument wins; then the environment; then the documented default.
/// Returning the default rather than `None` means the caller has exactly one path
/// to report, which matters when the failure is "I edited a different file".
#[must_use]
pub fn resolve_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Ok(from_env) = std::env::var(format!("{ENV_PREFIX}CONFIG"))
        && !from_env.trim().is_empty()
    {
        return PathBuf::from(from_env);
    }
    PathBuf::from(DEFAULT_CONFIG_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_valid_and_yields_no_sections() {
        let config = FileConfig::parse("").expect("empty must parse");
        assert_eq!(config, FileConfig::default());
        assert_eq!(config.secret_state(), SecretState::Absent);
        assert!(config.subscription_allow().is_empty());
        assert!(!config.allow_write_probes());
    }

    /// A file naming only one field must not require the rest.
    #[test]
    fn a_partial_file_is_valid() {
        let config = FileConfig::parse("[agent]\ninstance = \"edge\"\n").expect("parse");
        assert_eq!(
            config.agent.as_ref().and_then(|a| a.instance.as_deref()),
            Some("edge")
        );
        assert!(config.paths.is_none());
        assert!(config.kernel.is_none());
    }

    /// A mistyped key is refused rather than ignored, because ignoring it makes
    /// the system look healthy while disregarding the operator's intent.
    #[test]
    fn an_unknown_field_is_refused() {
        let err = FileConfig::parse("[agent]\nsocket_path = \"/tmp/x\"\n")
            .expect_err("a mistyped key must be refused");
        let text = err.to_string();
        assert!(text.contains("socket_path"), "{text}");
    }

    /// The same applies to an entirely unknown section.
    #[test]
    fn an_unknown_section_is_refused() {
        let err = FileConfig::parse("[firewall]\nmode = \"nft\"\n").expect_err("must be refused");
        assert!(err.to_string().contains("firewall"), "{err}");
    }

    /// The syntax error names a line, because that is what makes a typo findable.
    #[test]
    fn a_syntax_error_reports_its_location() {
        let err = FileConfig::parse("[agent\ninstance = 1\n").expect_err("must fail");
        let text = err.to_string();
        assert!(text.contains("line") || text.contains("expected"), "{text}");
    }

    /// The three secret states are distinct, and the middle one is not `None`.
    #[test]
    fn the_secret_field_has_three_distinct_states() {
        let absent = FileConfig::parse("[kernel]\nbinary = \"/x\"\n").expect("parse");
        assert_eq!(absent.secret_state(), SecretState::Absent);

        let empty = FileConfig::parse("[kernel]\nsecret = \"\"\n").expect("parse");
        assert_eq!(empty.secret_state(), SecretState::Empty);

        let whitespace = FileConfig::parse("[kernel]\nsecret = \"   \"\n").expect("parse");
        assert_eq!(
            whitespace.secret_state(),
            SecretState::Empty,
            "whitespace is empty for this purpose: the kernel trims it too"
        );

        let provided = FileConfig::parse("[kernel]\nsecret = \"hunter2\"\n").expect("parse");
        assert_eq!(
            provided.secret_state(),
            SecretState::Provided("hunter2".to_owned())
        );
    }

    /// The converter section's loopback default is security-relevant, so its
    /// absent value is asserted rather than assumed.
    #[test]
    fn the_converter_section_records_its_opt_in() {
        let config = FileConfig::parse(
            "[converter]\nbackend = \"substore\"\nbase_url = \"http://127.0.0.1:3001\"\n",
        )
        .expect("parse");
        let section = config.converter.expect("section");
        assert_eq!(section.backend.as_deref(), Some("substore"));
        assert_eq!(
            section.allow_non_loopback, None,
            "an omitted opt-in stays `None` so the merge can supply `false`; \
             defaulting it here would hide whether the operator asked for it"
        );
    }

    #[test]
    fn security_defaults_are_the_restrictive_ones() {
        let config = FileConfig::parse("[security]\n").expect("parse");
        assert!(config.subscription_allow().is_empty());
        assert!(!config.allow_write_probes());

        let opted_in = FileConfig::parse("[security]\nallow_write_probes = true\n").expect("parse");
        assert!(opted_in.allow_write_probes());
    }

    #[test]
    fn the_allow_list_is_read_in_order() {
        let config = FileConfig::parse(
            "[security]\nsubscription_allow = [\"10.0.0.0/8\", \"subs.internal\"]\n",
        )
        .expect("parse");
        assert_eq!(
            config.subscription_allow(),
            vec!["10.0.0.0/8".to_owned(), "subs.internal".to_owned()]
        );
    }

    /// The explicit path wins, and the default is returned rather than `None` so
    /// the failure message always names a file.
    #[test]
    fn the_path_resolves_explicit_then_environment_then_default() {
        assert_eq!(
            resolve_path(Some(Path::new("/tmp/explicit.toml"))),
            PathBuf::from("/tmp/explicit.toml")
        );
        if std::env::var(format!("{ENV_PREFIX}CONFIG")).is_err() {
            assert_eq!(resolve_path(None), PathBuf::from(DEFAULT_CONFIG_PATH));
        }
    }
}
