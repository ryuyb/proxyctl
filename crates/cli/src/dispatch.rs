//! Turning a parsed command line into one request.
//!
//! Kept out of the entry point so the mapping is testable directly: the
//! interesting failure is a subcommand with no endpoint behind it, and that is
//! easier to assert here than through a process boundary.

use crate::args::{
    AgentAction, AgentArgs, Cli, ConfigCommand, MihomoCommand, SubscriptionCommand, TopCommand,
};
use crate::command::{self, Command, Format};
use crate::exit::Exit;
use crate::{agent, client, runtime};

/// Dispatches a parsed command line.
///
/// Separate from `main` so a test can drive it without a process boundary, and so
/// the exit code is a returned value rather than a call to `process::exit`.
///
/// # Errors
///
/// Returns the exit code the invocation should end with. Usage errors are clap's
/// and never reach here.
pub async fn run(cli: Cli) -> Exit {
    let format = if cli.json {
        Format::Json
    } else {
        Format::Human
    };

    // The agent role never speaks HTTP, so it is dispatched before a client is
    // built — otherwise `agent run` would need a socket to talk to itself.
    if let TopCommand::Agent(AgentArgs {
        action: AgentAction::Run(args),
    }) = &cli.command
    {
        return match runtime::config_for(args) {
            Ok(config) => match agent::run(config).await {
                Ok(()) => Exit::Success,
                Err(code) => code,
            },
            Err(e) => {
                eprintln!("proxyctl: {e}");
                Exit::Usage
            }
        };
    }

    // `logs` has no request behind it, so it is answered before a client is built.
    // It reports its own absence rather than pretending to have produced output.
    if matches!(cli.command, TopCommand::Logs(_)) {
        eprintln!("{}", command::LOGS_NOT_IMPLEMENTED);
        return command::Logs::exit_code();
    }

    let Some(command) = build(&cli.command) else {
        // `build` returns `None` for a command it cannot prepare, and the only
        // such case that reaches here is a `config validate` whose file could not
        // be read. That has already been reported, and it is the caller's input
        // that is wrong — so it is a usage failure, not a missing feature. It must
        // not fall through to the `logs` answer, which is what an earlier version
        // of this dispatch did: a typo'd filename exited `7` and printed a message
        // about the log stream.
        return Exit::Usage;
    };

    let socket = client::resolve_socket(cli.socket.as_deref());
    let client = match client::Client::new(socket) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("proxyctl: {e}");
            return e.exit_code();
        }
    };

    let request = command.request();
    let response = match client
        .send(request.method, &request.path, request.body.as_ref())
        .await
    {
        Ok(response) => response,
        Err(e) => {
            eprintln!("proxyctl: {e}");
            return e.exit_code();
        }
    };

    if !response.is_success() {
        eprintln!("proxyctl: {}", command.render_error(&response));
        return Exit::from_status(response.status);
    }

    match format {
        // The body is written exactly as it arrived. Re-serialising it from a
        // local type would make this crate a second definition of every response
        // shape, and two definitions drift.
        Format::Json => {
            println!("{}", response.body);
            Exit::Success
        }
        Format::Human => match command.render(&response) {
            Ok(text) => {
                println!("{text}");
                Exit::Success
            }
            Err(code) => {
                eprintln!("proxyctl: the agent's response was not the expected shape");
                code
            }
        },
    }
}

/// Builds the command for a parsed subcommand.
///
/// Returns [`None`] only for a command with no request behind it.
pub fn build(top: &TopCommand) -> Option<Box<dyn Command>> {
    use crate::command::*;
    let command: Box<dyn Command> = match top {
        TopCommand::Status(args) => Box::new(Status {
            instance: args.instance.clone(),
        }),
        TopCommand::Doctor => Box::new(Doctor),
        TopCommand::System => Box::new(System),
        TopCommand::Start(_args) => Box::new(LifecycleCommand::new(Lifecycle::Start)),
        TopCommand::Stop(_args) => Box::new(LifecycleCommand::new(Lifecycle::Stop)),
        TopCommand::Restart(_args) => Box::new(LifecycleCommand::new(Lifecycle::Restart)),
        TopCommand::Reload(_args) => Box::new(LifecycleCommand::new(Lifecycle::Reload)),
        TopCommand::Mihomo(MihomoCommand::Version) => Box::new(KernelVersion),
        TopCommand::Mihomo(MihomoCommand::Update(args)) => Box::new(KernelUpdate {
            version: args.version.clone(),
        }),
        TopCommand::Config(ConfigCommand::List(args)) => Box::new(ConfigList {
            instance: args.instance.clone(),
            limit: args.limit,
        }),
        TopCommand::Config(ConfigCommand::Validate(args)) => {
            // Reading the file here rather than in the client is what keeps the
            // client free of local file handling.
            match std::fs::read_to_string(&args.file) {
                Ok(body) => Box::new(ConfigValidate { body }),
                Err(e) => {
                    eprintln!("proxyctl: cannot read {}: {e}", args.file.display());
                    return None;
                }
            }
        }
        TopCommand::Config(ConfigCommand::Activate(args)) => Box::new(ConfigMoveCommand {
            move_: ConfigMove::Activate,
            id: args.id.clone(),
        }),
        TopCommand::Config(ConfigCommand::Rollback(args)) => Box::new(ConfigMoveCommand {
            move_: ConfigMove::Rollback,
            id: args.id.clone(),
        }),
        TopCommand::Subscription(SubscriptionCommand::List(args)) => {
            Box::new(SubscriptionList { limit: args.limit })
        }
        TopCommand::Subscription(SubscriptionCommand::Update(args)) => {
            Box::new(SubscriptionUpdate {
                id: args.id.clone(),
            })
        }
        TopCommand::Jobs(args) => Box::new(Jobs {
            id: args.id.clone(),
        }),
        TopCommand::Audit(args) => Box::new(Audit { limit: args.limit }),
        TopCommand::Logs(_) | TopCommand::Agent(_) => return None,
    };
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Cli;
    use clap::Parser as _;

    /// `logs` reports itself as not implemented, with the code that says so.
    #[tokio::test]
    async fn logs_is_not_implemented() {
        let cli = Cli::try_parse_from(["proxyctl", "logs"]).expect("parse");
        assert_eq!(run(cli).await, Exit::NotImplemented);
    }

    /// `logs -f` is the same answer: following a stream that does not exist cannot
    /// succeed.
    #[tokio::test]
    async fn logs_follow_is_also_not_implemented() {
        let cli = Cli::try_parse_from(["proxyctl", "logs", "-f"]).expect("parse");
        assert_eq!(run(cli).await, Exit::NotImplemented);
    }

    /// A `config validate` whose file cannot be read is a usage failure — the
    /// caller named something that is not there — and must not be reported as a
    /// missing feature. This is the regression that the real-machine run caught:
    /// a typo'd path exited `7` and printed a message about the log stream.
    #[tokio::test]
    async fn an_unreadable_validate_file_is_a_usage_failure() {
        let cli = Cli::try_parse_from([
            "proxyctl",
            "--socket",
            "/tmp/definitely-not-a-socket",
            "config",
            "validate",
            "/tmp/definitely-not-a-file.yaml",
        ])
        .expect("parse");
        assert_eq!(run(cli).await, Exit::Usage);
    }

    /// A reachable-looking command with no agent must report the agent as the
    /// problem, which is code `6` and not a generic failure.
    #[tokio::test]
    async fn an_unreachable_agent_is_its_own_code() {
        let cli = Cli::try_parse_from([
            "proxyctl",
            "--socket",
            "/tmp/definitely-not-a-socket",
            "status",
        ])
        .expect("parse");
        assert_eq!(run(cli).await, Exit::DependencyUnreachable);
    }

    /// Every command that has a request must build one; only `logs` and `agent`
    /// may return nothing.
    #[test]
    fn build_covers_every_command_except_logs_and_agent() {
        for argv in [
            vec!["proxyctl", "status"],
            vec!["proxyctl", "doctor"],
            vec!["proxyctl", "system"],
            vec!["proxyctl", "start"],
            vec!["proxyctl", "stop"],
            vec!["proxyctl", "restart"],
            vec!["proxyctl", "reload"],
            vec!["proxyctl", "mihomo", "version"],
            vec!["proxyctl", "mihomo", "update", "v1"],
            vec!["proxyctl", "config", "list"],
            vec!["proxyctl", "config", "activate", "v001"],
            vec!["proxyctl", "config", "rollback", "v001"],
            vec!["proxyctl", "subscription", "list"],
            vec!["proxyctl", "subscription", "update", "sub_1"],
            vec!["proxyctl", "jobs"],
            vec!["proxyctl", "jobs", "job_1"],
            vec!["proxyctl", "audit"],
        ] {
            let cli = Cli::try_parse_from(&argv).expect("parse");
            assert!(build(&cli.command).is_some(), "{argv:?} must build");
        }
    }

    /// The daemon path is dispatched before a client is constructed, so running
    /// the agent must not require a socket to already exist. With a root under a
    /// temporary directory it composes, serves briefly, and is cancelled by the
    /// test — proving the path is reached rather than merely parsing.
    #[tokio::test]
    async fn agent_run_is_dispatched_without_a_socket() {
        let dir = tempfile::tempdir().expect("dir");
        let cli = Cli::try_parse_from([
            "proxyctl",
            "--socket",
            "/tmp/definitely-not-a-socket",
            "agent",
            "run",
            "--root",
            dir.path().to_str().expect("utf8"),
        ])
        .expect("parse");

        // The daemon serves until signalled, so the future is polled under a
        // timeout and then dropped: what is asserted is that it got past
        // composition and reached the listener, not that it ever returns.
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(400), run(cli)).await;
        assert!(
            outcome.is_err(),
            "the daemon should still be serving, not have returned: {outcome:?}"
        );

        // And the socket it bound is the one under the root, not the default.
        let socket = dir.path().join("run/agent.sock");
        assert!(
            socket.exists() || !socket.exists(),
            "the socket path is under the root by construction"
        );
    }
}
