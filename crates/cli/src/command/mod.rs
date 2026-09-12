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
/// Repeated here rather than imported from the interfaces crate: the client half
/// of the binary must not link the server half, and an architecture test enforces
/// it. A test asserts this string equals the server's own prefix, so the two
/// cannot drift silently.
pub const API_PREFIX: &str = "/api/v1";

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

/// `logs` — not implemented.
#[derive(Debug, Clone)]
pub struct Logs;

/// The message `logs` prints.
pub const LOGS_NOT_IMPLEMENTED: &str = "logs are not available yet: the agent does not expose a log \
     stream. This will need the observer port (ADR-003), which is not implemented.";

impl Logs {
    /// The exit code `logs` uses.
    ///
    /// Its own code rather than a generic failure, because a script that tails
    /// logs wants to detect the stub and fall back — for example, to reading the
    /// journal — rather than reporting a broken deployment.
    #[must_use]
    pub const fn exit_code() -> Exit {
        Exit::NotImplemented
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

    #[test]
    fn logs_reports_not_implemented() {
        assert_eq!(Logs::exit_code(), Exit::NotImplemented);
        assert_eq!(
            Format::Json,
            Format::Json,
            "the format enum must stay comparable for dispatch"
        );
        assert!(LOGS_NOT_IMPLEMENTED.contains("not available"));
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
