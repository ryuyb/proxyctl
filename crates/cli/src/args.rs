//! The command-line grammar.
//!
//! Separated from the dispatch so that both halves of the binary — and tests —
//! can name the same argument types without importing the entry point.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// The Mihomo management agent.
#[derive(Debug, Parser)]
#[command(
    name = "proxyctl",
    version,
    about = "Manage Mihomo and its configuration through the local agent",
    long_about = None,
    // A command that fails should print the error, not a wall of usage: the
    // usage text is for a person who mistyped, and `--help` is how they ask.
    disable_help_subcommand = true,
)]
pub struct Cli {
    /// Where the agent listens. Defaults to the environment, then the build's
    /// documented path.
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Print the server's response body verbatim instead of a summary.
    #[arg(long, global = true)]
    pub json: bool,

    /// The subcommand.
    #[command(subcommand)]
    pub command: TopCommand,
}

/// The top-level commands.
#[derive(Debug, Subcommand)]
pub enum TopCommand {
    /// Run the agent daemon.
    Agent(AgentArgs),

    /// Report the kernel's state and any operation in progress.
    Status(InstanceArg),

    /// Detect and report runtime capabilities.
    Doctor,

    /// Report the platform, capabilities, and environment.
    System,

    /// Start the kernel.
    Start(InstanceArg),

    /// Stop the kernel.
    Stop(InstanceArg),

    /// Restart the kernel.
    Restart(InstanceArg),

    /// Reload the active configuration.
    Reload(InstanceArg),

    /// Manage the installed kernel.
    #[command(subcommand)]
    Mihomo(MihomoCommand),

    /// Manage configuration versions.
    #[command(subcommand)]
    Config(ConfigCommand),

    /// Manage subscriptions.
    #[command(subcommand)]
    Subscription(SubscriptionCommand),

    /// List recent jobs, or show one.
    Jobs(JobsArgs),

    /// Show the audit trail.
    #[command(name = "audit")]
    Audit(AuditArgs),

    /// Stream the kernel's logs.
    Logs(LogsArgs),

    /// Inspect and close the kernel's connections.
    #[command(subcommand)]
    Connections(ConnectionsCommand),
}

/// The `agent` subcommand.
#[derive(Debug, Args)]
pub struct AgentArgs {
    /// The agent action.
    #[command(subcommand)]
    pub action: AgentAction,
}

/// What the agent subcommand can do.
#[derive(Debug, Subcommand)]
pub enum AgentAction {
    /// Compose the context and serve the socket until signalled.
    Run(AgentRunArgs),
}

/// The `agent run` arguments.
#[derive(Debug, Args)]
pub struct AgentRunArgs {
    /// The configuration file.
    ///
    /// Defaults to the environment, then `/etc/proxy-agent/config.toml`. A
    /// missing file is not an error: everything falls back to its default, which
    /// is what makes a development run possible without inventing a file.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Print the effective configuration and where each value came from, then
    /// exit without serving.
    ///
    /// The point is diagnosis: the most common deployment failure is "I changed
    /// the configuration and nothing happened", and the answer is always which
    /// source won.
    #[arg(long)]
    pub print_config: bool,

    /// The instance to compose for.
    ///
    /// No clap default here: the default lives in the merge, so that "was this
    /// typed" can be told from "was this defaulted" and reported as such.
    #[arg(long, value_name = "NAME")]
    pub instance: Option<String>,

    /// A root directory for every path, for development.
    ///
    /// Intended for a test or a development run; a system deployment uses the
    /// conventional locations instead of a single root.
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,

    /// The kernel's control endpoint. A path means a unix socket.
    #[arg(long, value_name = "ADDR|PATH")]
    pub controller: Option<String>,

    /// Where the kernel binary lives.
    #[arg(long, value_name = "PATH")]
    pub kernel_binary: Option<String>,

    /// Outbound hosts or CIDR blocks a subscription fetch may reach. Repeatable.
    /// Anything else public is refused by default.
    #[arg(long = "allow", value_name = "HOST|CIDR")]
    pub allow: Vec<String>,

    /// Run capability probes that write to the host.
    #[arg(long)]
    pub allow_write_probes: bool,
}

/// An instance argument.
#[derive(Debug, Args)]
pub struct InstanceArg {
    /// The instance.
    #[arg(long, default_value = "default")]
    pub instance: String,
}

/// The `mihomo` subcommands.
#[derive(Debug, Subcommand)]
pub enum MihomoCommand {
    /// Report the installed kernel version.
    Version,
    /// Install a kernel version.
    Update(KernelUpdateArgs),
}

/// The `mihomo update` arguments.
#[derive(Debug, Args)]
pub struct KernelUpdateArgs {
    /// The version to install, for example `v1.19.30`.
    #[arg(value_name = "VERSION")]
    pub version: String,
}

/// The `config` subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// List configuration versions.
    List(ListArgs),
    /// Validate a document without activating it.
    Validate(ValidateArgs),
    /// Activate a version.
    Activate(IdArg),
    /// Roll back to a version.
    Rollback(IdArg),
}

/// The `subscription` subcommands.
#[derive(Debug, Subcommand)]
pub enum SubscriptionCommand {
    /// List subscriptions.
    List(ListArgs),
    /// Fetch, convert and activate a subscription.
    Update(IdArg),
}

/// A list limit.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// The instance, for the commands that are per-instance.
    #[arg(long, default_value = "default")]
    pub instance: String,
    /// How many to request.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

/// A document to validate.
#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// The file to validate. `-` reads standard input.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
}

/// A version identifier.
#[derive(Debug, Args)]
pub struct IdArg {
    /// The identifier.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// The `jobs` arguments.
#[derive(Debug, Args)]
pub struct JobsArgs {
    /// A job to show. Without it, the most recent jobs are listed.
    #[arg(value_name = "ID")]
    pub id: Option<String>,
}

/// The `audit` arguments.
#[derive(Debug, Args)]
pub struct AuditArgs {
    /// How many records to request.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

/// The `connections` subcommands.
#[derive(Debug, Subcommand)]
pub enum ConnectionsCommand {
    /// List active connections.
    List,
    /// Close one connection, or every connection.
    Close(CloseArgs),
}

/// The `connections close` arguments.
#[derive(Debug, Args)]
pub struct CloseArgs {
    /// The connection to close. Omit with `--all` to close every connection.
    #[arg(value_name = "ID", required_unless_present = "all")]
    pub id: Option<String>,

    /// Close every connection. Requires `--yes`.
    #[arg(long)]
    pub all: bool,

    /// Confirm closing every connection.
    ///
    /// Required when `--all` is given. Closing every connection interrupts every
    /// active transfer at once, which is the only operation in this CLI whose blast
    /// radius is "all users", so it is the only one that needs an explicit
    /// acknowledgement.
    #[arg(long)]
    pub yes: bool,
}

/// The `logs` arguments.
#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Minimum level to show.
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<String>,

    /// Keep the stream open, rather than stopping at the first line.
    ///
    /// The command streams either way; this states the intent explicitly so a
    /// supervisor or script that wants a long-lived stream can say so.
    #[arg(short = 'f', long)]
    pub follow: bool,
}
