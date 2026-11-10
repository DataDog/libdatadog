// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Closes the APM traces a crash ended and submits them to the agent.
//!
//! A tracer stages each [`TraceToClose`] before the crash: the body it would
//! send, where to send it, and which spans are still open. The receiver owns
//! what ending a span at a crash means, the same for every tracer. Each span
//! the crash ends gets:
//! - its end moved to the crash report's timestamp, never earlier than the end the tracer staged;
//! - `error = true`, with the `error.type` and `error.message` errors intake reports;
//! - `track_error = "false"`: the crash report sent to errors intake is the crash's one Error
//!   Tracking issue, so the span must not open a second;
//! - `error.is_crash = "true"` and `dd.crash_tracking.uuid`, which the trace view uses to find the
//!   crash report and show its issue.
//!
//! The SDK is trusted to stage a complete, correct trace. One the receiver
//! cannot close is not sent, and the report says why.

mod v1;

use crate::crash_info::{CrashInfo, TraceEncoding, TraceToClose};
use chrono::{DateTime, Utc};
use http::uri::PathAndQuery;
use http::{HeaderName, HeaderValue};
use libdd_common::{Endpoint, parse_uri};
use libdd_networking::endpoint::EndpointExt;
use libdd_networking::http_common;
use std::time::Duration;

const SUBMIT_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) const CRASH_UUID_KEY: &str = "dd.crash_tracking.uuid";
/// With [`CRASH_UUID_KEY`], lets the trace view link the span to its crash report's issue.
pub(crate) const IS_CRASH_KEY: &str = "error.is_crash";
/// Error Tracking skips spans with `track_error = "false"`.
pub(crate) const TRACK_ERROR_KEY: &str = "track_error";

/// What the crash report knows about the crash, stamped into every span it ends.
#[derive(Debug)]
pub(crate) struct CrashDetails {
    /// The report's timestamp, in nanoseconds since the Unix epoch.
    pub crash_unix_ns: Option<i64>,
    pub error_type: String,
    /// `None` keeps the message the tracer staged.
    pub error_message: Option<String>,
    pub uuid: String,
}

impl CrashDetails {
    /// The crash time is the report's timestamp; the error is the one the
    /// report sends to errors intake.
    pub fn from_report(crash_info: &CrashInfo) -> Self {
        Self {
            crash_unix_ns: crash_info
                .timestamp
                .parse::<DateTime<Utc>>()
                .ok()
                .and_then(|t| t.timestamp_nanos_opt()),
            error_type: crash_info.error_type(),
            error_message: crash_info.error_message(),
            uuid: crash_info.uuid.clone(),
        }
    }

    /// The string attributes set on every span the crash ends.
    fn attributes(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("error.type", Some(self.error_type.as_str())),
            ("error.message", self.error_message.as_deref()),
            (IS_CRASH_KEY, Some("true")),
            (TRACK_ERROR_KEY, Some("false")),
            (CRASH_UUID_KEY, Some(self.uuid.as_str())),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key, value)))
    }
}

/// A body with the crash stamped into its spans.
struct Closed {
    body: Vec<u8>,
    trace_count: usize,
}

/// A request ready for the agent.
#[derive(Debug)]
pub(crate) struct Submission {
    uri: http::Uri,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Chunks in the body.
    trace_count: usize,
}

/// Takes the report's traces to close and prepares their requests. A trace
/// that cannot be closed is noted in the report's log messages, so this runs
/// before the report is uploaded.
pub(crate) fn prepare_all(crash_info: &mut CrashInfo) -> Vec<Submission> {
    let traces = std::mem::take(&mut crash_info.traces_to_close);
    if traces.is_empty() {
        return vec![];
    }
    let crash = CrashDetails::from_report(crash_info);
    if crash.crash_unix_ns.is_none() {
        crash_info.log_messages.push(format!(
            "APM traces to close keep their staged durations: no crash time in {:?}",
            crash_info.timestamp
        ));
    }
    let mut submissions = Vec::with_capacity(traces.len());
    for (i, trace) in traces.into_iter().enumerate() {
        match prepare(trace, &crash) {
            Ok(submission) => submissions.push(submission),
            Err(e) => crash_info
                .log_messages
                .push(format!("APM trace to close #{i} not sent: {e:#}")),
        }
    }
    submissions
}

/// The trace's request, with its spans closed.
fn prepare(trace: TraceToClose, crash: &CrashDetails) -> anyhow::Result<Submission> {
    let uri = traces_uri(&trace.agent_url, trace.encoding.agent_path())?;
    let closed = match trace.encoding {
        TraceEncoding::V1 => v1::close(&trace.body, trace.span_ids.as_deref(), crash)?,
    };
    Ok(Submission {
        uri,
        headers: trace.headers,
        body: closed.body,
        trace_count: closed.trace_count,
    })
}

/// `{agent_url}{path}`, for `http://` and `unix://` agent URLs alike.
fn traces_uri(agent_url: &str, path: &'static str) -> anyhow::Result<http::Uri> {
    let mut parts = parse_uri(agent_url)?.into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(path));
    Ok(http::Uri::from_parts(parts)?)
}

/// Headers the receiver sets for the body it sends; a tracer's are ignored.
fn receiver_owned(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "content-type"
            | "content-length"
            | "host"
            | "transfer-encoding"
            | "connection"
            | "x-datadog-trace-count"
    )
}

/// POSTs every submission at once and waits for them all. The report is
/// already out, so failures only go to stderr.
pub(crate) async fn submit_all(submissions: Vec<Submission>) {
    let mut set = tokio::task::JoinSet::new();
    for submission in submissions {
        set.spawn(submit(submission));
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("crashtracker: failed to submit APM trace to close: {e:#}"),
            Err(e) => eprintln!("crashtracker: APM trace to close submission panicked: {e}"),
        }
    }
}

/// POSTs one submission and returns the agent's status. Fails on a connection
/// error, timeout, or non-2xx status.
async fn submit(submission: Submission) -> anyhow::Result<u16> {
    let endpoint = Endpoint::from_url(submission.uri);
    let user_agent = format!("libdatadog-crashtracker/{}", env!("CARGO_PKG_VERSION"));
    let mut builder = endpoint
        .to_request_builder(&user_agent)?
        .method(http::Method::POST);
    let headers = builder
        .headers_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid request to {}", endpoint.url))?;
    // The tracer's headers replace the receiver's defaults (user agent,
    // container identity), except those that describe the body.
    for (name, value) in &submission.headers {
        let name = HeaderName::try_from(name.as_str())?;
        if !receiver_owned(&name) {
            headers.insert(name, HeaderValue::try_from(value.as_str())?);
        }
    }
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/msgpack"),
    );
    headers.insert(
        "x-datadog-trace-count",
        HeaderValue::from(submission.trace_count),
    );
    let req = builder.body(http_common::Body::from(submission.body))?;

    let client = http_common::new_client_periodic();
    let response = tokio::time::timeout(SUBMIT_TIMEOUT, client.request(req))
        .await
        .map_err(|_| anyhow::anyhow!("timed out after {SUBMIT_TIMEOUT:?}"))??;
    let status = response.status();
    anyhow::ensure!(status.is_success(), "agent replied {status}");
    Ok(status.as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash_info::ErrorsIntakePayload;
    use crate::crash_info::test_utils::TestInstance;

    fn trace(body: Vec<u8>) -> TraceToClose {
        TraceToClose {
            encoding: TraceEncoding::V1,
            agent_url: "http://agent:8126".to_string(),
            headers: vec![("Datadog-Meta-Lang".to_string(), "c".to_string())],
            span_ids: None,
            body,
        }
    }

    #[cfg_attr(miri, ignore)] // CrashInfo::test_instance spawns a process
    #[test]
    fn crash_details_come_from_the_report() {
        let mut crash_info = CrashInfo::test_instance(1);
        let at = DateTime::from_timestamp_nanos(1_762_800_000_123_456_789);
        crash_info.timestamp = at.to_string();
        let crash = CrashDetails::from_report(&crash_info);
        assert_eq!(crash.crash_unix_ns, Some(1_762_800_000_123_456_789));

        let intake = ErrorsIntakePayload::from_crash_info(&crash_info)
            .unwrap()
            .error;
        assert_eq!(Some(crash.error_type), intake.error_type);
        assert_eq!(crash.error_message, intake.message);
        assert_eq!(crash.uuid, crash_info.uuid);
    }

    #[cfg_attr(miri, ignore)] // CrashInfo::test_instance spawns a process
    #[test]
    fn unreadable_timestamp_is_noted_and_kept_out_of_the_spans() {
        let mut crash_info = CrashInfo::test_instance(1);
        crash_info.timestamp = "yesterday".to_string();
        crash_info.traces_to_close = vec![trace(v1::tests::body(vec![v1::tests::span(7, "dev")]))];
        let submissions = prepare_all(&mut crash_info);
        assert_eq!(submissions.len(), 1);
        assert!(crash_info.traces_to_close.is_empty());
        assert!(
            crash_info
                .log_messages
                .iter()
                .any(|m| m.contains("staged durations"))
        );
    }

    #[cfg_attr(miri, ignore)] // CrashInfo::test_instance spawns a process
    #[test]
    fn traces_that_cannot_be_closed_are_noted_not_sent() {
        let mut crash_info = CrashInfo::test_instance(1);
        crash_info.traces_to_close = vec![trace(vec![0xc1, 0x00])];
        assert!(prepare_all(&mut crash_info).is_empty());
        assert!(
            crash_info
                .log_messages
                .iter()
                .any(|m| m.contains("#0 not sent: v1 decode")),
            "{:?}",
            crash_info.log_messages
        );
    }

    #[test]
    fn traces_uri_handles_http_and_unix_agents() {
        let path = TraceEncoding::V1.agent_path();
        let http = traces_uri("http://agent:8126", path).unwrap();
        assert_eq!(http.to_string(), "http://agent:8126/v1.0/traces");
        let http = traces_uri("http://agent:8126/some/base", path).unwrap();
        assert_eq!(http.path(), path);
        let unix = traces_uri("unix:///var/run/datadog/apm.socket", path).unwrap();
        assert_eq!(unix.scheme_str(), Some("unix"));
        assert_eq!(unix.path(), path);
    }

    /// One HTTP exchange on a local listener: returns the request it read.
    async fn serve_once(listener: tokio::net::TcpListener, status: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let len = text[..head_end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if req.len() >= head_end + 4 + len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let reply =
            format!("HTTP/1.1 {status}\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}");
        sock.write_all(reply.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&req).into_owned()
    }

    fn submission(url: &str, headers: &[(&str, &str)]) -> Submission {
        Submission {
            uri: traces_uri(url, TraceEncoding::V1.agent_path()).unwrap(),
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            body: vec![0x80],
            trace_count: 1,
        }
    }

    #[tokio::test]
    async fn submit_sends_the_tracers_headers_but_owns_the_body_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_once(listener, "200 OK"));
        let headers = [
            ("Datadog-Meta-Lang", "c"),
            ("Datadog-Meta-Tracer-Version", "v0.33.0"),
            ("Datadog-Client-Computed-Top-Level", "yes"),
            ("Content-Type", "text/plain"),
            ("X-Datadog-Trace-Count", "99"),
        ];
        assert_eq!(submit(submission(&url, &headers)).await.unwrap(), 200);

        let seen = server.await.unwrap().to_ascii_lowercase();
        assert!(seen.starts_with("post /v1.0/traces http/1.1"), "{seen}");
        assert!(seen.contains("datadog-meta-lang: c\r\n"));
        assert!(seen.contains("datadog-meta-tracer-version: v0.33.0\r\n"));
        assert!(seen.contains("datadog-client-computed-top-level: yes\r\n"));
        assert!(seen.contains("content-type: application/msgpack\r\n"));
        assert!(!seen.contains("text/plain"));
        assert!(seen.contains("x-datadog-trace-count: 1\r\n"));
        assert!(!seen.contains("x-datadog-trace-count: 99"));
    }

    #[tokio::test]
    async fn submit_fails_on_rejection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_once(listener, "415 Unsupported Media Type"));
        let err = submit(submission(&url, &[])).await.unwrap_err();
        assert!(err.to_string().contains("415"), "{err}");
        server.await.unwrap();
    }
}
