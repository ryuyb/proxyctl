//! Turning agent flags into a [`RuntimeConfig`].
//!
//! # Why the flags are not the whole configuration
//!
//! A system deployment reads its settings from a file that packaging installs.
//! That loader does not exist yet, so this module covers the subset the daemon
//! needs to start today, and it says so rather than implying the flags are the
//! intended interface.
//!
//! The one translation that matters is the controller: a value containing a `/`
//! is a socket path, anything else is host:port. Guessing the other way — treating
//! a bare word as a path — would put a socket file in the working directory.

use std::path::PathBuf;

use proxy_bootstrap::{ControllerEndpoint, DataPaths, RuntimeConfig};
use proxy_domain::shared::id::MihomoInstanceId;

use crate::args::AgentRunArgs;

/// The default controller socket, matching the documented layout.
pub const DEFAULT_CONTROLLER: &str = "/run/proxy-agent/mihomo.sock";

/// Builds a configuration from `agent run`'s flags.
///
/// # Errors
///
/// Returns a message when the instance name is not a valid identifier, or the
/// root or controller cannot be interpreted. The caller turns that into a usage
/// error, because that is what it is: the operator passed something unusable.
pub fn config_for(args: &AgentRunArgs) -> Result<RuntimeConfig, String> {
    let instance = MihomoInstanceId::parse(&args.instance)
        .map_err(|e| format!("invalid instance name {:?}: {e}", args.instance))?;

    let mut config = match &args.root {
        Some(root) => RuntimeConfig::rooted_at(instance, root.display().to_string()),
        None => RuntimeConfig::local(instance),
    };

    config.controller = match &args.controller {
        Some(value) => parse_controller(value)?,
        None => match &args.root {
            // `rooted_at` already placed the socket under the root, and it must
            // stay there: pointing a development run at `/run` would either fail
            // or, worse, collide with a real agent's socket.
            None => ControllerEndpoint::UnixSocket(DEFAULT_CONTROLLER.to_owned()),
            Some(_) => config.controller,
        },
    };

    if let Some(binary) = &args.kernel_binary {
        config.kernel_binary = binary.clone();
    }
    config.subscription_allow = args.allow.clone();
    config.allow_write_probes = args.allow_write_probes;

    // A root without an explicit controller keeps `rooted_at`'s socket, but the
    // socket path must also be rooted — `rooted_at` sets it, `local` does not.
    if args.root.is_some() && args.controller.is_none() {
        let root = args
            .root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let root = root.trim_end_matches('/');
        config.socket_path = Some(format!("{root}/run/agent.sock"));
    }

    Ok(config)
}

/// Interprets a controller value.
///
/// # Errors
///
/// Returns a message when the value is empty.
pub fn parse_controller(value: &str) -> Result<ControllerEndpoint, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("the controller endpoint must not be empty".to_owned());
    }
    if value.contains('/') {
        return Ok(ControllerEndpoint::UnixSocket(value.to_owned()));
    }
    // `host:port` with no port is almost certainly a mistake, but the endpoint
    // type carries it and the connection attempt reports it with the address,
    // which is more useful than a guess here.
    Ok(ControllerEndpoint::Loopback {
        address: value.to_owned(),
    })
}

/// The conventional data paths, exposed so a caller can report them.
#[must_use]
pub fn standard_paths() -> DataPaths {
    DataPaths::standard()
}

/// A convenience for tests and callers that want a rooted config directly.
#[must_use]
pub fn rooted(instance: &str, root: &std::path::Path) -> Option<RuntimeConfig> {
    let Ok(instance) = MihomoInstanceId::parse(instance) else {
        return None;
    };
    Some(RuntimeConfig::rooted_at(
        instance,
        root.display().to_string(),
    ))
}

/// The root as a path, when one was given.
#[must_use]
pub fn root_of(args: &AgentRunArgs) -> Option<PathBuf> {
    args.root.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> AgentRunArgs {
        AgentRunArgs {
            instance: "default".to_owned(),
            root: None,
            controller: None,
            kernel_binary: None,
            allow: Vec::new(),
            allow_write_probes: false,
        }
    }

    #[test]
    fn a_bare_controller_value_is_host_and_port() {
        assert_eq!(
            parse_controller("127.0.0.1:9090").expect("ok"),
            ControllerEndpoint::Loopback {
                address: "127.0.0.1:9090".to_owned()
            }
        );
    }

    /// A path is a socket. This is the case that would otherwise create a file
    /// named after a host in the working directory.
    #[test]
    fn a_controller_value_with_a_slash_is_a_socket_path() {
        assert_eq!(
            parse_controller("/run/proxy-agent/mihomo.sock").expect("ok"),
            ControllerEndpoint::UnixSocket("/run/proxy-agent/mihomo.sock".to_owned())
        );
    }

    #[test]
    fn an_empty_controller_is_refused() {
        assert!(parse_controller("").is_err());
        assert!(parse_controller("   ").is_err());
    }

    #[test]
    fn an_invalid_instance_name_is_refused() {
        let mut args = args();
        args.instance = String::new();
        assert!(config_for(&args).is_err());
    }

    /// The system default keeps every path conventional; a root moves all of them
    /// together, which is the property that makes a development run safe.
    #[test]
    fn a_root_moves_the_socket_under_itself() {
        let mut args = args();
        args.root = Some(PathBuf::from("/tmp/proxyctl-dev"));
        let config = config_for(&args).expect("ok");
        assert_eq!(
            config.agent_socket_path(),
            "/tmp/proxyctl-dev/run/agent.sock"
        );
        assert_eq!(config.paths.configs_dir, "/tmp/proxyctl-dev/lib/configs");
        assert_eq!(
            config.controller,
            ControllerEndpoint::UnixSocket("/tmp/proxyctl-dev/run/mihomo.sock".to_owned())
        );
    }

    #[test]
    fn without_a_root_the_paths_are_conventional() {
        let config = config_for(&args()).expect("ok");
        assert_eq!(config.agent_socket_path(), "/run/proxy-agent/agent.sock");
        assert_eq!(
            config.controller,
            ControllerEndpoint::UnixSocket(DEFAULT_CONTROLLER.to_owned())
        );
    }

    /// An explicit controller overrides the derived one, in both directions.
    #[test]
    fn an_explicit_controller_wins() {
        let mut args = args();
        args.root = Some(PathBuf::from("/tmp/x"));
        args.controller = Some("127.0.0.1:9090".to_owned());
        let config = config_for(&args).expect("ok");
        assert_eq!(
            config.controller,
            ControllerEndpoint::Loopback {
                address: "127.0.0.1:9090".to_owned()
            }
        );
        // The agent socket still follows the root: the two are different sockets.
        assert_eq!(config.agent_socket_path(), "/tmp/x/run/agent.sock");
    }

    #[test]
    fn flags_reach_the_configuration() {
        let mut args = args();
        args.kernel_binary = Some("/opt/mihomo".to_owned());
        args.allow = vec!["10.0.0.0/8".to_owned()];
        args.allow_write_probes = true;
        let config = config_for(&args).expect("ok");
        assert_eq!(config.kernel_binary, "/opt/mihomo");
        assert_eq!(config.subscription_allow, vec!["10.0.0.0/8".to_owned()]);
        assert!(config.allow_write_probes);
    }

    /// Write probes mutate host state, so the default must be off.
    #[test]
    fn write_probes_default_to_off() {
        assert!(!config_for(&args()).expect("ok").allow_write_probes);
    }

    #[test]
    fn a_rooted_config_can_be_built_directly() {
        let config = rooted("default", std::path::Path::new("/tmp/y")).expect("ok");
        assert_eq!(config.agent_socket_path(), "/tmp/y/run/agent.sock");
        assert!(rooted("", std::path::Path::new("/tmp/y")).is_none());
    }
}
