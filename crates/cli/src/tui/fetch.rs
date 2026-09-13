//! The background readers.
//!
//! # Polling *and* streaming, deliberately
//!
//! `EventPublisher`'s contract says events notify and are not a ledger: a
//! subscriber that misses one is expected to re-read state, and the channel is
//! bounded so misses are ordinary rather than exceptional. So this module polls
//! as the source of truth and treats an event as a hint to re-read *sooner*.
//!
//! Polling alone would be slow to react to a change; events alone would show stale
//! data after any missed one. Both are needed, and neither is a fallback.
//!
//! # Why the log panel reads `/api/v1/logs`
//!
//! Not the event stream's `mihomo.log`. That event only exists when the
//! deployment sets `publish_mihomo_logs = true` — off by default because it makes
//! the agent read and redact every kernel line whether or not anyone is watching.
//! The `/logs` endpoint is independent of that switch, so "the log panel works"
//! does not depend on a configuration the operator may not have set. The event
//! panel shows state changes, which is what the stream is for.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::app::{EventLine, GroupRow, JobRow, LogLine, Request, StatusSnapshot, Update};
use crate::client::Client;

/// The endpoints the readers use.
const STATUS_PATH: &str = "/api/v1/mihomo";
const PROXIES_PATH: &str = "/api/v1/mihomo/proxies";
const JOBS_PATH: &str = "/api/v1/jobs?limit=20";
const LOGS_PATH: &str = "/api/v1/logs";
const EVENTS_PATH: &str = "/ws/v1/events";
const SYSTEM_PATH: &str = "/api/v1/system";

/// Starts every reader, returning their handles so the caller can abort them.
///
/// One task per source, all reporting through `updates`: a single channel keeps the
/// main loop's `select!` from growing with each source, and the loop does not need
/// to know which reader produced a value.
#[must_use]
pub fn spawn_all(
    client: Client,
    updates: mpsc::Sender<Update>,
    refresh: tokio::sync::watch::Receiver<u64>,
    actions: mpsc::Receiver<Request>,
) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();

    // The permission probe runs once. It decides whether the write bindings offer
    // anything, so a remote read-only token does not present a stop button that
    // always fails.
    handles.push(tokio::spawn({
        let client = client.clone();
        let updates = updates.clone();
        async move { probe_permission(&client, &updates).await }
    }));

    handles.push(tokio::spawn({
        let client = client.clone();
        let updates = updates.clone();
        let mut gate = Gate::new(refresh.clone());
        async move {
            poll(
                &mut gate,
                Duration::from_secs(2),
                &updates,
                |client| async move {
                    match client.send("GET", STATUS_PATH, None).await {
                        Ok(r) if r.is_success() => parse_status(&r.body).map(Update::Status),
                        Ok(r) => Some(Update::Failed(describe(&r))),
                        Err(e) => Some(Update::Failed(e.to_string())),
                    }
                },
                &client,
            )
            .await;
        }
    }));

    handles.push(tokio::spawn({
        let client = client.clone();
        let updates = updates.clone();
        let mut gate = Gate::new(refresh.clone());
        async move {
            poll(
                &mut gate,
                Duration::from_secs(5),
                &updates,
                |client| async move {
                    match client.send("GET", PROXIES_PATH, None).await {
                        Ok(r) if r.is_success() => parse_groups(&r.body).map(Update::Proxies),
                        // A kernel that is not running has no proxies, and that is
                        // not a connection failure: reporting it as one would make
                        // the status bar claim the agent is unreachable when it is
                        // answering.
                        Ok(_) => Some(Update::Proxies(Vec::new())),
                        Err(e) => Some(Update::Failed(e.to_string())),
                    }
                },
                &client,
            )
            .await;
        }
    }));

    handles.push(tokio::spawn({
        let client = client.clone();
        let updates = updates.clone();
        let mut gate = Gate::new(refresh.clone());
        async move {
            poll(
                &mut gate,
                Duration::from_secs(3),
                &updates,
                |client| async move {
                    match client.send("GET", JOBS_PATH, None).await {
                        Ok(r) if r.is_success() => parse_jobs(&r.body).map(Update::Jobs),
                        Ok(_) => Some(Update::Jobs(Vec::new())),
                        Err(e) => Some(Update::Failed(e.to_string())),
                    }
                },
                &client,
            )
            .await;
        }
    }));

    // The two streams. They do not poll: they hold a connection and report lines
    // as they arrive.
    handles.push(tokio::spawn({
        let client = client.clone();
        let updates = updates.clone();
        async move { stream_logs(&client, &updates).await }
    }));

    handles.push(tokio::spawn({
        let client = client.clone();
        let updates = updates.clone();
        async move { stream_events(&client, &updates).await }
    }));

    // Writes are their own task so a slow start or stop does not delay a read, and
    // so the main loop never blocks on the network: the loop's job is to stay
    // responsive to the keyboard.
    handles.push(tokio::spawn({
        let client = client.clone();
        let updates = updates.clone();
        let mut actions = actions;
        async move {
            while let Some(request) = actions.recv().await {
                let update = match request {
                    Request::Refresh => continue,
                    Request::Start => write(&client, "/api/v1/mihomo/start").await,
                    Request::Stop => write(&client, "/api/v1/mihomo/stop").await,
                };
                // A write invalidates the status, so one is fetched immediately
                // rather than waiting up to the poll interval: the user pressed a
                // key and must see the result.
                if updates.send(update).await.is_err() {
                    return;
                }
                if let Some(update) = read_status(&client).await
                    && updates.send(update).await.is_err()
                {
                    return;
                }
            }
        }
    }));

    handles
}

/// Performs a lifecycle write and reports the outcome.
async fn write(client: &Client, path: &str) -> Update {
    match client
        .send("POST", path, Some(&serde_json::json!({})))
        .await
    {
        Ok(response) if response.is_success() => Update::Recovered,
        Ok(response) => Update::Failed(describe(&response)),
        Err(e) => Update::Failed(e.to_string()),
    }
}

/// Reads the status once.
async fn read_status(client: &Client) -> Option<Update> {
    match client.send("GET", STATUS_PATH, None).await {
        Ok(r) if r.is_success() => parse_status(&r.body).map(Update::Status),
        Ok(_) => None,
        Err(_) => None,
    }
}

/// A signal the main loop uses to ask for an immediate refresh.
///
/// A `watch` channel rather than a queue: *every* polling task must react to one
/// press of `r`, and a queue would deliver it to exactly one of them. Writes are
/// not sent here at all — they go to the action task — so this carries one bit.
#[derive(Debug, Clone)]
pub struct Refresh {
    sender: tokio::sync::watch::Sender<u64>,
}

impl Refresh {
    /// Creates a refresh signal and its receiver.
    #[must_use]
    pub fn new() -> (Self, tokio::sync::watch::Receiver<u64>) {
        let (sender, receiver) = tokio::sync::watch::channel(0u64);
        (Self { sender }, receiver)
    }

    /// Asks every polling task to refresh now.
    pub fn request(&self) {
        // A counter rather than a boolean, so two presses in quick succession are
        // two distinct values and neither is coalesced away.
        self.sender.send_modify(|count| *count += 1);
    }
}

impl Default for Refresh {
    fn default() -> Self {
        Self::new().0
    }
}

/// Watches the refresh signal and sleeps between polls.
struct Gate {
    receiver: tokio::sync::watch::Receiver<u64>,
    seen: u64,
}

impl Gate {
    fn new(receiver: tokio::sync::watch::Receiver<u64>) -> Self {
        let seen = *receiver.borrow();
        Self { receiver, seen }
    }

    /// Waits for the interval or for a refresh request, whichever comes first.
    async fn wait(&mut self, interval: Duration) {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = self.receiver.changed() => {
                self.seen = *self.receiver.borrow();
                let _ = self.seen;
            }
        }
    }
}

/// Polls an endpoint until the task is aborted.
async fn poll<F, Fut>(
    gate: &mut Gate,
    interval: Duration,
    updates: &mpsc::Sender<Update>,
    fetch: F,
    client: &Client,
) where
    F: Fn(Client) -> Fut,
    Fut: std::future::Future<Output = Option<Update>>,
{
    loop {
        if let Some(update) = fetch(client.clone()).await
            && updates.send(update).await.is_err()
        {
            // The main loop is gone, which happens only during shutdown.
            return;
        }
        gate.wait(interval).await;
    }
}

/// Probes what the connected token may do.
async fn probe_permission(client: &Client, updates: &mpsc::Sender<Update>) {
    // A `system` read is the cheapest call every role is allowed. A 403 here means
    // the token cannot even read, which is a configuration problem worth saying;
    // for the write distinction, a successful read means the token is at least
    // read-only or better, and an attempt to write is what would reveal which.
    match client.send("GET", SYSTEM_PATH, None).await {
        Ok(r) if r.is_success() => {
            let _ = updates
                .send(Update::Event(EventLine {
                    kind: "session".to_owned(),
                    summary: format!("connected to {}", client.endpoint().describe()),
                }))
                .await;
        }
        Ok(r) => {
            let _ = updates
                .send(Update::Failed(format!(
                    "the agent refused a status read with status {}",
                    r.status
                )))
                .await;
        }
        Err(e) => {
            let _ = updates.send(Update::Failed(e.to_string())).await;
        }
    }
}

/// Reads the log stream, reconnecting when it ends.
async fn stream_logs(client: &Client, updates: &mpsc::Sender<Update>) {
    loop {
        match client.stream(LOGS_PATH).await {
            Ok(mut stream) => {
                let _ = updates.send(Update::Recovered).await;
                loop {
                    match stream.next_line().await {
                        Ok(Some(line)) => {
                            if updates
                                .send(Update::Log(parse_log_line(&line)))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        // The agent went away. Wait and reattach rather than
                        // exiting: an agent restart must not close the TUI.
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
                let _ = updates
                    .send(Update::Failed("the log stream ended".to_owned()))
                    .await;
            }
            Err(e) => {
                let _ = updates.send(Update::Failed(e.to_string())).await;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Reads the event stream, reconnecting when it ends.
async fn stream_events(client: &Client, updates: &mpsc::Sender<Update>) {
    loop {
        match client.stream(EVENTS_PATH).await {
            Ok(mut stream) => {
                let _ = updates.send(Update::Recovered).await;
                loop {
                    match stream.next_line().await {
                        Ok(Some(line)) => {
                            if let Some(event) = parse_event_line(&line)
                                && updates.send(event).await.is_err()
                            {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
            }
            Err(e) => {
                let _ = updates.send(Update::Failed(e.to_string())).await;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// A one-line description of a refusal.
fn describe(response: &crate::client::Response) -> String {
    match (response.error_code(), response.error_message()) {
        (Some(code), Some(message)) => format!("agent refused: {code} — {message}"),
        (Some(code), None) => format!("agent refused: {code}"),
        _ => format!("agent refused with status {}", response.status),
    }
}

/// Parses a status response.
fn parse_status(body: &str) -> Option<StatusSnapshot> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let state = value.get("state")?;
    Some(StatusSnapshot {
        state: state
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("-")
            .to_owned(),
        live: state.get("live").and_then(|v| v.as_bool()).unwrap_or(false),
        serving: state
            .get("serving")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        active_config: value
            .get("active_config")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        last_failure: value
            .get("last_failure")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
    })
}

/// Parses the proxy list into group rows.
fn parse_groups(body: &str) -> Option<Vec<GroupRow>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let groups = value.get("groups")?.as_array()?;
    Some(
        groups
            .iter()
            .map(|group| GroupRow {
                name: group
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_owned(),
                kind: group
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_owned(),
                now: group
                    .get("now")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned),
                member_count: group
                    .get("members")
                    .and_then(|v| v.as_array())
                    .map_or(0, Vec::len),
            })
            .collect(),
    )
}

/// Parses the job list.
fn parse_jobs(body: &str) -> Option<Vec<JobRow>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let items = value.as_array()?;
    Some(
        items
            .iter()
            .map(|job| JobRow {
                id: job
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_owned(),
                state: job
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_owned(),
                target: job
                    .get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
                    .to_owned(),
            })
            .collect(),
    )
}

/// Parses one NDJSON log line from the agent.
fn parse_log_line(line: &str) -> LogLine {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(value) => LogLine {
            level: value
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("info")
                .to_owned(),
            message: value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
        },
        // The rendering layer would show raw JSON otherwise; the message is what
        // matters and an unparseable line still has one.
        Err(_) => LogLine {
            level: "info".to_owned(),
            message: line.to_owned(),
        },
    }
}

/// Parses one NDJSON event line.
///
/// Returns `None` for a heartbeat: it carries no information a person needs, and
/// filling the panel with them would bury the events that matter.
fn parse_event_line(line: &str) -> Option<Update> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = value.get("kind").and_then(|v| v.as_str())?;
    if kind == "heartbeat" {
        return None;
    }
    let summary = match kind {
        "lagged" => {
            let missed = value
                .get("data")
                .and_then(|d| d.get("missed"))
                .and_then(|m| m.as_u64())
                .unwrap_or(0);
            format!("{missed} event(s) dropped; re-read state for a current view")
        }
        _ => {
            let data = value.get("data").cloned().unwrap_or_default();
            data.as_object()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(&v.to_string())))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        }
    };
    Some(Update::Event(EventLine {
        kind: kind.to_owned(),
        summary,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_response_parses() {
        let body = r#"{"instance":"default","name":"default",
            "state":{"status":"Running","live":true,"serving":true},
            "active_config":"v002","build":null,"health":null,
            "last_failure":"readiness timed out"}"#;
        let snapshot = parse_status(body).expect("parsed");
        assert_eq!(snapshot.state, "Running");
        assert!(snapshot.live);
        assert!(snapshot.serving);
        assert_eq!(snapshot.active_config.as_deref(), Some("v002"));
        assert_eq!(
            snapshot.last_failure.as_deref(),
            Some("readiness timed out")
        );
    }

    #[test]
    fn a_status_without_a_state_is_refused() {
        assert!(parse_status("{}").is_none());
        assert!(parse_status("not json").is_none());
    }

    #[test]
    fn a_proxy_group_parses_with_its_members() {
        let body = r#"{"groups":[{"name":"Proxy","kind":"select","now":"Node A",
            "members":["Node A","Node B"]}],"proxies":[]}"#;
        let groups = parse_groups(body).expect("parsed");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Proxy");
        assert_eq!(groups[0].now.as_deref(), Some("Node A"));
        assert_eq!(groups[0].member_count, 2);
    }

    /// A group with no selection is representable, and is not the same as a group
    /// with none fetched.
    #[test]
    fn a_group_without_a_selection_parses() {
        let body = r#"{"groups":[{"name":"G","kind":"select","now":null,"members":[]}]}"#;
        let groups = parse_groups(body).expect("parsed");
        assert!(groups[0].now.is_none());
        assert_eq!(groups[0].member_count, 0);
    }

    #[test]
    fn a_job_list_parses() {
        let body = r#"[{"id":"j1","state":"running","target":"default"}]"#;
        let jobs = parse_jobs(body).expect("parsed");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "j1");
        assert_eq!(jobs[0].state, "running");
    }

    #[test]
    fn a_log_line_parses_its_level_and_message() {
        let line = r#"{"level":"error","message":"bind failed"}"#;
        let parsed = parse_log_line(line);
        assert_eq!(parsed.level, "error");
        assert_eq!(parsed.message, "bind failed");
    }

    /// An unparseable line still has a message: showing raw JSON is better than
    /// showing nothing.
    #[test]
    fn an_unparseable_log_line_is_kept_verbatim() {
        let parsed = parse_log_line("not json at all");
        assert_eq!(parsed.message, "not json at all");
    }

    #[test]
    fn an_event_line_parses_into_a_summary() {
        let line = r#"{"seq":1,"kind":"config.activated","at":1,
            "data":{"instance":"default","version":"v002"}}"#;
        match parse_event_line(line).expect("parsed") {
            Update::Event(event) => {
                assert_eq!(event.kind, "config.activated");
                assert!(event.summary.contains("version=v002"), "{}", event.summary);
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }

    /// A heartbeat is dropped: it carries nothing a person needs, and showing them
    /// would bury the events that matter.
    #[test]
    fn a_heartbeat_is_dropped() {
        let line = r#"{"seq":5,"kind":"heartbeat","at":1,"data":{}}"#;
        assert!(parse_event_line(line).is_none());
    }

    /// A lag notice must survive: it is the one event the reader has to act on.
    #[test]
    fn a_lag_notice_is_kept_and_explains_itself() {
        let line = r#"{"seq":9,"kind":"lagged","at":1,"data":{"missed":7}}"#;
        match parse_event_line(line).expect("parsed") {
            Update::Event(event) => {
                assert_eq!(event.kind, "lagged");
                assert!(event.summary.contains('7'), "{}", event.summary);
                assert!(
                    event.summary.contains("re-read"),
                    "it must say what to do: {}",
                    event.summary
                );
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }
}
