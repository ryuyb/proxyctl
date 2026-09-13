//! The client-side commands.
//!
//! Each command turns typed arguments into one HTTP request and renders the
//! response. Nothing here decides policy: a command that wants to know whether a
//! restart is allowed asks the server, because the server owns the lifecycle
//! rule and a second copy would eventually disagree with it.
//!
//! # The one place this layer does decide something
//!
//! `logs` is not implemented, and it says so with its own exit code rather than
//! pretending for a moment that it produced output. A stub that printed nothing
//! and exited `0` would be worse than an error: a script would treat silence as
//! "no logs".

use serde_json::json;

use crate::client::Response;
use crate::exit::Exit;

/// How to print a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A human-readable summary.
    Human,
    /// The server's body, verbatim.
    Json,
}

/// A prepared request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The HTTP method.
    pub method: &'static str,
    /// The path, including the `/api/v1` prefix.
    pub path: String,
    /// The JSON body, when the endpoint takes one.
    pub body: Option<serde_json::Value>,
}

impl Request {
    /// A request with no body.
    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: "GET",
            path: path.into(),
            body: None,
        }
    }

    /// A request with a body.
    #[must_use]
    pub fn with_body(
        method: &'static str,
        path: impl Into<String>,
        body: serde_json::Value,
    ) -> Self {
        Self {
            method,
            path: path.into(),
            body: Some(body),
        }
    }
}

/// The `/api/v1` prefix.
///
/// Re-exported from [`crate::endpoint`], where it is defined: that module builds
/// request URLs and sits below this one, so a transport can name the prefix
/// without reaching upward. Kept as a re-export so existing call sites here read
/// the same as before.
pub use crate::endpoint::API_PREFIX;

/// One command's behaviour.
pub trait Command {
    /// Builds the request this command issues.
    fn request(&self) -> Request;

    /// Renders a successful response.
    ///
    /// # Errors
    ///
    /// Returns an exit code when the body is not what the command expects, which
    /// is a real possibility: the server's contract could have moved on.
    fn render(&self, response: &Response) -> Result<String, Exit>;

    /// Renders a non-success response.
    ///
    /// Defaulted because the useful behaviour is the same nearly everywhere —
    /// name the status, add the server's own code and message — and a command
    /// that silently dropped the server's message would make every failure
    /// harder to diagnose.
    fn render_error(&self, response: &Response) -> String {
        let mut text = format!("request failed with status {}", response.status);
        if let Some(code) = response.error_code() {
            text.push_str(&format!(" ({code})"));
        }
        if let Some(message) = response.error_message() {
            text.push_str(&format!(": {message}"));
        }
        text
    }
}

/// Parses a JSON body into an object, or reports a rendering failure.
///
/// Shared by the commands that read a field out of a response, so the "the server
/// sent something unexpected" case is handled once.
///
/// # Errors
///
/// Returns [`Exit::Failure`] when the body is not the JSON object expected.
fn as_object(body: &str) -> Result<serde_json::Value, Exit> {
    serde_json::from_str(body).map_err(|_| Exit::Failure)
}

/// Parses a body that must be a JSON array.
///
/// Separate from [`as_object`] so a list command fails with a clear exit code when
/// the server changes an endpoint from a bare array to an envelope, rather than
/// printing an empty list — which is what reading the wrong field silently does.
///
/// # Errors
///
/// Returns [`Exit::Failure`] when the body is not a JSON array.
fn as_array(body: &str) -> Result<Vec<serde_json::Value>, Exit> {
    serde_json::from_str::<Vec<serde_json::Value>>(body).map_err(|_| Exit::Failure)
}

/// Reads a string field, defaulting to `-` for display purposes.
fn field<'a>(value: &'a serde_json::Value, name: &str) -> &'a str {
    value.get(name).and_then(|v| v.as_str()).unwrap_or("-")
}

/// Renders a value that may be absent.
fn optional(value: &serde_json::Value, name: &str) -> String {
    match value.get(name) {
        Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
        Some(serde_json::Value::Null) | None => "-".to_owned(),
        Some(other) => other.to_string(),
    }
}

/// `status` — the kernel's state and the progress of any long-running operation.
#[derive(Debug, Clone)]
pub struct Status {
    /// The instance to report on.
    pub instance: String,
}

impl Command for Status {
    fn request(&self) -> Request {
        Request::get(format!("{API_PREFIX}/mihomo?instance={}", self.instance))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        // The state is a nested object, not a string field. Reading it with
        // `field(&value, "status")` would have silently printed `-` for every
        // instance, which is exactly the kind of wrong that looks like "the server
        // sent nothing".
        let state = value.get("state").unwrap_or(&serde_json::Value::Null);
        let mut lines = vec![
            format!("instance  {}", field(&value, "instance")),
            format!("status    {}", field(state, "status")),
        ];
        // `live` and `serving` are different questions — a process that exists is
        // not necessarily serving — so both are shown rather than collapsed.
        lines.push(format!(
            "live      {}",
            state.get("live").and_then(|v| v.as_bool()).unwrap_or(false)
        ));
        lines.push(format!(
            "serving   {}",
            state
                .get("serving")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        ));
        lines.push(format!("config    {}", optional(&value, "active_config")));
        if let Some(build) = value.get("build") {
            lines.push(format!("build     {}", optional(build, "version")));
        }
        if let Some(failure) = value.get("last_failure").and_then(|v| v.as_str()) {
            lines.push(format!("failure   {failure}"));
        }
        Ok(lines.join("\n"))
    }
}

/// `start` / `stop` / `restart` / `reload`.
///
/// One type for all four because they differ only in the path they POST to and
/// the word they print. Four near-identical types would be four places for the
/// rendering to drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// Start the kernel.
    Start,
    /// Stop the kernel.
    Stop,
    /// Restart the kernel.
    Restart,
    /// Reload the active configuration.
    Reload,
}

impl Lifecycle {
    /// The path segment.
    #[must_use]
    pub const fn segment(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Reload => "reload",
        }
    }

    /// The past-tense word used in output.
    #[must_use]
    pub const fn past_tense(self) -> &'static str {
        match self {
            Self::Start => "started",
            Self::Stop => "stopped",
            Self::Restart => "restarted",
            Self::Reload => "reloaded",
        }
    }
}

/// A lifecycle command.
#[derive(Debug, Clone)]
pub struct LifecycleCommand {
    /// Which transition.
    pub lifecycle: Lifecycle,
    /// The instance, sent as a query parameter so the server need not guess.
    ///
    /// The body is left empty rather than carrying the instance: the router
    /// accepts no body on these routes, and adding one would make the client's
    /// request shape depend on a field the current server ignores.
    pub instance: Option<String>,
}

impl LifecycleCommand {
    /// A command for the default instance.
    #[must_use]
    pub fn new(lifecycle: Lifecycle) -> Self {
        Self {
            lifecycle,
            instance: None,
        }
    }

    /// A command for a named instance.
    #[must_use]
    pub fn with_instance(lifecycle: Lifecycle, instance: &str) -> Self {
        Self {
            lifecycle,
            instance: Some(instance.to_owned()),
        }
    }
}

impl LifecycleCommand {
    /// The path for this transition.
    ///
    /// One literal per transition rather than an interpolated segment, so the
    /// architecture guard can confirm every path against the server's route table.
    /// See [`ConfigMoveCommand::path`] for the same reasoning.
    #[must_use]
    pub fn path(&self) -> String {
        match self.lifecycle {
            Lifecycle::Start => format!("{API_PREFIX}/mihomo/start"),
            Lifecycle::Stop => format!("{API_PREFIX}/mihomo/stop"),
            Lifecycle::Restart => format!("{API_PREFIX}/mihomo/restart"),
            Lifecycle::Reload => format!("{API_PREFIX}/mihomo/reload"),
        }
    }
}

impl Command for LifecycleCommand {
    fn request(&self) -> Request {
        Request::with_body("POST", self.path(), json!({}))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let mut text = format!(
            "{} {}",
            field(&value, "instance"),
            self.lifecycle.past_tense()
        );
        // A successful start can still be degraded; saying so on the success line
        // is the point, because a person who reads only one line must not miss it.
        if let Some(note) = value.get("degradation").and_then(|v| v.as_str()) {
            text.push_str(&format!("\nnote: {note}"));
        }
        Ok(text)
    }
}

/// `doctor` — the runtime findings.
#[derive(Debug, Clone)]
pub struct Doctor;

impl Command for Doctor {
    fn request(&self) -> Request {
        Request::get(format!("{API_PREFIX}/doctor"))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let findings = value
            .get("findings")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if findings.is_empty() {
            return Ok("no findings".to_owned());
        }

        let mut lines = Vec::with_capacity(findings.len() + 2);
        // The verdict comes first: it is the one-word answer to "is this host
        // ready", and a reader should not have to derive it from the findings.
        if let Some(verdict) = value.get("verdict").and_then(|v| v.as_str()) {
            lines.push(format!("verdict: {verdict}"));
            lines.push(String::new());
        }
        for finding in &findings {
            // Severity first so the eye can scan the column, then the finding's
            // own `code` — a stable identifier a script or a bug report can name —
            // then the message.
            lines.push(format!(
                "{:<9} {:<24} {}",
                field(finding, "severity"),
                field(finding, "code"),
                field(finding, "message")
            ));
        }

        // A summary line, because "7 findings" and "7 findings, 3 of them fatal"
        // are different situations and the second is what the exit decision needs.
        let blocking = findings
            .iter()
            .filter(|f| {
                matches!(
                    field(f, "severity"),
                    "error" | "fatal" | "critical" | "blocking"
                )
            })
            .count();
        lines.push(format!(
            "{} finding(s), {} blocking",
            findings.len(),
            blocking
        ));
        Ok(lines.join("\n"))
    }
}

/// `system` — platform, capabilities and the environment they were read from.
#[derive(Debug, Clone)]
pub struct System;

impl Command for System {
    fn request(&self) -> Request {
        Request::get(format!("{API_PREFIX}/system"))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let mut lines = Vec::new();

        if let Some(env) = value.get("environment") {
            // The field names are the DTO's own (`os`, `arch`, `init`), not a
            // longer spelling: this renderer previously read `operating_system`
            // and printed `-` for every field, which looked like an empty server
            // response rather than a client bug.
            lines.push(format!(
                "os         {} {}",
                field(env, "os"),
                optional(env, "os_version")
            ));
            lines.push(format!("arch       {}", field(env, "arch")));
            lines.push(format!("kernel     {}", optional(env, "kernel")));
            lines.push(format!("init       {}", field(env, "init")));
            lines.push(format!("container  {}", field(env, "container")));
        }

        // Capabilities are a list, not a keyed object, so each is rendered by its
        // `kind`. Iterating the server's own list means a capability added there
        // shows up here without a change — the earlier keyed lookup silently
        // dropped everything it did not already know about.
        if let Some(capabilities) = value.get("capabilities").and_then(|v| v.as_array()) {
            if !capabilities.is_empty() {
                lines.push(String::new());
            }
            for capability in capabilities {
                let kind = field(capability, "kind");
                let status = field(capability, "status");
                // The evidence is the reason to trust the verdict, so it is
                // printed with the state rather than hidden behind a verbose flag.
                match capability.get("evidence").and_then(|v| v.as_str()) {
                    Some(evidence) => {
                        lines.push(format!("{kind:<18} {status:<12} {evidence}"));
                    }
                    None => lines.push(format!("{kind:<18} {status}")),
                }
            }
        }

        if lines.is_empty() {
            return Ok("the server reported no environment data".to_owned());
        }
        Ok(lines.join("\n"))
    }
}

/// `mihomo version` — the installed kernel, and whether an update is available.
#[derive(Debug, Clone)]
pub struct KernelVersion;

impl Command for KernelVersion {
    fn request(&self) -> Request {
        Request::get(format!("{API_PREFIX}/mihomo/kernel"))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let outcome = field(&value, "outcome");
        let version = optional(&value, "version");
        let mut text = format!("{outcome}: {version}");
        if let Some(path) = value.get("binary_path").and_then(|v| v.as_str()) {
            text.push_str(&format!("\nbinary: {path}"));
        }
        if let Some(reason) = value.get("reason").and_then(|v| v.as_str()) {
            text.push_str(&format!("\nreason: {reason}"));
        }
        Ok(text)
    }
}

/// `mihomo update` — installs a kernel version.
#[derive(Debug, Clone)]
pub struct KernelUpdate {
    /// The version to install.
    pub version: String,
}

impl Command for KernelUpdate {
    fn request(&self) -> Request {
        Request::with_body(
            "POST",
            format!("{API_PREFIX}/mihomo/kernel"),
            json!({ "version": self.version }),
        )
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let checksum = value
            .get("checksum")
            .and_then(|v| v.as_str())
            .unwrap_or("(none)");
        Ok(format!(
            "{} {} (checksum {checksum})",
            field(&value, "outcome"),
            optional(&value, "version")
        ))
    }
}

/// `config list`.
#[derive(Debug, Clone)]
pub struct ConfigList {
    /// The instance.
    pub instance: String,
    /// How many to request.
    pub limit: u32,
}

impl Command for ConfigList {
    fn request(&self) -> Request {
        Request::get(format!(
            "{API_PREFIX}/configs?instance={}&limit={}",
            self.instance, self.limit
        ))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        // The list endpoints return a bare JSON array, not an `{items: [...]}`
        // envelope. Reading `items` here would have made every list print "no
        // versions" no matter what the server held.
        let items = as_array(&response.body)?;
        if items.is_empty() {
            return Ok("no configuration versions".to_owned());
        }

        let mut lines = Vec::with_capacity(items.len() + 1);
        for item in &items {
            // A leading `*` marks the active version: it is the single fact a
            // reader scans the list for.
            let marker = if item.get("active").and_then(|v| v.as_bool()) == Some(true) {
                "*"
            } else {
                " "
            };
            lines.push(format!(
                "{marker} {:<28} {:<10} {:<10} {}",
                field(item, "id"),
                field(item, "label"),
                optional(item, "source"),
                optional(item, "checksum")
            ));
        }
        lines.push(format!("{} version(s)", items.len()));
        Ok(lines.join("\n"))
    }
}

/// `config validate` — validates a document without activating it.
#[derive(Debug, Clone)]
pub struct ConfigValidate {
    /// The document.
    pub body: String,
}

impl Command for ConfigValidate {
    fn request(&self) -> Request {
        Request::with_body(
            "POST",
            format!("{API_PREFIX}/configs/validate"),
            json!({ "body": self.body }),
        )
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let acceptable = value
            .get("acceptable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let text = format!(
            "preflight  {}\nsyntax     {}\nsemantic   {}\n{}",
            field(&value, "preflight"),
            field(&value, "syntax"),
            field(&value, "semantic"),
            if acceptable { "acceptable" } else { "rejected" }
        );
        Ok(text)
    }
}

/// `config activate` / `config rollback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMove {
    /// Activate a version.
    Activate,
    /// Roll back to a version.
    Rollback,
}

/// A configuration move.
#[derive(Debug, Clone)]
pub struct ConfigMoveCommand {
    /// Which move.
    pub move_: ConfigMove,
    /// The target version.
    pub id: String,
}

impl ConfigMoveCommand {
    /// The path for this move.
    ///
    /// Written as two literal `format!` calls rather than one with an interpolated
    /// segment. The literal form is what the architecture guard reads to check that
    /// every path the client builds is a route the server registers — with the
    /// segment in a variable the guard sees `/configs/{}/{segment}` and cannot
    /// confirm it. Keeping the two paths explicit costs four lines and keeps that
    /// check meaningful.
    #[must_use]
    pub fn path(&self) -> String {
        match self.move_ {
            ConfigMove::Activate => format!("{API_PREFIX}/configs/{}/activate", self.id),
            ConfigMove::Rollback => format!("{API_PREFIX}/configs/{}/rollback", self.id),
        }
    }
}

impl Command for ConfigMoveCommand {
    fn request(&self) -> Request {
        // An empty object rather than no body: the endpoints accept an optional
        // body, and sending one keeps every write's request shape uniform.
        Request::with_body("POST", self.path(), json!({}))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let word = match self.move_ {
            ConfigMove::Activate => "activated",
            ConfigMove::Rollback => "rolled back to",
        };
        Ok(format!("{word} {}", field(&value, "id")))
    }
}

/// `subscription list`.
#[derive(Debug, Clone)]
pub struct SubscriptionList {
    /// How many to request.
    pub limit: u32,
}

impl Command for SubscriptionList {
    fn request(&self) -> Request {
        Request::get(format!("{API_PREFIX}/subscriptions?limit={}", self.limit))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let items = as_array(&response.body)?;
        if items.is_empty() {
            return Ok("no subscriptions".to_owned());
        }

        let mut lines = Vec::with_capacity(items.len() + 1);
        for item in &items {
            // The URL is deliberately not printed: it routinely embeds a token,
            // and `list` is the command most likely to be pasted into an issue.
            lines.push(format!(
                "{:<28} {:<24} {:<8} {}",
                field(item, "id"),
                field(item, "name"),
                if item.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
                    "enabled"
                } else {
                    "disabled"
                },
                optional(item, "last_update")
            ));
        }
        lines.push(format!("{} subscription(s)", items.len()));
        Ok(lines.join("\n"))
    }
}

/// `subscription update` — runs a conversion and activates the result.
#[derive(Debug, Clone)]
pub struct SubscriptionUpdate {
    /// The subscription to update.
    pub id: String,
}

impl Command for SubscriptionUpdate {
    fn request(&self) -> Request {
        Request::with_body(
            "POST",
            format!("{API_PREFIX}/subscriptions/{}/update", self.id),
            json!({}),
        )
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        // A job identifier is the only thing worth printing: the update runs
        // asynchronously and the caller polls for it.
        Ok(format!(
            "job {} queued for subscription {}",
            field(&value, "id"),
            self.id
        ))
    }
}

/// `jobs` — the recent jobs, or one job when an id is given.
#[derive(Debug, Clone)]
pub struct Jobs {
    /// An optional job id.
    pub id: Option<String>,
}

impl Command for Jobs {
    fn request(&self) -> Request {
        match &self.id {
            Some(id) => Request::get(format!("{API_PREFIX}/jobs/{id}")),
            None => Request::get(format!("{API_PREFIX}/jobs?limit=20")),
        }
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        // One job and a list of jobs have different shapes; the id is the cue, and
        // it is checked before parsing so each branch reads exactly one shape.
        if self.id.is_some() {
            return Ok(render_job(&as_object(&response.body)?));
        }

        let items = as_array(&response.body)?;
        if items.is_empty() {
            return Ok("no jobs".to_owned());
        }
        let mut lines: Vec<String> = items.iter().map(render_job).collect();
        lines.push(format!("{} job(s)", items.len()));
        Ok(lines.join("\n"))
    }
}

/// Renders one job record.
fn render_job(job: &serde_json::Value) -> String {
    let mut text = format!(
        "{:<28} {:<10} {:<16} {}",
        field(job, "id"),
        field(job, "state"),
        field(job, "kind"),
        field(job, "target")
    );
    if let Some(detail) = job.get("detail").and_then(|v| v.as_str()) {
        text.push_str(&format!("\n  {detail}"));
    }
    if let Some(note) = job.get("degradation").and_then(|v| v.as_str()) {
        text.push_str(&format!("\n  note: {note}"));
    }
    text
}

/// `audit` — the audit trail.
#[derive(Debug, Clone)]
pub struct Audit {
    /// How many to request.
    pub limit: u32,
}

impl Command for Audit {
    fn request(&self) -> Request {
        Request::get(format!("{API_PREFIX}/audit?limit={}", self.limit))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let items = as_array(&response.body)?;
        if items.is_empty() {
            return Ok("no audit records".to_owned());
        }
        let mut lines: Vec<String> = items
            .iter()
            .map(|item| {
                format!(
                    "{:<10} {:<24} {:<16} {}",
                    optional(item, "at"),
                    field(item, "actor"),
                    field(item, "action"),
                    optional(item, "outcome")
                )
            })
            .collect();
        lines.push(format!("{} record(s)", items.len()));
        Ok(lines.join("\n"))
    }
}

/// `connections` — the kernel's live connections.
#[derive(Debug, Clone)]
pub struct Connections;

impl Command for Connections {
    fn request(&self) -> Request {
        Request::get(format!("{API_PREFIX}/connections"))
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let items = value
            .get("connections")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if items.is_empty() {
            return Ok("no active connections".to_owned());
        }

        let mut lines = Vec::with_capacity(items.len() + 3);
        // Totals first: they answer "is anything happening at all" before the
        // reader scans individual rows.
        lines.push(format!(
            "up {}  down {}  ({} active)",
            human_bytes(
                value
                    .get("upload_total")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            ),
            human_bytes(
                value
                    .get("download_total")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            ),
            items.len()
        ));
        lines.push(String::new());

        for item in &items {
            let chains = item
                .get("chains")
                .and_then(|v| v.as_array())
                .map(|links| {
                    links
                        .iter()
                        .filter_map(|l| l.as_str())
                        .collect::<Vec<_>>()
                        .join(" -> ")
                })
                .unwrap_or_default();
            lines.push(format!(
                "{} -> {}",
                field(item, "source"),
                field(item, "destination")
            ));
            lines.push(format!(
                "  chain  {}",
                if chains.is_empty() { "-" } else { &chains }
            ));
            lines.push(format!(
                "  bytes  up {} / down {}",
                human_bytes(item.get("upload").and_then(|v| v.as_u64()).unwrap_or(0)),
                human_bytes(item.get("download").and_then(|v| v.as_u64()).unwrap_or(0))
            ));
            // Process identity is present only for an administrative caller. Its
            // absence is not worth a line, because the field's absence is already
            // the whole signal.
            if let Some(process) = item.get("process").and_then(|v| v.as_str()) {
                lines.push(format!(
                    "  proc   {} (uid {})",
                    process,
                    item.get("uid").and_then(|v| v.as_u64()).unwrap_or(0)
                ));
            }
            lines.push(format!("  id     {}", field(item, "id")));
        }
        Ok(lines.join("\n"))
    }
}

/// `connections close <id>` / `connections close --all`.
#[derive(Debug, Clone)]
pub struct CloseConnections {
    /// One connection, or every connection when `None`.
    pub id: Option<String>,
}

impl Command for CloseConnections {
    fn request(&self) -> Request {
        match &self.id {
            Some(id) => Request::with_body(
                "DELETE",
                format!("{API_PREFIX}/connections/{id}"),
                json!({}),
            ),
            // The confirmation the server requires, sent because the caller
            // already confirmed with `--yes`. Both gates exist: this one is the
            // client's, and the server's is not bypassable by a client that
            // forgets to send it.
            None => Request::with_body(
                "DELETE",
                format!("{API_PREFIX}/connections"),
                json!({ "confirm": true }),
            ),
        }
    }

    fn render(&self, response: &Response) -> Result<String, Exit> {
        let value = as_object(&response.body)?;
        let outcome = field(&value, "outcome");
        let closed = value.get("closed").and_then(|v| v.as_u64()).unwrap_or(0);

        let mut text = match &self.id {
            Some(id) => format!("{outcome}: close requested for {id}"),
            None => format!("{outcome}: closed {closed} connection(s)"),
        };
        // A degradation means the action happened but something about reporting it
        // did not. Saying so is the point: the caller must not conclude the record
        // was written.
        if let Some(note) = value.get("degradation").and_then(|v| v.as_str()) {
            text.push_str(&format!("\nnote: {note}"));
        }
        Ok(text)
    }
}

/// Renders a byte count for a human.
///
/// Connection totals reach gigabytes, where a raw number is unreadable and the
/// exact value is not what the reader wants.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `logs` — the kernel's log stream.
///
/// # Not a [`Command`]
///
/// Every other command issues one request and renders one response. This one
/// holds a connection open and prints lines as they arrive, so it has no
/// `render` step to speak of and is driven directly by the dispatcher. Forcing it
/// into the trait would give every other command a streaming concern it does not
/// have.
#[derive(Debug, Clone)]
pub struct Logs {
    /// The minimum level, or `None` for the server's default.
    pub level: Option<String>,
}

impl Logs {
    /// The path this command requests.
    #[must_use]
    pub fn path(&self) -> String {
        match &self.level {
            Some(level) => format!("{API_PREFIX}/logs?level={level}"),
            None => format!("{API_PREFIX}/logs"),
        }
    }
}

/// Formats one streamed event for a terminal.
///
/// The server sends `{"seq":N,"kind":"...","at":...,"data":{...}}`. The rendering
/// turns the interesting part of `data` into a short suffix, so a person watching
/// the stream sees `config.activated v002` rather than a nested object.
///
/// A `lagged` event is called out explicitly. It means the agent dropped events
/// because this client could not keep up — the one case where the reader must act
/// rather than just read, so it must not look like an ordinary line.
pub fn format_event_line(line: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        // An unfamiliar shape is still output the caller asked for.
        return line.to_owned();
    };
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("event");
    let seq = value.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);

    if kind == "lagged" {
        let missed = value
            .get("data")
            .and_then(|d| d.get("missed"))
            .and_then(|m| m.as_u64())
            .unwrap_or(0);
        return format!(
            "{seq:>6}  LAGGED   {missed} event(s) dropped; re-read state for a current view"
        );
    }

    let detail = describe_event(kind, value.get("data"));
    if detail.is_empty() {
        format!("{seq:>6}  {kind}")
    } else {
        format!("{seq:>6}  {kind}  {detail}")
    }
}

/// The one-line summary of an event's payload.
fn describe_event(kind: &str, data: Option<&serde_json::Value>) -> String {
    let Some(data) = data else {
        return String::new();
    };
    let field = |name: &str| data.get(name).and_then(|v| v.as_str()).unwrap_or("");

    match kind {
        "config.activated" | "config.rolled-back" => {
            let version = field("version");
            let instance = field("instance");
            if instance.is_empty() {
                version.to_owned()
            } else {
                format!("{instance} {version}")
            }
        }
        "subscription.updated" => {
            let id = field("subscription");
            let ok = data
                .get("succeeded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            format!("{id} {}", if ok { "succeeded" } else { "failed" })
        }
        "job.progress" => format!("{} {}", field("job"), field("step")),
        "job.finished" => format!("{} {}", field("job"), field("state")),
        "mihomo.log" => format!("{:<7} {}", field("level"), field("message")),
        _ => String::new(),
    }
}

/// Formats one streamed line for a terminal.
///
/// The server sends `{"level":"...","message":"..."}`. Printing the whole object
/// would be unreadable, and printing only the message would drop the severity,
/// which is the main reason to filter or skim. So the level is shown as a short
/// tag and the message follows.
///
/// A line that is not the expected shape is printed as-is rather than dropped: the
/// caller asked for output, and a line whose shape is unfamiliar is still output.
#[must_use]
pub fn format_log_line(line: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(value) => {
            let level = value
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("info");
            let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("");
            format!("{:<7} {message}", short_level(level))
        }
        Err(_) => line.to_owned(),
    }
}

/// Shortens a level label to four characters so the column lines up.
fn short_level(level: &str) -> &str {
    match level.to_ascii_lowercase().as_str() {
        "debug" => "DEBUG",
        "info" => "INFO",
        "warning" => "WARN",
        "error" => "ERROR",
        // An unfamiliar level is shown as-is, truncated to keep the column.
        _ => {
            if level.len() > 5 {
                &level[..5]
            } else {
                level
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;

    fn ok(body: serde_json::Value) -> Response {
        Response {
            status: 200,
            body: body.to_string(),
        }
    }

    /// Every command must address the versioned API. A command that forgot the
    /// prefix would 404 in production and nowhere else.
    #[test]
    fn every_command_addresses_the_versioned_api() {
        let requests = [
            Status {
                instance: "default".into(),
            }
            .request(),
            LifecycleCommand::new(Lifecycle::Start).request(),
            LifecycleCommand::new(Lifecycle::Restart).request(),
            Doctor.request(),
            System.request(),
            KernelVersion.request(),
            KernelUpdate {
                version: "v1".into(),
            }
            .request(),
            ConfigList {
                instance: "default".into(),
                limit: 50,
            }
            .request(),
            ConfigValidate { body: "x".into() }.request(),
            ConfigMoveCommand {
                move_: ConfigMove::Activate,
                id: "v001".into(),
            }
            .request(),
            ConfigMoveCommand {
                move_: ConfigMove::Rollback,
                id: "v001".into(),
            }
            .request(),
            SubscriptionList { limit: 50 }.request(),
            SubscriptionUpdate { id: "sub_1".into() }.request(),
            Jobs { id: None }.request(),
            Jobs {
                id: Some("job_1".into()),
            }
            .request(),
            Audit { limit: 50 }.request(),
        ];
        for request in &requests {
            assert!(
                request.path.starts_with(API_PREFIX),
                "{} does not start with {API_PREFIX}",
                request.path
            );
        }
    }

    /// The path shapes are the contract with the router, so they are asserted
    /// literally. The interfaces crate has its own test for its own table; these
    /// two are what keep the halves in step.
    #[test]
    fn command_paths_match_the_router() {
        assert_eq!(
            Status {
                instance: "d".into()
            }
            .request()
            .path,
            "/api/v1/mihomo?instance=d"
        );
        assert_eq!(
            LifecycleCommand::new(Lifecycle::Stop).request().path,
            "/api/v1/mihomo/stop"
        );
        assert_eq!(
            LifecycleCommand::new(Lifecycle::Reload).request().path,
            "/api/v1/mihomo/reload"
        );
        assert_eq!(Doctor.request().path, "/api/v1/doctor");
        assert_eq!(System.request().path, "/api/v1/system");
        assert_eq!(KernelVersion.request().path, "/api/v1/mihomo/kernel");
        assert_eq!(
            KernelUpdate {
                version: "v".into()
            }
            .request()
            .path,
            "/api/v1/mihomo/kernel"
        );
        assert_eq!(
            ConfigMoveCommand {
                move_: ConfigMove::Activate,
                id: "v001".into()
            }
            .request()
            .path,
            "/api/v1/configs/v001/activate"
        );
        assert_eq!(
            ConfigMoveCommand {
                move_: ConfigMove::Rollback,
                id: "v001".into()
            }
            .request()
            .path,
            "/api/v1/configs/v001/rollback"
        );
        assert_eq!(
            SubscriptionUpdate { id: "s".into() }.request().path,
            "/api/v1/subscriptions/s/update"
        );
        assert_eq!(
            Jobs {
                id: Some("j".into())
            }
            .request()
            .path,
            "/api/v1/jobs/j"
        );
        assert_eq!(Audit { limit: 5 }.request().path, "/api/v1/audit?limit=5");
    }

    /// A lifecycle POST carries a method and a body, because the router registers
    /// these as `post` and a GET would 405.
    #[test]
    fn lifecycle_commands_post() {
        for lifecycle in [
            Lifecycle::Start,
            Lifecycle::Stop,
            Lifecycle::Restart,
            Lifecycle::Reload,
        ] {
            let request = LifecycleCommand::new(lifecycle).request();
            assert_eq!(request.method, "POST", "{lifecycle:?}");
            assert!(request.body.is_some(), "{lifecycle:?}");
        }
    }

    #[test]
    fn a_status_summary_prints_the_state_and_the_active_config() {
        // The fixture is the server's real shape: the state is nested, and `live`
        // and `serving` are separate booleans inside it. An earlier version of this
        // test used a flat `"status": "running"`, which passed while the renderer
        // was reading a field that does not exist.
        let response = ok(json!({
            "instance": "default",
            "name": "default",
            "state": { "status": "Running", "live": true, "serving": true },
            "active_config": "v004",
            "build": null,
            "health": null,
            "last_failure": null,
        }));
        let text = Status {
            instance: "default".into(),
        }
        .render(&response)
        .expect("render");
        assert!(text.contains("Running"), "{text}");
        assert!(text.contains("v004"), "{text}");
        assert!(text.contains("live      true"), "{text}");
        assert!(text.contains("serving   true"), "{text}");
    }

    /// A stopped kernel is the default state and must not look like a broken
    /// response: `live: false` is an answer, not a missing value.
    #[test]
    fn a_stopped_status_is_reported_as_stopped() {
        let response = ok(json!({
            "instance": "default",
            "state": { "status": "Stopped", "live": false, "serving": false },
            "active_config": null,
            "build": null,
            "health": null,
            "last_failure": null,
        }));
        let text = Status {
            instance: "default".into(),
        }
        .render(&response)
        .expect("render");
        assert!(text.contains("Stopped"), "{text}");
        assert!(text.contains("live      false"), "{text}");
    }

    /// A recorded failure is the reason to look at `status` at all, so it must
    /// surface rather than being buried.
    #[test]
    fn a_recorded_failure_is_surfaced() {
        let response = ok(json!({
            "instance": "default",
            "state": { "status": "Stopped", "live": false, "serving": false },
            "active_config": null,
            "build": null,
            "health": null,
            "last_failure": "readiness timed out after 30s",
        }));
        let text = Status {
            instance: "default".into(),
        }
        .render(&response)
        .expect("render");
        assert!(text.contains("readiness timed out"), "{text}");
    }

    #[test]
    fn a_degraded_success_still_says_so() {
        let response = ok(json!({
            "instance": "default",
            "degradation": "TUN was unavailable; running without it",
        }));
        let text = LifecycleCommand::new(Lifecycle::Start)
            .render(&response)
            .expect("render");
        assert!(text.contains("started"), "{text}");
        assert!(text.contains("TUN was unavailable"), "{text}");
    }

    #[test]
    fn config_list_marks_the_active_version() {
        // A bare array, and the flag is `active` — both are the DTO's real shape.
        let response = ok(json!([
            { "id": "v001", "label": "v001", "source": "file", "checksum": "aa", "active": false, "created_at": 0, "activated_at": null },
            { "id": "v002", "label": "v002", "source": "subscription", "checksum": "bb", "active": true, "created_at": 0, "activated_at": 1 },
        ]));
        let text = ConfigList {
            instance: "d".into(),
            limit: 50,
        }
        .render(&response)
        .expect("render");
        let active_line = text
            .lines()
            .find(|l| l.contains("v002"))
            .expect("v002 line");
        assert!(active_line.starts_with('*'), "{text}");
        let other_line = text
            .lines()
            .find(|l| l.contains("v001"))
            .expect("v001 line");
        assert!(!other_line.starts_with('*'), "{text}");
        assert!(text.contains("2 version(s)"), "{text}");
    }

    #[test]
    fn an_empty_list_says_so_rather_than_printing_nothing() {
        // An empty bare array is what the server actually sends for an empty list.
        let response = ok(json!([]));
        assert_eq!(
            ConfigList {
                instance: "d".into(),
                limit: 50
            }
            .render(&response)
            .expect("render"),
            "no configuration versions"
        );
        assert_eq!(
            SubscriptionList { limit: 50 }
                .render(&response)
                .expect("render"),
            "no subscriptions"
        );
        assert_eq!(
            Jobs { id: None }.render(&response).expect("render"),
            "no jobs"
        );
        assert_eq!(
            Audit { limit: 50 }.render(&response).expect("render"),
            "no audit records"
        );
    }

    /// A subscription URL carries credentials, so the list must not print it.
    #[test]
    fn a_subscription_list_does_not_print_the_url() {
        let response = ok(json!([{
                "id": "sub_1",
                "name": "airport",
                "enabled": true,
                "url": "https://example.invalid/sub?token=SECRET",
                "last_update": "2024-01-01T00:00:00Z",
                "interval_seconds": null,
                "is_due": false,
        }]));
        let text = SubscriptionList { limit: 50 }
            .render(&response)
            .expect("render");
        assert!(!text.contains("SECRET"), "the URL leaked: {text}");
        assert!(text.contains("airport"), "{text}");
    }

    #[test]
    fn a_doctor_report_counts_blocking_findings() {
        // The finding fields are `code` and `message`, and the report carries a
        // `verdict` before them.
        let response = ok(json!({
            "verdict": "degraded",
            "findings": [
                { "severity": "error", "code": "tun", "message": "tun: unavailable (EPERM)" },
                { "severity": "warning", "code": "geodata", "message": "geodata: missing" },
            ]
        }));
        let text = Doctor.render(&response).expect("render");
        assert!(text.contains("verdict: degraded"), "{text}");
        assert!(text.contains("2 finding(s), 1 blocking"), "{text}");
        assert!(text.contains("EPERM"), "{text}");
        assert!(text.contains("geodata"), "{text}");
    }

    #[test]
    fn a_clean_doctor_report_is_not_silent() {
        let response = ok(json!({ "findings": [] }));
        assert_eq!(Doctor.render(&response).expect("render"), "no findings");
    }

    /// The system view has to survive a field the server stopped sending, because
    /// a missing section must degrade the output rather than fail the command.
    #[test]
    fn a_system_view_reports_every_capability_the_server_sends() {
        // The real shape: `environment` is an object with short field names, and
        // `capabilities` is a list whose members carry `kind`, `status`, `evidence`.
        let response = ok(json!({
            "environment": {
                "os": "debian",
                "os_version": "12",
                "arch": "aarch64",
                "kernel": "6.1.0",
                "init": "systemd",
                "container": "lxc",
            },
            "capabilities": [
                { "kind": "tun_device", "status": "misconfigured", "evidence": "stat(/dev/net/tun): EPERM" },
                { "kind": "nftables", "status": "unavailable", "evidence": "stat(nft): absent" },
            ],
        }));
        let text = System.render(&response).expect("render");
        assert!(text.contains("debian"), "{text}");
        assert!(text.contains("aarch64"), "{text}");
        assert!(text.contains("lxc"), "{text}");
        assert!(text.contains("misconfigured"), "{text}");
        assert!(text.contains("EPERM"), "{text}");

        // An unknown capability is not dropped: iterating the server's own list is
        // what makes a capability added there appear here without a client change.
        let extended = ok(json!({
            "environment": { "os": "debian", "os_version": null, "arch": "x86_64", "kernel": null, "init": "systemd", "container": "none" },
            "capabilities": [
                { "kind": "something_new", "status": "supported", "evidence": "probe: ok" },
            ],
        }));
        let text = System.render(&extended).expect("render");
        assert!(text.contains("something_new"), "{text}");
        assert!(text.contains("supported"), "{text}");
    }

    /// A response with neither section is reported as such rather than printing an
    /// empty string, which a script would read as "no problems".
    #[test]
    fn an_empty_system_view_says_so() {
        let response = ok(json!({}));
        assert_eq!(
            System.render(&response).expect("render"),
            "the server reported no environment data"
        );
    }
    /// A malformed body is a render failure with a defined exit code, never a
    /// panic: the server's contract changing must not crash the client.
    #[test]
    fn a_malformed_response_is_a_defined_failure() {
        let response = Response {
            status: 200,
            body: "<html>".to_owned(),
        };
        assert_eq!(
            Status {
                instance: "d".into()
            }
            .render(&response),
            Err(Exit::Failure)
        );
        assert_eq!(Doctor.render(&response), Err(Exit::Failure));
        assert_eq!(Jobs { id: None }.render(&response), Err(Exit::Failure));
    }

    /// The close-all request must carry the confirmation the server requires, or
    /// the client would be unable to perform the operation it advertises.
    #[test]
    fn closing_all_sends_the_confirmation() {
        let request = CloseConnections { id: None }.request();
        assert_eq!(request.method, "DELETE");
        assert_eq!(request.path, "/api/v1/connections");
        assert_eq!(
            request.body,
            Some(serde_json::json!({ "confirm": true })),
            "the server refuses a bulk close without this"
        );
    }

    #[test]
    fn closing_one_addresses_that_connection() {
        let request = CloseConnections {
            id: Some("abc".to_owned()),
        }
        .request();
        assert_eq!(request.method, "DELETE");
        assert_eq!(request.path, "/api/v1/connections/abc");
    }

    /// The list form is a GET on the same path the bulk close uses with DELETE, so
    /// the method is what distinguishes them.
    #[test]
    fn listing_connections_is_a_get() {
        let request = Connections.request();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/v1/connections");
    }

    #[test]
    fn a_connection_list_shows_the_chain_and_the_totals() {
        let response = ok(serde_json::json!({
            "upload_total": 2048,
            "download_total": 3_145_728,
            "connections": [{
                "id": "c1",
                "source": "127.0.0.1:51844",
                "destination": "example.com:443",
                "chains": ["Proxy", "Node A"],
                "upload": 79,
                "download": 15116,
                "rule": "DomainSuffix",
                "rule_payload": "example.com",
                "uid": 1000,
                "process": "curl",
                "process_path": "/usr/bin/curl",
                "inbound": "DEFAULT-MIXED",
                "started_at": 1789220403
            }]
        }));
        let text = Connections.render(&response).expect("render");
        assert!(
            text.contains("127.0.0.1:51844 -> example.com:443"),
            "{text}"
        );
        assert!(text.contains("Proxy -> Node A"), "{text}");
        assert!(text.contains("curl"), "{text}");
        assert!(
            text.contains("2.0 KiB"),
            "totals must be human-sized: {text}"
        );
        assert!(text.contains("3.0 MiB"), "{text}");
    }

    /// A non-administrative caller receives no process identity, and the renderer
    /// must not invent a line for it.
    #[test]
    fn a_list_without_process_identity_omits_that_line() {
        let response = ok(serde_json::json!({
            "upload_total": 0,
            "download_total": 0,
            "connections": [{
                "id": "c1",
                "source": "127.0.0.1:1",
                "destination": "example.com:443",
                "chains": ["DIRECT"],
                "upload": 0,
                "download": 0,
                "uid": null,
                "process": null,
                "process_path": null,
                "started_at": null,
                "rule": null,
                "rule_payload": null,
                "inbound": null
            }]
        }));
        let text = Connections.render(&response).expect("render");
        assert!(!text.contains("proc"), "no process line expected: {text}");
        assert!(text.contains("example.com:443"), "{text}");
    }

    #[test]
    fn an_empty_connection_list_says_so() {
        let response = ok(serde_json::json!({
            "upload_total": 0,
            "download_total": 0,
            "connections": []
        }));
        assert_eq!(
            Connections.render(&response).expect("render"),
            "no active connections"
        );
    }

    /// A degradation must be surfaced: the action happened, but the record of it
    /// may not have been written, and the caller has to know that.
    #[test]
    fn a_close_reports_a_degradation() {
        let response = ok(serde_json::json!({
            "outcome": "accepted",
            "closed": 1,
            "degradation": "the audit record could not be written"
        }));
        let text = CloseConnections {
            id: Some("abc".to_owned()),
        }
        .render(&response)
        .expect("render");
        assert!(text.contains("accepted"), "{text}");
        assert!(text.contains("audit record could not be written"), "{text}");
    }

    #[test]
    fn a_config_event_renders_its_version() {
        let text = format_event_line(
            r#"{"seq":1,"kind":"config.activated","at":1,"data":{"instance":"default","version":"v002"}}"#,
        );
        assert!(text.contains("config.activated"), "{text}");
        assert!(text.contains("default v002"), "{text}");
    }

    #[test]
    fn a_subscription_event_renders_its_outcome() {
        let ok = format_event_line(
            r#"{"seq":2,"kind":"subscription.updated","at":1,"data":{"subscription":"sub_1","succeeded":true}}"#,
        );
        assert!(ok.contains("sub_1 succeeded"), "{ok}");

        let failed = format_event_line(
            r#"{"seq":3,"kind":"subscription.updated","at":1,"data":{"subscription":"sub_1","succeeded":false}}"#,
        );
        assert!(failed.contains("sub_1 failed"), "{failed}");
    }

    #[test]
    fn a_job_event_renders_its_step() {
        let text = format_event_line(
            r#"{"seq":4,"kind":"job.progress","at":1,"data":{"job":"job_1","step":"reload"}}"#,
        );
        assert!(text.contains("job_1 reload"), "{text}");
    }

    /// A lag notice is the one line a reader must act on, so it must not look like
    /// an ordinary event.
    #[test]
    fn a_lag_notice_is_called_out() {
        let text = format_event_line(r#"{"seq":9,"kind":"lagged","at":1,"data":{"missed":7}}"#);
        assert!(text.contains("LAGGED"), "{text}");
        assert!(text.contains('7'), "{text}");
        assert!(text.contains("re-read"), "it must say what to do: {text}");
    }

    /// A heartbeat is expected noise and must render as such rather than as
    /// something needing attention.
    #[test]
    fn a_heartbeat_renders_plainly() {
        let text = format_event_line(r#"{"seq":5,"kind":"heartbeat","at":1,"data":{}}"#);
        assert!(text.contains("heartbeat"), "{text}");
        assert!(!text.contains("LAGGED"), "{text}");
    }

    /// An unfamiliar shape is printed rather than dropped: the caller asked for
    /// output, and a line whose shape is new is still output.
    #[test]
    fn an_unfamiliar_event_line_is_printed_as_it_arrived() {
        assert_eq!(format_event_line("not json"), "not json");
        let unknown = format_event_line(r#"{"seq":1,"kind":"something.new","at":1,"data":{}}"#);
        assert!(unknown.contains("something.new"), "{unknown}");
    }

    #[test]
    fn byte_counts_are_human_sized() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(human_bytes(1024 * 1024 * 1024), "1.0 GiB");
    }

    #[test]
    fn the_logs_path_carries_the_level_when_one_is_given() {
        assert_eq!(
            Logs { level: None }.path(),
            "/api/v1/logs",
            "no level means the server's default"
        );
        assert_eq!(
            Logs {
                level: Some("warning".to_owned())
            }
            .path(),
            "/api/v1/logs?level=warning"
        );
    }

    /// A streamed line is rendered with its severity, because the severity is the
    /// reason to filter and the first thing a reader scans.
    #[test]
    fn a_log_line_is_rendered_with_its_level() {
        assert_eq!(
            format_log_line("{\"level\":\"error\",\"message\":\"bind failed\"}"),
            "ERROR   bind failed"
        );
        assert_eq!(
            format_log_line("{\"level\":\"warning\",\"message\":\"slow\"}"),
            "WARN    slow"
        );
        assert_eq!(
            format_log_line("{\"level\":\"info\",\"message\":\"started\"}"),
            "INFO    started"
        );
        assert_eq!(
            format_log_line("{\"level\":\"debug\",\"message\":\"detail\"}"),
            "DEBUG   detail"
        );
    }

    /// A line whose shape is unfamiliar is printed rather than dropped: the caller
    /// asked for output, and an unexpected shape is still output.
    #[test]
    fn an_unexpected_line_is_printed_as_it_arrived() {
        assert_eq!(format_log_line("not json"), "not json");
        assert_eq!(format_log_line(""), "");
        // A missing level is treated as the default the server uses.
        assert_eq!(
            format_log_line("{\"message\":\"no level here\"}"),
            "INFO    no level here"
        );
    }

    /// A level the kernel does not define must not panic on the fixed-width column.
    #[test]
    fn an_unfamiliar_level_does_not_break_the_column() {
        let rendered = format_log_line("{\"level\":\"trace\",\"message\":\"x\"}");
        assert!(rendered.starts_with("trace"), "{rendered}");
        let long = format_log_line("{\"level\":\"extraordinarily-long\",\"message\":\"x\"}");
        assert!(long.len() < 40, "the column must stay bounded: {long}");
    }

    #[test]
    fn one_job_renders_its_detail() {
        let response = ok(json!({
            "id": "job_1",
            "state": "failed",
            "kind": "subscription_update",
            "target": "sub_1",
            "detail": "the converter refused: 404",
        }));
        let text = Jobs {
            id: Some("job_1".into()),
        }
        .render(&response)
        .expect("render");
        assert!(text.contains("failed"), "{text}");
        assert!(text.contains("the converter refused"), "{text}");
    }
}

/// If the prefix ever moved, every command would 404 at once, and the failure
/// would look like a server problem. Asserting the literal here turns that into
/// a single failing test with an obvious name.
#[test]
fn the_client_prefix_matches_the_documented_api_prefix() {
    assert_eq!(API_PREFIX, "/api/v1");
}

#[test]
fn a_get_request_has_no_body() {
    let request = Request::get("/api/v1/health");
    assert_eq!(request.method, "GET");
    assert!(request.body.is_none());
}

#[test]
fn a_body_request_carries_its_body() {
    let request = Request::with_body(
        "POST",
        "/api/v1/configs/validate",
        serde_json::json!({ "body": "x" }),
    );
    assert_eq!(request.method, "POST");
    assert!(request.body.is_some());
}

#[test]
fn the_default_error_rendering_includes_the_servers_own_explanation() {
    struct Probe;
    impl Command for Probe {
        fn request(&self) -> Request {
            Request::get("/")
        }
        fn render(&self, _response: &crate::client::Response) -> Result<String, Exit> {
            Ok(String::new())
        }
    }

    let response = crate::client::Response {
        status: 409,
        body: r#"{"code":"INVALID_STATE","message":"nothing to start"}"#.to_owned(),
    };
    let text = Probe.render_error(&response);
    assert!(text.contains("409"), "{text}");
    assert!(text.contains("INVALID_STATE"), "{text}");
    assert!(text.contains("nothing to start"), "{text}");
}

/// An error body that is not JSON still has to produce a usable message.
#[test]
fn a_non_json_error_still_names_the_status() {
    struct Probe;
    impl Command for Probe {
        fn request(&self) -> Request {
            Request::get("/")
        }
        fn render(&self, _response: &crate::client::Response) -> Result<String, Exit> {
            Ok(String::new())
        }
    }
    let response = crate::client::Response {
        status: 502,
        body: String::new(),
    };
    assert_eq!(
        Probe.render_error(&response),
        "request failed with status 502"
    );
}

#[test]
fn an_unparsable_body_is_a_render_failure_not_a_panic() {
    assert!(as_object("not json").is_err());
    assert!(as_object("{}").is_ok());
}

#[test]
fn field_reads_strings_and_tolerates_absence() {
    let value = serde_json::json!({ "a": "x", "b": 3, "c": null });
    assert_eq!(field(&value, "a"), "x");
    assert_eq!(field(&value, "missing"), "-");
    // A non-string field is not silently coerced into a string here; that is
    // `optional`'s job, which prints it for a human.
    assert_eq!(field(&value, "b"), "-");
    assert_eq!(optional(&value, "b"), "3");
    assert_eq!(optional(&value, "c"), "-");
    assert_eq!(optional(&value, "a"), "x");
}
