//! Turning `agent run`'s arguments into a composed configuration.
//!
//! # Why this is a thin layer now
//!
//! This module used to build a [`RuntimeConfig`] by hand from flags, which meant
//! the precedence rules lived here for flags and would have had to be written a
//! second time for the configuration file. Both paths now go through
//! [`proxy_bootstrap::config::merge`], so "a flag beats the file" is a property of
//! one function rather than of two that could drift.
//!
//! What is left here is the translation from *argv* to merge *inputs*: deciding
//! which arguments were actually supplied. An absent flag must stay absent rather
//! than becoming its default, because the merge has to be able to tell "the
//! operator typed the default" from "nobody set this" — that distinction is what
//! `--print-config` reports.

use std::path::PathBuf;

use proxy_bootstrap::RuntimeConfig;
use proxy_bootstrap::config::file;
use proxy_bootstrap::config::merge::{self, Inputs};

use crate::args::AgentRunArgs;

/// The outcome of preparing a configuration, or a request to print and exit.
#[derive(Debug)]
pub enum Prepared {
    /// Serve with this configuration.
    Serve(Box<RuntimeConfig>),
    /// Print this report and exit successfully.
    Print(String),
}

/// Prepares the configuration for `agent run`.
///
/// # Errors
///
/// Returns a message when the file cannot be used — unreadable, too permissive,
/// malformed, or holding a value that cannot be composed — or when an argument is
/// unusable. The caller turns that into a startup failure; none of these is
/// recoverable by guessing.
pub fn prepare(args: &AgentRunArgs) -> Result<Prepared, String> {
    let path = file::resolve_path(args.config.as_deref());
    let loaded = file::load(&path).map_err(|e| e.to_string())?;
    let was_loaded = loaded.is_some();

    let mut inputs = Inputs {
        file: loaded,
        ..Inputs::default()
    };

    // Only what was actually supplied. An absent flag stays `None` so the merge
    // can fall through to the file, and so the report can say which source won.
    inputs.instance = args.instance.clone();
    inputs.root = args.root.as_ref().map(|p| p.display().to_string());
    inputs.controller = args.controller.clone();
    inputs.kernel_binary = args.kernel_binary.clone();
    // An empty list means "the flag was not given": clap cannot express `--allow`
    // with no value, so it rejects that as a usage error before reaching here and
    // emptiness is unambiguous.
    inputs.subscription_allow = if args.allow.is_empty() {
        None
    } else {
        Some(args.allow.clone())
    };
    // Only an explicit affirmative is forwarded. Forwarding `false` would let an
    // absent flag override a `true` in the file, and the failure mode of getting
    // this backwards is silently disabling a probe the operator asked for.
    inputs.allow_write_probes = if args.allow_write_probes {
        Some(true)
    } else {
        None
    };

    let resolved = merge::merge(&inputs).map_err(|e| e.to_string())?;

    if args.print_config {
        return Ok(Prepared::Print(resolved.report(&path, was_loaded)));
    }

    // A warning is printed rather than swallowed, because it names a conflict the
    // operator cannot otherwise see: a file-provided secret overriding a stored
    // one produces no error, only a difference from what they expect.
    if let Some(warning) = &resolved.warning {
        eprintln!("proxyctl: warning: {warning}");
    }

    Ok(Prepared::Serve(Box::new(resolved.config)))
}

/// The configuration file a run would read.
#[must_use]
pub fn config_path(args: &AgentRunArgs) -> PathBuf {
    file::resolve_path(args.config.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_bootstrap::ControllerEndpoint;

    fn args() -> AgentRunArgs {
        AgentRunArgs {
            config: None,
            print_config: false,
            instance: None,
            root: None,
            controller: None,
            kernel_binary: None,
            allow: Vec::new(),
            allow_write_probes: false,
        }
    }

    fn serve(args: &AgentRunArgs) -> RuntimeConfig {
        match prepare(args).expect("prepare must succeed") {
            Prepared::Serve(config) => *config,
            Prepared::Print(_) => panic!("expected a configuration, not a report"),
        }
    }

    #[test]
    fn with_no_flags_and_no_file_everything_defaults() {
        let config = serve(&args());
        assert_eq!(config.instance.as_str(), "default");
        assert_eq!(config.paths.configs_dir, "/var/lib/proxy-agent/configs");
        assert_eq!(config.agent_socket_path(), "/run/proxy-agent/agent.sock");
    }

    /// A root still moves every path, including both sockets.
    #[test]
    fn a_root_moves_every_path() {
        let mut args = args();
        args.root = Some(PathBuf::from("/tmp/proxyctl-dev"));
        let config = serve(&args);
        assert_eq!(config.paths.configs_dir, "/tmp/proxyctl-dev/lib/configs");
        assert_eq!(
            config.agent_socket_path(),
            "/tmp/proxyctl-dev/run/agent.sock"
        );
        assert_eq!(
            config.controller,
            ControllerEndpoint::UnixSocket("/tmp/proxyctl-dev/run/mihomo.sock".to_owned())
        );
    }

    /// A flag must beat the file. This is the precedence rule the module exists to
    /// make true, so it is asserted through the real entry point.
    #[test]
    fn a_flag_beats_the_file() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[agent]\ninstance = \"from-file\"\n").expect("write");
        set_private(&path);

        let mut args = args();
        args.config = Some(path);
        args.instance = Some("from-flag".to_owned());
        assert_eq!(serve(&args).instance.as_str(), "from-flag");

        // And with no flag, the file wins over the default.
        args.instance = None;
        assert_eq!(serve(&args).instance.as_str(), "from-file");
    }

    /// `--print-config` reports provenance and never serves.
    #[test]
    fn print_config_reports_and_returns_a_report() {
        let mut args = args();
        args.print_config = true;
        args.root = Some(PathBuf::from("/tmp/dev"));
        match prepare(&args).expect("prepare") {
            Prepared::Print(report) => {
                assert!(report.contains("[argument]"), "{report}");
                assert!(report.contains("/tmp/dev/lib/configs"), "{report}");
            }
            Prepared::Serve(_) => panic!("--print-config must not serve"),
        }
    }

    /// The report says when no file was found, because the operator's first
    /// question on a failed start is "which file did it read".
    #[test]
    fn the_report_names_the_file_and_whether_it_was_found() {
        let mut args = args();
        args.print_config = true;
        args.config = Some(PathBuf::from("/tmp/definitely-not-here.toml"));
        match prepare(&args).expect("prepare") {
            Prepared::Print(report) => {
                assert!(report.contains("/tmp/definitely-not-here.toml"), "{report}");
                assert!(report.contains("not found"), "{report}");
            }
            Prepared::Serve(_) => panic!("expected a report"),
        }
    }

    /// A malformed file fails the start rather than being ignored.
    #[test]
    fn a_malformed_file_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[agent\ninstance = 1\n").expect("write");
        set_private(&path);

        let mut args = args();
        args.config = Some(path);
        let error = prepare(&args).expect_err("must fail");
        assert!(error.contains("invalid configuration"), "{error}");
    }

    /// The file's mode is the deployment's decision, not the loader's.
    ///
    /// Every one of these was refused before, on the grounds that the file may hold
    /// the kernel secret. The cost was that an ordinary local user could not read
    /// the configuration they were expected to edit — and the packaged install made
    /// it worse, since `/etc/proxy-agent` was `0700` and the failure arrived as
    /// `Permission denied` before any check ran.
    ///
    /// Allowing read is defensible because the secret is already reachable on this
    /// host: Mihomo hardcodes `chmod 0666` on its controller socket and verifies no
    /// secret over one, so any local user can already `PUT /configs`. Refusing to
    /// let them read a file they can already act on protected it from everyone
    /// except the people who could use it.
    ///
    /// `0666` is in the list because the packaged install ships it.
    #[test]
    fn the_file_mode_is_not_the_loaders_business() {
        use std::os::unix::fs::PermissionsExt;
        for mode in [0o600, 0o400, 0o644, 0o640, 0o666, 0o604, 0o770] {
            let dir = tempfile::tempdir().expect("dir");
            let path = dir.path().join("config.toml");
            std::fs::write(&path, "[kernel]\nbinary = \"/x/mihomo\"\n").expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");

            let mut args = args();
            args.config = Some(path);
            assert_eq!(
                serve(&args).kernel_binary,
                "/x/mihomo",
                "mode {mode:o} must be accepted; the mode is the deployment's choice"
            );
        }
    }

    /// A file-provided secret reaches the configuration, and the loopback
    /// controller it enables survives composition.
    #[test]
    fn a_file_secret_reaches_the_configuration() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[controller]\nendpoint = \"127.0.0.1:9090\"\n\n[kernel]\nsecret = \"hunter2\"\n",
        )
        .expect("write");
        set_private(&path);

        let mut args = args();
        args.config = Some(path);
        let config = serve(&args);
        assert_eq!(config.mihomo_secret.as_deref(), Some("hunter2"));
        assert_eq!(
            config.controller,
            ControllerEndpoint::Loopback {
                address: "127.0.0.1:9090".to_owned()
            }
        );
    }

    /// An empty secret is refused through the real entry point too, since this is
    /// the path an operator actually takes.
    #[test]
    fn an_empty_secret_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[kernel]\nsecret = \"\"\n").expect("write");
        set_private(&path);

        let mut args = args();
        args.config = Some(path);
        let error = prepare(&args).expect_err("must be refused");
        assert!(error.contains("empty"), "{error}");
    }

    /// Flags reach the composed configuration.
    #[test]
    fn flags_reach_the_configuration() {
        let mut args = args();
        args.kernel_binary = Some("/opt/mihomo".to_owned());
        args.allow = vec!["10.0.0.0/8".to_owned()];
        args.allow_write_probes = true;
        let config = serve(&args);
        assert_eq!(config.kernel_binary, "/opt/mihomo");
        assert_eq!(config.subscription_allow, vec!["10.0.0.0/8".to_owned()]);
        assert!(config.allow_write_probes);
    }

    /// Write probes mutate host state and must stay off unless asked for.
    #[test]
    fn write_probes_default_to_off() {
        assert!(!serve(&args()).allow_write_probes);
    }

    /// A file enabling write probes must survive an absent flag: forwarding the
    /// flag's `false` would silently disable what the operator configured.
    #[test]
    fn an_absent_probe_flag_does_not_disable_the_file_setting() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[security]\nallow_write_probes = true\n").expect("write");
        set_private(&path);

        let mut args = args();
        args.config = Some(path);
        assert!(
            serve(&args).allow_write_probes,
            "the file asked for write probes and no flag contradicted it"
        );
    }

    /// A relative `--controller` is refused, because it would otherwise be read as
    /// a host name and fail much later as a connection error.
    #[test]
    fn a_relative_controller_flag_is_refused() {
        for value in ["./mihomo.sock", "mihomo.sock"] {
            let mut args = args();
            args.controller = Some(value.to_owned());
            let error = prepare(&args).expect_err("must be refused");
            assert!(
                error.contains("absolute") || error.contains("host:port"),
                "{value}: {error}"
            );
        }
    }

    #[test]
    fn the_config_path_follows_the_flag_then_the_default() {
        let mut args = args();
        assert_eq!(config_path(&args), PathBuf::from(file::DEFAULT_CONFIG_PATH));
        args.config = Some(PathBuf::from("/tmp/explicit.toml"));
        assert_eq!(config_path(&args), PathBuf::from("/tmp/explicit.toml"));
    }

    /// Sets the owner-only mode the loader requires.
    fn set_private(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
}
