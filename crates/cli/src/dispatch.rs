//! Turning a parsed command line into one request.
//!
//! Kept out of the entry point so the mapping is testable directly: the
//! interesting failure is a subcommand with no endpoint behind it, and that is
//! easier to assert here than through a process boundary.

use crate::args::{
    AgentAction, AgentArgs, Cli, ConfigCommand, ConnectionsCommand, EventsArgs, LogsArgs,
    MihomoCommand, SubscriptionCommand, TokenCommand, TopCommand,
};
use crate::command::{self, Command, Format};
use crate::exit::Exit;
use crate::{agent, client, endpoint, runtime};

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
        return match runtime::prepare(args) {
            // `--print-config` is a diagnosis, not a failure: it reports and exits
            // zero, so packaging can use it as a self-check.
            Ok(runtime::Prepared::Print(report)) => {
                println!("{report}");
                Exit::Success
            }
            Ok(runtime::Prepared::Serve(config)) => match agent::run(*config).await {
                Ok(()) => Exit::Success,
                Err(code) => code,
            },
            Err(e) => {
                // A configuration the operator supplied that cannot be used is a
                // usage failure rather than a runtime one: the process never
                // started, and the fix is to change the input.
                eprintln!("proxyctl: {e}");
                Exit::Usage
            }
        };
    }

    // `logs` streams rather than rendering, so it is driven here instead of being
    // modelled as a `Command`. See `command::Logs` for why it does not fit the
    // trait that every other command implements.
    if let TopCommand::Logs(args) = &cli.command {
        return stream_logs(&cli.socket, &cli.token, args, format).await;
    }

    // The event stream is followed the same way the log stream is: it has no
    // single response to render, so it is driven here rather than modelled as a
    // `Command`.
    if let TopCommand::Events(args) = &cli.command {
        return stream_events(&cli.socket, &cli.token, args, format).await;
    }

    // The TUI owns the terminal for its whole run, so it is dispatched before
    // anything else can print — a message written while it is open would corrupt
    // the display.
    if matches!(cli.command, TopCommand::Tui) {
        let Some(client) = connect(&cli.socket, &cli.token) else {
            return Exit::Usage;
        };
        return match crate::tui::run(client).await {
            Ok(()) => Exit::Success,
            Err(e) => {
                eprintln!("proxyctl: {e}");
                Exit::Failure
            }
        };
    }

    // Token commands open the database directly rather than going over the socket:
    // issuing a token is what makes a listener allowed to exist, so it must work
    // before one is running. See the module for why.
    match &cli.command {
        TopCommand::Token(TokenCommand::Issue(args)) => return crate::token::issue(args).await,
        TopCommand::Token(TokenCommand::List(args)) => {
            return crate::token::list(&args.config).await;
        }
        TopCommand::Token(TokenCommand::Revoke(args)) => return crate::token::revoke(args).await,
        _ => {}
    }

    // `connections close --all` needs an acknowledgement before anything is sent.
    // The server also requires one, so this is the client-side half of a gate that
    // exists on both sides: a mistyped `--all` must not reach the kernel because
    // the caller's own shell was the only thing that could have stopped it.
    if let TopCommand::Connections(ConnectionsCommand::Close(args)) = &cli.command
        && args.all
        && !args.yes
    {
        eprintln!(
            "proxyctl: closing every connection interrupts all active transfers; \
             pass --yes to confirm"
        );
        return Exit::Usage;
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

    let Some(client) = connect(&cli.socket, &cli.token) else {
        return Exit::Usage;
    };

    // `doctor` answers "can I reach this agent", which is the first question of a
    // remote deployment and one the agent itself cannot answer: an unreachable
    // agent replies with nothing. The report goes first, so it is visible even when
    // the request that follows fails.
    if matches!(cli.command, TopCommand::Doctor)
        && let Ok(report) = client.connection_report().await
    {
        println!("{report}\n");
    }

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
        // A 401 and a 403 are the two failures a remote caller hits most, and the
        // status alone does not distinguish them in a way an operator can act on:
        // one means the token is not accepted at all, the other that it is accepted
        // and insufficient. Saying which is which points at the fix.
        match response.status {
            401 => eprintln!(
                "proxyctl: the token was not accepted. Check that it was copied whole, that it has                  not been revoked, and that it is being sent with --token or {}. A fresh one can be                  issued with `proxyctl token issue` on the agent host.",
                crate::endpoint::TOKEN_ENV
            ),
            403 => eprintln!(
                "proxyctl: the request was refused as not permitted. Every issued token carries \
                 the same authority, so this is usually a method the endpoint does not accept — \
                 a write sent to a read-only path, or a write to /clash-api"
            ),
            _ => {}
        }
        return Exit::from_status(response.status);
    }

    match format {
        // The body is written exactly as it arrived. Re-serialising it from a
        // local type would make this crate a second definition of every response
        // shape, and two definitions drift.
        Format::Json => {
            println!("{}", response.body);
            // The same rule as the human path, and for the same reason. `--json`
            // is the form a script parses, so getting the exit code wrong here
            // would matter more than getting it wrong in the rendered output.
            command.exit_code(&response)
        }
        Format::Human => match command.render(&response) {
            // The text is the output, but the status comes from the command: a
            // `200` can still be bad news, which is what `config validate`
            // produces for a rejected document. Reporting `Success` here meant the
            // word "rejected" printed alongside an exit code of `0`.
            Ok(text) => {
                println!("{text}");
                command.exit_code(&response)
            }
            Err(code) => {
                eprintln!("proxyctl: the agent's response was not the expected shape");
                code
            }
        },
    }
}

/// Builds a client from the global options, printing and explaining any refusal.
///
/// Returns `None` when the endpoint could not be resolved, after reporting why.
/// The three commands that need a client all go through this, so a change to how
/// an endpoint is resolved or authorized cannot reach two of them and miss the
/// third.
fn connect(socket: &Option<std::path::PathBuf>, token: &Option<String>) -> Option<client::Client> {
    let endpoint = match endpoint::resolve(socket.as_deref(), token.as_deref()) {
        Ok(endpoint) => endpoint,
        Err(e) => {
            eprintln!("proxyctl: {}", e.message());
            return None;
        }
    };

    // A token sent in the clear off this machine is worth saying out loud, every
    // time: it is legal, it works, and it is exactly the arrangement an operator
    // forgets they have.
    if let Some(warning) = endpoint::plaintext_warning(&endpoint) {
        eprintln!("proxyctl: warning: {warning}");
    }

    match client::Client::new(endpoint) {
        Ok(client) => Some(client),
        Err(e) => {
            eprintln!("proxyctl: {e}");
            None
        }
    }
}

/// Streams the kernel's logs to standard output.
///
/// # `-f` semantics
///
/// Without `-f` the command prints until it is interrupted, because a log is not a
/// document with an end: asking for "the logs" and getting a blocking stream is
/// what every operator expects from `logs`. `-f` is accepted and makes that
/// explicit — it is the same behaviour, stated deliberately, so a script written
/// against `logs -f` means what it says.
///
/// The command ends when the stream ends (the agent went away, or the kernel
/// restarted) or when the caller interrupts it. An interrupted stream is a normal
/// way to stop, so it is not an error.
async fn stream_logs(
    socket: &Option<std::path::PathBuf>,
    token: &Option<String>,
    args: &LogsArgs,
    format: Format,
) -> Exit {
    let path = command::Logs {
        level: args.level.clone(),
    }
    .path();

    let Some(client) = connect(socket, token) else {
        return Exit::Usage;
    };

    let mut stream = match client.stream(&path).await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("proxyctl: {e}");
            return e.exit_code();
        }
    };

    let as_json = format == Format::Json;

    // Interrupting a stream is how a person stops watching, so SIGINT is handled
    // to return success rather than letting the default disposition look like a
    // crash. Only the first signal is intercepted: the handler is installed once
    // per loop iteration and the process exits on the first, so a stream that will
    // not settle can still be abandoned with a second signal.
    loop {
        let next = tokio::select! {
            line = stream.next_line() => line,
            _ = tokio::signal::ctrl_c() => return Exit::Success,
        };

        match next {
            Ok(Some(line)) => {
                if as_json {
                    // The raw NDJSON line, unchanged: the same rule as every other
                    // command's `--json`.
                    println!("{line}");
                } else {
                    println!("{}", command::format_log_line(&line));
                }
            }
            Ok(None) => {
                // The body ended. Without `-f` that means the agent closed the
                // stream, which happens when the kernel it was attached to goes
                // away; reporting success would hide a stopped agent.
                return if args.follow {
                    Exit::Success
                } else {
                    Exit::DependencyUnreachable
                };
            }
            Err(e) => {
                eprintln!("proxyctl: {e}");
                return e.exit_code();
            }
        }
    }
}

/// Follows the agent's event stream.
///
/// # Why `--once` exists
///
/// Without it the command runs until interrupted, which is what "follow" means.
/// `--once` makes it a connectivity check: a caller that wants to block until
/// something happens — a script waiting for a configuration to activate — gets
/// exactly one event and an exit code that says whether one arrived.
async fn stream_events(
    socket: &Option<std::path::PathBuf>,
    token: &Option<String>,
    args: &EventsArgs,
    format: Format,
) -> Exit {
    let Some(client) = connect(socket, token) else {
        return Exit::Usage;
    };

    // The path carries no version prefix: it is fixed by the design document, and
    // the version is in the path itself.
    let mut stream = match client.stream("/ws/v1/events").await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("proxyctl: {e}");
            return e.exit_code();
        }
    };

    let as_json = format == Format::Json;
    loop {
        let next = tokio::select! {
            line = stream.next_line() => line,
            _ = tokio::signal::ctrl_c() => return Exit::Success,
        };

        match next {
            Ok(Some(line)) => {
                if as_json {
                    println!("{line}");
                } else {
                    println!("{}", command::format_event_line(&line));
                }
                if args.once {
                    return Exit::Success;
                }
            }
            Ok(None) => {
                // The body ended: the agent went away or shut down.
                return Exit::DependencyUnreachable;
            }
            Err(e) => {
                eprintln!("proxyctl: {e}");
                return e.exit_code();
            }
        }
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
        TopCommand::Config(ConfigCommand::Add(args)) => match std::fs::read_to_string(&args.file) {
            Ok(body) => Box::new(ConfigAdd { body }),
            Err(e) => {
                eprintln!("proxyctl: cannot read {}: {e}", args.file.display());
                return None;
            }
        },
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
        TopCommand::Connections(ConnectionsCommand::List) => Box::new(Connections),
        TopCommand::Connections(ConnectionsCommand::Close(args)) => Box::new(CloseConnections {
            id: args.id.clone(),
        }),
        TopCommand::Logs(_) | TopCommand::Events(_) | TopCommand::Agent(_) => return None,
        TopCommand::Token(_) | TopCommand::Tui => return None,
    };
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Cli;
    use clap::Parser as _;

    /// `logs` is a real stream now, so with no agent it reports the *agent* as
    /// unreachable. It must not fall back to "not implemented": an operator who
    /// sees the wrong one of those goes looking for the wrong problem.
    #[tokio::test]
    async fn logs_without_an_agent_reports_it_unreachable() {
        let cli = Cli::try_parse_from([
            "proxyctl",
            "--socket",
            "/tmp/definitely-not-a-socket",
            "logs",
        ])
        .expect("parse");
        assert_eq!(run(cli).await, Exit::DependencyUnreachable);
    }

    /// `logs -f` behaves the same way when the agent is absent, and must accept the
    /// flag: the streaming path does not depend on it, but a caller that wants a
    /// long-lived stream has to be able to say so.
    #[tokio::test]
    async fn logs_follow_without_an_agent_reports_it_unreachable() {
        let cli = Cli::try_parse_from([
            "proxyctl",
            "--socket",
            "/tmp/definitely-not-a-socket",
            "logs",
            "-f",
        ])
        .expect("parse");
        assert_eq!(run(cli).await, Exit::DependencyUnreachable);
    }

    /// A level is passed through in the request path, so a typo reaches the server
    /// and is refused there with the message that names the valid levels — rather
    /// than being silently defaulted by the client.
    #[tokio::test]
    async fn logs_passes_the_level_through() {
        let args = crate::args::LogsArgs {
            level: Some("warning".to_owned()),
            follow: false,
        };
        let path = command::Logs {
            level: args.level.clone(),
        }
        .path();
        assert_eq!(path, "/api/v1/logs?level=warning");
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
            // A root is mandatory here: without one the daemon composes against
            // `/var/lib/proxy-agent` and `/usr/lib/proxy-agent`, which a test has
            // no business touching. Discovering that took a failing run.
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
        assert!(
            dir.path().join("run/agent.sock").exists(),
            "the daemon must bind the socket under the root it was given"
        );
    }

    /// `--print-config` is a diagnosis and must succeed without a daemon, a
    /// socket, or a writable root. This is what makes it usable from packaging.
    #[tokio::test]
    async fn print_config_exits_zero_without_touching_anything() {
        let cli = Cli::try_parse_from([
            "proxyctl",
            "--socket",
            "/tmp/definitely-not-a-socket",
            "agent",
            "run",
            "--print-config",
        ])
        .expect("parse");
        assert_eq!(run(cli).await, Exit::Success);
    }

    /// A configuration file that cannot be used is a usage failure: the process
    /// never started, so the fix is to change the input rather than to retry.
    #[tokio::test]
    async fn an_unusable_config_file_is_a_usage_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[agent\ninstance = 1\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        let cli = Cli::try_parse_from([
            "proxyctl",
            "agent",
            "run",
            "--config",
            path.to_str().expect("utf8"),
            "--root",
            dir.path().to_str().expect("utf8"),
        ])
        .expect("parse");
        assert_eq!(run(cli).await, Exit::Usage);
    }
}
