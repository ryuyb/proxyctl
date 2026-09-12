//! Connection inspection and termination.
//!
//! # Snapshot, not stream
//!
//! The kernel offers `/connections` as a chunked stream that pushes a new snapshot
//! every second, and this adapter deliberately uses the one-shot `send` path
//! instead. "List the current connections" is a question with an answer, not a
//! subscription: modelling it as a stream would make every caller read one frame
//! and drop the rest, and would leave a connection open for a request that is
//! complete.
//!
//! # What this adapter normalises
//!
//! The kernel reports a field as an empty string when it does not apply or could
//! not be determined — `rule: ""` for unmatched traffic, `process: ""` when the
//! socket's owner is unknown. It does not use `null`. So every optional string
//! passes through [`non_empty`], which turns `""` into `None` once, here, rather
//! than leaving each consumer to rediscover the convention.
//!
//! Measured against mihomo v1.19.30.

use async_trait::async_trait;
use proxy_application::ports::PortError;
use proxy_application::ports::mihomo_connection_ops::{
    CloseOutcome, ConnectionList, ConnectionView, MihomoConnectionOps,
};
use proxy_domain::shared::time::Timestamp;
use serde::Deserialize;

use super::transport::{Request, Response, Transport};

/// Reads and terminates the kernel's connections.
pub struct KernelConnections<T> {
    transport: T,
}

impl<T> KernelConnections<T> {
    /// Builds the adapter over `transport`.
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// The transport in use, for diagnostics and tests.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

/// The kernel's `/connections` body.
///
/// Only the fields this adapter maps are declared, and unknown ones are ignored:
/// the measured record carries a dozen more (`sourceGeoIP`, `dscp`, `specialRules`,
/// …) that this port does not model, and `deny_unknown_fields` would break the
/// adapter the moment the kernel added one.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireConnections {
    #[serde(default)]
    upload_total: u64,
    #[serde(default)]
    download_total: u64,
    // The kernel sends `null`, not `[]`, when there are no connections — measured
    // on an idle instance: `{"downloadTotal":0,"uploadTotal":0,"connections":null,
    // "memory":0}`. Without `deserialize_with` the adapter fails on a body that is
    // perfectly valid, and the only symptom is a 502 whenever nothing is happening
    // — which is most of the time, and the opposite of when a developer looks.
    #[serde(default, deserialize_with = "null_is_empty")]
    connections: Vec<WireConnection>,
}

/// Treats an explicit `null` the same as an absent field.
fn null_is_empty<'de, D>(deserializer: D) -> Result<Vec<WireConnection>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<WireConnection>>::deserialize(deserializer)?.unwrap_or_default())
}

/// One connection as the kernel reports it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireConnection {
    id: String,
    #[serde(default)]
    upload: u64,
    #[serde(default)]
    download: u64,
    /// RFC 3339 with an offset, such as `2026-09-12T21:40:03.325015696+08:00`.
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    chains: Vec<String>,
    #[serde(default)]
    rule: String,
    #[serde(default)]
    rule_payload: String,
    #[serde(default)]
    metadata: WireMetadata,
}

/// The connection's metadata block.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireMetadata {
    // `rename_all = "camelCase"` turns `source_ip` into `sourceIp`, but the kernel
    // spells these `sourceIP` and `destinationIP` — an acronym it keeps uppercase.
    // Without the explicit rename the fields silently default to empty, and the
    // only visible symptom is an address that reads `:51844`. Found by asserting
    // the mapped values against a record captured from a live kernel.
    #[serde(default, rename = "sourceIP")]
    source_ip: String,
    #[serde(default)]
    source_port: String,
    #[serde(default, rename = "destinationIP")]
    destination_ip: String,
    #[serde(default)]
    destination_port: String,
    /// The sniffed host name, when there is one.
    #[serde(default)]
    host: String,
    #[serde(default)]
    inbound_name: String,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    process: String,
    #[serde(default)]
    process_path: String,
}

impl WireMetadata {
    /// Renders `host:port`, preferring the sniffed host over the raw address.
    ///
    /// The host is what a reader recognises; the address is what the kernel
    /// connected to. Showing the host when sniffing succeeded is the more useful
    /// answer, and falling back to the address keeps the field populated for
    /// traffic that carries no name.
    fn address(&self, port: &str) -> String {
        let host = if self.host.is_empty() {
            self.destination_ip.as_str()
        } else {
            self.host.as_str()
        };
        if port.is_empty() {
            host.to_owned()
        } else {
            format!("{host}:{port}")
        }
    }

    fn source(&self) -> String {
        if self.source_port.is_empty() {
            self.source_ip.clone()
        } else {
            format!("{}:{}", self.source_ip, self.source_port)
        }
    }
}

/// An empty string means "not available", not "empty".
fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// Parses the kernel's timestamp.
///
/// The format is RFC 3339 with nanosecond precision and an offset, which
/// `Timestamp` cannot hold — it stores whole seconds. Only the seconds are kept,
/// because that is the resolution this port models, and the alternative would be
/// to claim a precision the type does not have.
fn parse_start(raw: &str) -> Option<Timestamp> {
    let (date, time) = raw.split_once('T')?;

    // The offset, which must be applied: the kernel reports its local time, so
    // `21:40:03+08:00` is the same instant as `13:40:03Z`. Ignoring the offset
    // would shift every timestamp by the host's zone — eight hours here — and the
    // value would still look plausible, which is what makes it worth a comment.
    let (clock, offset_seconds) = split_offset(time)?;
    let time = clock;
    let mut parts = time.split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next()?.parse().ok()?;
    let seconds: i64 = parts.next()?.split('.').next()?.parse().ok()?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;

    // A small civil-date conversion, so the adapter needs no date library for one
    // field. It is the standard days-from-civil algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    Some(Timestamp::from_unix_seconds(
        days * 86_400 + hours * 3600 + minutes * 60 + seconds - offset_seconds,
    ))
}

/// Separates the clock from its offset, returning the offset in seconds.
///
/// Accepts `Z`, `+HH:MM`, `-HH:MM`, and a bare offset without minutes.
fn split_offset(time: &str) -> Option<(&str, i64)> {
    if let Some(clock) = time.strip_suffix('Z') {
        return Some((clock, 0));
    }
    // The sign that introduces the offset is the last one, since the clock itself
    // contains none.
    let position = time.rfind(['+', '-'])?;
    let (clock, offset) = time.split_at(position);
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let body = &offset[1..];
    let (hours, minutes) = match body.split_once(':') {
        Some((h, m)) => (h, m),
        None => (body, "0"),
    };
    let hours: i64 = hours.parse().ok()?;
    let minutes: i64 = minutes.parse().ok()?;
    Some((clock, sign * (hours * 3600 + minutes * 60)))
}

#[async_trait]
impl<T: Transport + 'static> MihomoConnectionOps for KernelConnections<T> {
    async fn connections(&self) -> Result<ConnectionList, PortError> {
        let response = self.transport.send(Request::get("/connections")).await?;
        if !response.is_success() {
            return Err(refusal(&response, "listing connections"));
        }
        parse_connections(&response.body)
    }

    async fn close_connection(&self, id: &str) -> Result<CloseOutcome, PortError> {
        let path = format!("/connections/{id}");
        let response = self.transport.send(Request::delete(path)).await?;
        Ok(outcome(&response))
    }

    async fn close_all(&self) -> Result<usize, PortError> {
        // Counted around the request, because the kernel does not report how many
        // it closed. Without this the caller could not tell "closed 200" from
        // "there was nothing to close", and an audit record saying only "closed
        // all" would be worth very little.
        let before = self.connections().await?.connections.len();
        let response = self.transport.send(Request::delete("/connections")).await?;
        match outcome(&response) {
            CloseOutcome::Accepted => Ok(before),
            CloseOutcome::Rejected { http_status } => Err(PortError::InvalidResponse(format!(
                "the kernel refused to close all connections with status {http_status}"
            ))),
        }
    }
}

/// Maps a status onto the close outcome.
fn outcome(response: &Response) -> CloseOutcome {
    // 204 is what the kernel returns, for a connection it closed *and* for one
    // that never existed. Nothing finer can be reported, so nothing finer is
    // claimed.
    if response.is_success() {
        CloseOutcome::Accepted
    } else {
        CloseOutcome::Rejected {
            http_status: response.status,
        }
    }
}

/// Turns a refused request into an error.
fn refusal(response: &Response, doing: &str) -> PortError {
    PortError::InvalidResponse(format!(
        "the kernel refused {doing} with status {}",
        response.status
    ))
}

/// Parses the connection list.
///
/// # Errors
///
/// Returns [`PortError::InvalidResponse`] when the body is not the expected shape.
pub fn parse_connections(body: &str) -> Result<ConnectionList, PortError> {
    let wire: WireConnections = serde_json::from_str(body).map_err(|e| {
        PortError::InvalidResponse(format!(
            "the connection list is not the expected shape: {e}"
        ))
    })?;

    Ok(ConnectionList {
        upload_total: wire.upload_total,
        download_total: wire.download_total,
        connections: wire
            .connections
            .into_iter()
            .map(|c| ConnectionView {
                id: c.id,
                source: c.metadata.source(),
                destination: c.metadata.address(&c.metadata.destination_port.clone()),
                rule: non_empty(&c.rule),
                rule_payload: non_empty(&c.rule_payload),
                // The kernel sends `[""]` for traffic that matched no provider, so
                // an entry that is empty is dropped rather than shown as a blank
                // link in the chain.
                chains: c
                    .chains
                    .into_iter()
                    .filter(|link| !link.is_empty())
                    .collect(),
                // A uid of `0` is ambiguous — root really is 0 — so the field is
                // taken as reported. The kernel leaves it `0` when it could not
                // determine the owner, which is the same value root would have;
                // that ambiguity is the kernel's, and inventing a distinction here
                // would be worse than reporting what was sent.
                uid: c.metadata.uid,
                process: non_empty(&c.metadata.process),
                // The one field that is not simply passed through: an empty
                // `processPath` means the kernel could not read it, and this is
                // also the field the interface layer withholds from non-admins.
                process_path: non_empty(&c.metadata.process_path),
                started_at: c.start.as_deref().and_then(parse_start),
                upload: c.upload,
                download: c.download,
                inbound: non_empty(&c.metadata.inbound_name),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The requests a test's transport recorded, as `(method, path)`.
    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    /// A transport that replays canned responses and records requests.
    struct ScriptedTransport {
        responses: Mutex<Vec<Result<Response, PortError>>>,
        seen: Seen,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<Response>) -> (Self, Seen) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    responses: Mutex::new(responses.into_iter().map(Ok).collect()),
                    seen: Arc::clone(&seen),
                },
                seen,
            )
        }
    }

    #[async_trait]
    impl Transport for ScriptedTransport {
        async fn send(&self, request: Request) -> Result<Response, PortError> {
            self.seen
                .lock()
                .expect("lock")
                .push((request.method.as_str().to_owned(), request.path.clone()));
            let mut responses = self.responses.lock().expect("lock");
            if responses.is_empty() {
                return Ok(Response {
                    status: 204,
                    body: String::new(),
                });
            }
            responses.remove(0)
        }

        async fn open_stream(
            &self,
            _request: Request,
        ) -> Result<
            proxy_application::ports::mihomo_observer::BoxStream<Result<String, PortError>>,
            PortError,
        > {
            Err(PortError::Transport("not used".to_owned()))
        }

        fn timeout(&self) -> std::time::Duration {
            std::time::Duration::from_secs(1)
        }

        fn describe(&self) -> String {
            "scripted".to_owned()
        }
    }

    /// The exact body measured from a live kernel, so the mapping is verified
    /// against reality rather than against a convenient fixture.
    const MEASURED: &str = r#"{
      "uploadTotal": 79,
      "downloadTotal": 15116,
      "memory": 42098688,
      "connections": [
        {
          "id": "1f733396-4967-4e16-8fcf-62e659ddefad",
          "upload": 79,
          "download": 15116,
          "start": "2026-09-12T21:40:03.325015696+08:00",
          "chains": ["DIRECT"],
          "providerChains": [""],
          "rule": "",
          "rulePayload": "",
          "metadata": {
            "network": "tcp",
            "type": "HTTP",
            "sourceIP": "127.0.0.1",
            "sourcePort": "51844",
            "destinationIP": "127.0.0.1",
            "destinationPort": "18081",
            "host": "",
            "dnsMode": "normal",
            "inboundName": "DEFAULT-MIXED",
            "inboundPort": "7890",
            "uid": 0,
            "process": "",
            "processPath": ""
          }
        }
      ]
    }"#;

    #[test]
    fn the_measured_record_maps_field_by_field() {
        let list = parse_connections(MEASURED).expect("must parse");
        assert_eq!(list.upload_total, 79);
        assert_eq!(list.download_total, 15116);
        assert_eq!(list.connections.len(), 1);

        let c = &list.connections[0];
        assert_eq!(c.id, "1f733396-4967-4e16-8fcf-62e659ddefad");
        assert_eq!(c.source, "127.0.0.1:51844");
        // No sniffed host, so the address is shown.
        assert_eq!(c.destination, "127.0.0.1:18081");
        assert_eq!(c.chains, vec!["DIRECT".to_owned()]);
        assert_eq!(c.inbound.as_deref(), Some("DEFAULT-MIXED"));
        assert_eq!(c.upload, 79);
        assert_eq!(c.download, 15116);
    }

    /// The empty strings the kernel sends for "not applicable" become `None`.
    #[test]
    fn empty_strings_become_none() {
        let list = parse_connections(MEASURED).expect("must parse");
        let c = &list.connections[0];
        assert!(c.rule.is_none(), "an empty rule means nothing matched");
        assert!(c.rule_payload.is_none());
        assert!(
            c.process.is_none(),
            "an empty process means it could not be read"
        );
        assert!(c.process_path.is_none());
    }

    /// A provider chain entry that is empty is dropped, not shown as a blank link.
    #[test]
    fn empty_chain_entries_are_dropped() {
        let body = r#"{"connections":[{"id":"x","chains":["Proxy",""],"metadata":{}}]}"#;
        let list = parse_connections(body).expect("must parse");
        assert_eq!(list.connections[0].chains, vec!["Proxy".to_owned()]);
    }

    /// A chain with more than one link survives in order: that is the whole reason
    /// the field is a vector.
    #[test]
    fn a_multi_link_chain_is_preserved_in_order() {
        let body = r#"{"connections":[{"id":"x","chains":["Proxy","Node A"],"metadata":{}}]}"#;
        let list = parse_connections(body).expect("must parse");
        assert_eq!(
            list.connections[0].chains,
            vec!["Proxy".to_owned(), "Node A".to_owned()]
        );
    }

    /// The sniffed host is preferred over the raw address, because it is what a
    /// reader recognises.
    #[test]
    fn a_sniffed_host_is_shown_instead_of_the_address() {
        let body = r#"{"connections":[{"id":"x","metadata":{
            "host":"example.com","destinationIP":"93.184.216.34","destinationPort":"443"}}]}"#;
        let list = parse_connections(body).expect("must parse");
        assert_eq!(list.connections[0].destination, "example.com:443");
    }

    /// The nanosecond-precision timestamp parses to whole seconds, and the offset
    /// is applied.
    #[test]
    fn the_kernels_timestamp_parses_to_seconds() {
        // 2026-09-12T21:40:03+08:00 is 2026-09-12T13:40:03Z, which is 1789220403.
        // The offset has to be subtracted; without that this returns a value eight
        // hours late, which still looks like a valid timestamp.
        let parsed = parse_start("2026-09-12T21:40:03.325015696+08:00").expect("must parse");
        assert_eq!(parsed.as_unix_seconds(), 1_789_220_403);

        // A negative offset adds instead of subtracting.
        let west = parse_start("2026-09-12T06:40:03-07:00").expect("must parse");
        assert_eq!(
            west.as_unix_seconds(),
            1_789_220_403,
            "the same instant expressed in another zone must compare equal"
        );

        // A UTC value with `Z` works too.
        let utc = parse_start("1970-01-01T00:00:00Z").expect("must parse");
        assert_eq!(utc.as_unix_seconds(), 0);
    }

    #[test]
    fn a_malformed_timestamp_is_none_rather_than_a_panic() {
        for bad in ["", "not a date", "2026-09-12", "2026-13-45T99:99:99Z"] {
            // The last one must not panic; an out-of-range value is the caller's
            // problem to notice, not a reason to crash the adapter.
            let _ = parse_start(bad);
        }
        assert!(parse_start("2026-09-12").is_none());
        assert!(parse_start("nonsense").is_none());
    }

    /// `uid` is passed through as reported: `0` is both "root" and "unknown", and
    /// guessing between them would be inventing information.
    #[test]
    fn a_uid_is_reported_as_sent() {
        let body = r#"{"connections":[{"id":"x","metadata":{"uid":0}}]}"#;
        let list = parse_connections(body).expect("must parse");
        assert_eq!(list.connections[0].uid, Some(0));

        let body = r#"{"connections":[{"id":"x","metadata":{"uid":1000}}]}"#;
        let list = parse_connections(body).expect("must parse");
        assert_eq!(list.connections[0].uid, Some(1000));
    }

    /// A record with no metadata at all must not fail: the adapter's job is to
    /// report what is there.
    #[test]
    fn a_connection_without_metadata_still_parses() {
        let body = r#"{"connections":[{"id":"x"}]}"#;
        let list = parse_connections(body).expect("must parse");
        assert_eq!(list.connections.len(), 1);
        assert_eq!(list.connections[0].id, "x");
        assert!(list.connections[0].uid.is_none());
    }

    /// An empty list is a valid answer, not an error.
    #[test]
    fn an_empty_list_parses() {
        let list = parse_connections(r#"{"uploadTotal":0,"downloadTotal":0,"connections":[]}"#)
            .expect("must parse");
        assert!(list.connections.is_empty());
    }

    /// The idle body, copied from a live kernel: `connections` is `null`, not `[]`.
    ///
    /// This is the shape a real deployment sees almost all the time, and getting it
    /// wrong makes every `connections list` fail with a 502 on a healthy system.
    #[test]
    fn the_measured_idle_body_parses() {
        let list = parse_connections(
            r#"{"downloadTotal":0,"uploadTotal":0,"connections":null,"memory":0}"#,
        )
        .expect("the idle body must parse");
        assert!(list.connections.is_empty());
        assert_eq!(list.upload_total, 0);
    }

    /// An absent field is also empty, so a kernel that omitted it would still work.
    #[test]
    fn an_absent_connections_field_is_empty() {
        let list = parse_connections(r#"{"uploadTotal":5,"downloadTotal":6}"#).expect("must parse");
        assert!(list.connections.is_empty());
        assert_eq!(list.upload_total, 5);
    }

    #[test]
    fn a_malformed_body_is_refused() {
        assert!(parse_connections("not json").is_err());
        assert!(parse_connections(r#"{"connections": "not an array"}"#).is_err());
    }

    /// The endpoints used, asserted literally: reading must be a GET, and closing
    /// must be a DELETE on the documented paths.
    #[tokio::test]
    async fn the_endpoints_match_the_kernels_routes() {
        let (transport, seen) = ScriptedTransport::new(vec![
            Response {
                status: 200,
                body: MEASURED.to_owned(),
            },
            Response {
                status: 204,
                body: String::new(),
            },
            Response {
                status: 200,
                body: MEASURED.to_owned(),
            },
            Response {
                status: 204,
                body: String::new(),
            },
        ]);
        let ops = KernelConnections::new(transport);

        ops.connections().await.expect("list");
        ops.close_connection("abc").await.expect("close");
        ops.close_all().await.expect("close all");

        let calls = seen.lock().expect("lock").clone();
        assert_eq!(
            calls,
            vec![
                ("GET".to_owned(), "/connections".to_owned()),
                ("DELETE".to_owned(), "/connections/abc".to_owned()),
                ("GET".to_owned(), "/connections".to_owned()),
                ("DELETE".to_owned(), "/connections".to_owned()),
            ]
        );
    }

    /// `close_all` reports the count it measured before the request, because the
    /// kernel does not report one.
    #[tokio::test]
    async fn close_all_reports_how_many_were_active() {
        let (transport, _) = ScriptedTransport::new(vec![
            Response {
                status: 200,
                body: MEASURED.to_owned(),
            },
            Response {
                status: 204,
                body: String::new(),
            },
        ]);
        let ops = KernelConnections::new(transport);
        assert_eq!(ops.close_all().await.expect("close all"), 1);
    }

    /// A refusal is reported as a business outcome, not an error, because the
    /// kernel answering "no" is an answer. The alternative — an error — would make
    /// a caller retry a request that will keep being refused.
    #[tokio::test]
    async fn a_refused_close_is_an_outcome_not_an_error() {
        let (transport, _) = ScriptedTransport::new(vec![Response {
            status: 500,
            body: "nope".to_owned(),
        }]);
        let ops = KernelConnections::new(transport);
        assert_eq!(
            ops.close_connection("x").await.expect("not an error"),
            CloseOutcome::Rejected { http_status: 500 }
        );
    }

    /// A refused *list* is an error: there is no partial answer worth returning.
    #[tokio::test]
    async fn a_refused_list_is_an_error() {
        let (transport, _) = ScriptedTransport::new(vec![Response {
            status: 500,
            body: "nope".to_owned(),
        }]);
        let ops = KernelConnections::new(transport);
        assert!(ops.connections().await.is_err());
    }

    /// `close_all` must not claim success when the kernel refused, or an operator
    /// would believe connections were closed.
    #[tokio::test]
    async fn a_refused_close_all_is_an_error() {
        let (transport, _) = ScriptedTransport::new(vec![
            Response {
                status: 200,
                body: MEASURED.to_owned(),
            },
            Response {
                status: 500,
                body: "nope".to_owned(),
            },
        ]);
        let ops = KernelConnections::new(transport);
        assert!(ops.close_all().await.is_err());
    }
}
