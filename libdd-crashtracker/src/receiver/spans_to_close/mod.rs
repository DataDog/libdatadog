// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Finishes the spans a crash ended and submits them to the agent.
//!
//! An SDK describes its open spans before the crash ([`SpansToClose`]); the
//! receiver owns the rest, the same for every SDK: the trace format, the
//! request, and what ending a span at a crash means. Each span the crash ends
//! gets:
//! - its end at the crash report's timestamp (the receive time if the report has none);
//! - `error = true`, with the `error.type` and `error.message` errors intake reports;
//! - `error.is_crash = "true"` and `dd.crash_tracking.uuid`, which the trace view uses to find the
//!   crash report and show its issue.
//!
//! The spans are built as a v1 payload and finished through the shared
//! `libdd_trace_model` mutation traits, so no SDK format is decoded here.
//!
//! The SDK is trusted to describe complete, correct spans. A block the
//! receiver cannot finish is not sent, and the report says why.

use crate::crash_info::{
    Attributes, CrashInfo, ScalarValue, SpanToClose, SpansToClose, TraceToClose,
};
use chrono::{DateTime, Utc};
use http::uri::PathAndQuery;
use http::{HeaderName, HeaderValue};
use libdd_common::{Endpoint, parse_uri};
use libdd_networking::endpoint::EndpointExt;
use libdd_networking::http_common;
use libdd_trace_model::{AttributesMut, SpanMut, TraceChunkMut};
use libdd_trace_utils::msgpack_encoder::v1 as encode_v1;
use libdd_trace_utils::span::SliceData;
use libdd_trace_utils::span::trace_utils_v1::compute_top_level_span;
use libdd_trace_utils::span::v1::{
    AttributeValue, Span, SpanKind, TraceChunk, TracerPayload, TracerPayloadSlice,
};
use libdd_trace_utils::span::vec_map::VecMap;
use std::borrow::Cow;
use std::time::Duration;

const SUBMIT_TIMEOUT: Duration = Duration::from_secs(3);
/// Where v1 payloads go.
const V1_TRACES_PATH: &str = "/v1.0/traces";
/// Kept by the agent's samplers: crashed spans are rare and worth keeping.
const USER_KEEP: i32 = 2;

pub(crate) const CRASH_UUID_KEY: &str = "dd.crash_tracking.uuid";
/// With [`CRASH_UUID_KEY`], lets the trace view link the span to its crash report's issue.
pub(crate) const IS_CRASH_KEY: &str = "error.is_crash";

/// What the crash report knows about the crash, stamped into every span it ends.
#[derive(Debug)]
pub(crate) struct CrashDetails {
    /// When the spans end, in nanoseconds since the Unix epoch.
    pub crash_unix_ns: i64,
    /// Whether `crash_unix_ns` is the report's timestamp, not the receive time.
    pub from_report: bool,
    pub error_type: String,
    /// `None` keeps the message the SDK staged.
    pub error_message: Option<String>,
    pub uuid: String,
}

impl CrashDetails {
    /// The crash time is the report's timestamp; the error is the one the
    /// report sends to errors intake.
    pub fn from_report(crash_info: &CrashInfo) -> Self {
        let reported = crash_info
            .timestamp
            .parse::<DateTime<Utc>>()
            .ok()
            .and_then(|t| t.timestamp_nanos_opt());
        Self {
            crash_unix_ns: reported
                .or_else(|| Utc::now().timestamp_nanos_opt())
                .unwrap_or(0),
            from_report: reported.is_some(),
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
            (CRASH_UUID_KEY, Some(self.uuid.as_str())),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key, value)))
    }
}

/// A request ready for the agent.
#[derive(Debug)]
pub(crate) struct Submission {
    endpoint: Endpoint,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
    /// Chunks in the body.
    trace_count: usize,
}

/// Takes the report's spans to close and prepares their requests. A block
/// that cannot be finished is noted in the report's log messages, so this runs
/// before the report is uploaded. A block without its own agent URL goes to
/// `crash_endpoint`, where the crash tracker sends the report.
pub(crate) fn prepare_all(
    crash_info: &mut CrashInfo,
    crash_endpoint: Option<&Endpoint>,
) -> Vec<Submission> {
    let blocks = std::mem::take(&mut crash_info.spans_to_close);
    if blocks.is_empty() {
        return vec![];
    }
    let crash = CrashDetails::from_report(crash_info);
    if !crash.from_report {
        crash_info.log_messages.push(format!(
            "Spans to close end at the receive time: no crash time in {:?}",
            crash_info.timestamp
        ));
    }
    let mut submissions = Vec::with_capacity(blocks.len());
    for (i, block) in blocks.iter().enumerate() {
        match prepare(block, &crash, crash_endpoint) {
            Ok(submission) => submissions.push(submission),
            Err(e) => crash_info
                .log_messages
                .push(format!("Spans to close #{i} not sent: {e:#}")),
        }
    }
    submissions
}

/// The block's request, with its spans finished.
fn prepare(
    block: &SpansToClose,
    crash: &CrashDetails,
    crash_endpoint: Option<&Endpoint>,
) -> anyhow::Result<Submission> {
    let endpoint = traces_endpoint(block.agent_url.as_deref(), crash_endpoint)?;
    let headers = request_headers(block)?;
    let mut payload = TracerPayloadSlice::from(block);
    for chunk in &mut payload.chunks {
        finish_chunk(chunk, crash);
    }
    Ok(Submission {
        endpoint,
        headers,
        body: encode_v1::to_vec_from_v1(&payload),
        trace_count: payload.chunks.len(),
    })
}

/// Ends every span of the chunk at the crash and keeps the chunk.
fn finish_chunk<'a>(chunk: &mut TraceChunk<SliceData<'a>>, crash: &'a CrashDetails) {
    if chunk.priority.is_none() {
        chunk.set_priority(USER_KEEP);
    }
    for span in chunk.spans_mut() {
        end_at_crash(span, crash);
    }
    compute_top_level_span(&mut chunk.spans);
}

fn end_at_crash<'a>(span: &mut Span<SliceData<'a>>, crash: &'a CrashDetails) {
    span.set_duration(crash.crash_unix_ns.saturating_sub(span.start).max(0));
    span.set_error(true);
    for (key, value) in crash.attributes() {
        span.set_attribute(
            Cow::Borrowed(key),
            AttributeValue::String(Cow::Borrowed(value)),
        );
    }
}

/// The block as a v1 payload, borrowing its strings.
impl<'a> From<&'a SpansToClose> for TracerPayloadSlice<'a> {
    fn from(block: &'a SpansToClose) -> Self {
        let p = &block.process;
        TracerPayload {
            container_id: text(&p.container_id),
            language_name: text(&p.language),
            language_version: text(&p.language_version),
            tracer_version: text(&p.tracer_version),
            runtime_id: text(&p.runtime_id),
            env: text(&p.env),
            hostname: text(&p.hostname),
            app_version: text(&p.app_version),
            attributes: attributes(&p.attributes),
            chunks: block.traces.iter().map(Into::into).collect(),
        }
    }
}

impl<'a> From<&'a TraceToClose> for TraceChunk<SliceData<'a>> {
    fn from(trace: &'a TraceToClose) -> Self {
        TraceChunk {
            trace_id: trace.trace_id.to_be_bytes(),
            priority: trace.priority,
            origin: text(&trace.origin),
            sampling_mechanism: trace.sampling_mechanism,
            dropped_trace: false,
            attributes: attributes(&trace.attributes),
            spans: trace.spans.iter().map(Into::into).collect(),
        }
    }
}

impl<'a> From<&'a SpanToClose> for Span<SliceData<'a>> {
    fn from(span: &'a SpanToClose) -> Self {
        Span {
            service: text(&span.service),
            name: text(&span.name),
            resource: text(span.resource.as_deref().unwrap_or(&span.name)),
            r#type: text(&span.r#type),
            span_id: span.span_id,
            parent_id: span.parent_id,
            start: span.start_unix_ns,
            span_kind: span
                .kind
                .as_deref()
                .map_or(SpanKind::Internal, SpanKind::from_meta),
            env: text(&span.env),
            version: text(&span.version),
            component: text(&span.component),
            attributes: attributes(&span.attributes),
            ..Default::default()
        }
    }
}

fn text(s: &str) -> Cow<'_, str> {
    Cow::Borrowed(s)
}

/// Attributes as v1 holds them.
fn attributes(attributes: &Attributes) -> VecMap<Cow<'_, str>, AttributeValue<SliceData<'_>>> {
    let mut map = VecMap::with_capacity(attributes.len());
    for (key, value) in attributes {
        let value = match value {
            ScalarValue::String(s) => AttributeValue::String(text(s)),
            ScalarValue::Bool(b) => AttributeValue::Bool(*b),
            ScalarValue::Int(i) => AttributeValue::Int(*i),
            ScalarValue::Float(f) => AttributeValue::Float(*f),
        };
        map.insert(text(key), value);
    }
    map
}

/// The headers the agent reads off a trace request, from what the block says
/// about its process. The body's own headers are added when it is sent.
fn request_headers(block: &SpansToClose) -> anyhow::Result<Vec<(HeaderName, HeaderValue)>> {
    let p = &block.process;
    let mut headers = vec![(
        HeaderName::from_static("datadog-client-computed-top-level"),
        HeaderValue::from_static("yes"),
    )];
    for (name, value) in [
        ("datadog-meta-lang", &p.language),
        ("datadog-meta-lang-version", &p.language_version),
        ("datadog-meta-tracer-version", &p.tracer_version),
        ("datadog-container-id", &p.container_id),
    ] {
        if !value.is_empty() {
            headers.push((
                HeaderName::from_static(name),
                HeaderValue::try_from(value.as_str())
                    .map_err(|e| anyhow::anyhow!("{name}: {e}"))?,
            ));
        }
    }
    Ok(headers)
}

/// Where the block's spans go: its own agent URL, else the agent the crash
/// tracker reports to, keeping that endpoint's settings (timeout, test token,
/// resolver). An endpoint with an API key is the errors intake, and a file
/// endpoint writes the report locally; neither takes traces.
fn traces_endpoint(
    agent_url: Option<&str>,
    crash_endpoint: Option<&Endpoint>,
) -> anyhow::Result<Endpoint> {
    if let Some(agent_url) = agent_url {
        return Ok(Endpoint::from_url(traces_uri(parse_uri(agent_url)?)?));
    }
    let Some(crash_endpoint) = crash_endpoint else {
        anyhow::bail!("no agent URL, and the crash tracker has no endpoint");
    };
    anyhow::ensure!(
        crash_endpoint.api_key.is_none(),
        "no agent URL, and the crash tracker sends to the intake, not an agent"
    );
    anyhow::ensure!(
        crash_endpoint.url.scheme_str() != Some("file"),
        "no agent URL, and the crash tracker writes to a file, not an agent"
    );
    Ok(Endpoint {
        url: traces_uri(crash_endpoint.url.clone())?,
        ..crash_endpoint.clone()
    })
}

/// The agent's v1 traces URL at `agent`'s scheme and authority, for `http://`
/// and `unix://` agents alike.
fn traces_uri(agent: http::Uri) -> anyhow::Result<http::Uri> {
    let mut parts = agent.into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(V1_TRACES_PATH));
    Ok(http::Uri::from_parts(parts)?)
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
            Ok(Err(e)) => eprintln!("crashtracker: failed to submit spans to close: {e:#}"),
            Err(e) => eprintln!("crashtracker: spans to close submission panicked: {e}"),
        }
    }
}

/// POSTs one submission and returns the agent's status. Fails on a connection
/// error, timeout, or non-2xx status.
async fn submit(submission: Submission) -> anyhow::Result<u16> {
    let endpoint = submission.endpoint;
    let user_agent = format!("libdatadog-crashtracker/{}", env!("CARGO_PKG_VERSION"));
    let mut builder = endpoint
        .to_request_builder(&user_agent)?
        .method(http::Method::POST);
    let headers = builder
        .headers_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid request to {}", endpoint.url))?;
    headers.extend(submission.headers);
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
    use libdd_trace_utils::msgpack_decoder::v1 as decode_v1;

    const START: i64 = 1_700_000_000_000_000_000;

    const BLOCK: &str = r#"{"version":1,"agent_url":"http://agent:8126",
        "process":{"language":"python","language_version":"3.12","tracer_version":"v0.33.0",
          "runtime_id":"809dad96","hostname":"gpu-host","env":"dev",
          "attributes":{"_dd.tags.process":"entrypoint.name:train"}},
        "traces":[{"trace_id":"000000000000000000000000000000ff","origin":"ray",
          "attributes":{"_dd.p.dm":"-4"},
          "spans":[
            {"span_id":"0000000000000007","service":"trainer","name":"training.rank",
             "type":"custom","start_unix_ns":1700000000000000000,"kind":"internal","env":"dev",
             "attributes":{"rank":1,"ml.batch_size":16,"ml.dataset":"synthetic-tokens",
               "loss":0.25,"done":false,"training.status":"failed",
               "error.message":"Process crashed during training"}},
            {"span_id":"0000000000000008","parent_id":"0000000000000007","service":"trainer",
             "name":"torch.step","resource":"step 12","start_unix_ns":1700000040000000000,
             "kind":"client"}]}]}"#;

    fn block() -> SpansToClose {
        SpansToClose::parse(BLOCK).unwrap()
    }

    fn crash(at: i64, message: Option<&str>) -> CrashDetails {
        CrashDetails {
            crash_unix_ns: at,
            from_report: true,
            error_type: "SIGSEGV".to_string(),
            error_message: message.map(str::to_string),
            uuid: "abc".to_string(),
        }
    }

    fn attr<'s, 'a>(
        span: &'s Span<SliceData<'a>>,
        key: &str,
    ) -> Option<&'s AttributeValue<SliceData<'a>>> {
        span.attributes.get(key)
    }

    fn string_attr<'s>(span: &'s Span<SliceData<'_>>, key: &str) -> Option<&'s str> {
        attr(span, key).and_then(|v| v.as_str())
    }

    #[test]
    fn builds_finishes_and_encodes_the_described_spans() {
        let crash = crash(
            START + 42_000_000_000,
            Some("Crash during training (SIGSEGV)"),
        );
        let submission = prepare(&block(), &crash, None).unwrap();
        assert_eq!(submission.trace_count, 1);
        assert_eq!(
            submission.endpoint.url.to_string(),
            "http://agent:8126/v1.0/traces"
        );

        let (payload, used) = decode_v1::from_slice(&submission.body).unwrap();
        assert_eq!(used, submission.body.len());
        assert_eq!(payload.language_name, "python");
        assert_eq!(payload.language_version, "3.12");
        assert_eq!(payload.tracer_version, "v0.33.0");
        assert_eq!(payload.runtime_id, "809dad96");
        assert_eq!(payload.hostname, "gpu-host");
        assert_eq!(payload.env, "dev");
        assert_eq!(
            payload
                .attributes
                .get("_dd.tags.process")
                .and_then(|v| v.as_str()),
            Some("entrypoint.name:train")
        );

        let chunk = &payload.chunks[0];
        assert_eq!(chunk.trace_id, 0xffu128.to_be_bytes());
        assert_eq!(chunk.priority, Some(USER_KEEP));
        assert_eq!(chunk.origin, "ray");
        assert_eq!(
            chunk.attributes.get("_dd.p.dm").and_then(|v| v.as_str()),
            Some("-4")
        );

        let [rank, step] = &chunk.spans[..] else {
            panic!("expected two spans");
        };
        assert_eq!(rank.name, "training.rank");
        assert_eq!(rank.resource, "training.rank");
        assert_eq!(rank.r#type, "custom");
        assert_eq!(rank.span_kind, SpanKind::Internal);
        assert_eq!(rank.env, "dev");
        assert_eq!(rank.start, START);
        assert_eq!(rank.duration, 42_000_000_000);
        assert!(rank.error);
        assert_eq!(string_attr(rank, "error.type"), Some("SIGSEGV"));
        assert_eq!(
            string_attr(rank, "error.message"),
            Some("Crash during training (SIGSEGV)")
        );
        assert_eq!(string_attr(rank, IS_CRASH_KEY), Some("true"));
        assert_eq!(string_attr(rank, CRASH_UUID_KEY), Some("abc"));
        assert_eq!(string_attr(rank, "training.status"), Some("failed"));
        assert_eq!(string_attr(rank, "ml.dataset"), Some("synthetic-tokens"));
        assert!(matches!(attr(rank, "rank"), Some(AttributeValue::Int(1))));
        assert!(matches!(
            attr(rank, "ml.batch_size"),
            Some(AttributeValue::Int(16))
        ));
        assert!(matches!(attr(rank, "loss"), Some(AttributeValue::Float(f)) if *f == 0.25));
        assert!(matches!(
            attr(rank, "done"),
            Some(AttributeValue::Bool(false))
        ));
        // A root span is top-level; a child of the same service is not.
        assert!(matches!(attr(rank, "_top_level"), Some(AttributeValue::Float(f)) if *f == 1.0));

        assert_eq!(step.parent_id, 7);
        assert_eq!(step.resource, "step 12");
        assert_eq!(step.span_kind, SpanKind::Client);
        assert_eq!(step.duration, 2_000_000_000);
        assert!(step.error);
        assert!(attr(step, "_top_level").is_none());
    }

    #[test]
    fn keeps_the_staged_message_and_priority_and_never_ends_before_the_start() {
        let mut block = block();
        block.traces[0].priority = Some(0);
        let submission = prepare(&block, &crash(START - 1, None), None).unwrap();
        let (payload, _) = decode_v1::from_slice(&submission.body).unwrap();
        let chunk = &payload.chunks[0];
        assert_eq!(chunk.priority, Some(0));
        let rank = &chunk.spans[0];
        assert_eq!(rank.duration, 0);
        assert_eq!(
            string_attr(rank, "error.message"),
            Some("Process crashed during training")
        );
    }

    #[test]
    fn request_headers_come_from_the_process() {
        let headers = request_headers(&block()).unwrap();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.to_str().unwrap())
        };
        assert_eq!(get("datadog-meta-lang"), Some("python"));
        assert_eq!(get("datadog-meta-lang-version"), Some("3.12"));
        assert_eq!(get("datadog-meta-tracer-version"), Some("v0.33.0"));
        assert_eq!(get("datadog-client-computed-top-level"), Some("yes"));
        assert_eq!(get("datadog-container-id"), None);

        let mut bad = block();
        bad.process.language = "py\nthon".to_string();
        assert!(prepare(&bad, &crash(START, None), None).is_err());
    }

    #[cfg_attr(miri, ignore)] // CrashInfo::test_instance spawns a process
    #[test]
    fn crash_details_come_from_the_report() {
        let mut crash_info = CrashInfo::test_instance(1);
        let at = DateTime::from_timestamp_nanos(1_762_800_000_123_456_789);
        crash_info.timestamp = at.to_string();
        let crash = CrashDetails::from_report(&crash_info);
        assert_eq!(crash.crash_unix_ns, 1_762_800_000_123_456_789);
        assert!(crash.from_report);

        let intake = ErrorsIntakePayload::from_crash_info(&crash_info)
            .unwrap()
            .error;
        assert_eq!(Some(crash.error_type), intake.error_type);
        assert_eq!(crash.error_message, intake.message);
        assert_eq!(crash.uuid, crash_info.uuid);
    }

    #[cfg_attr(miri, ignore)] // CrashInfo::test_instance spawns a process
    #[test]
    fn unreadable_timestamp_ends_spans_at_the_receive_time() {
        let mut crash_info = CrashInfo::test_instance(1);
        crash_info.timestamp = "yesterday".to_string();
        crash_info.spans_to_close = vec![block()];
        let before = Utc::now().timestamp_nanos_opt().unwrap();
        let submissions = prepare_all(&mut crash_info, None);
        assert_eq!(submissions.len(), 1);
        assert!(crash_info.spans_to_close.is_empty());
        assert!(
            crash_info
                .log_messages
                .iter()
                .any(|m| m.contains("receive time"))
        );
        let (payload, _) = decode_v1::from_slice(&submissions[0].body).unwrap();
        assert!(payload.chunks[0].spans[0].duration >= before - START);
    }

    #[cfg_attr(miri, ignore)] // CrashInfo::test_instance spawns a process
    #[test]
    fn blocks_that_cannot_be_finished_are_noted_not_sent() {
        let mut crash_info = CrashInfo::test_instance(1);
        let mut bad = block();
        bad.agent_url = Some("not a url".to_string());
        crash_info.spans_to_close = vec![bad, block()];
        assert_eq!(prepare_all(&mut crash_info, None).len(), 1);
        assert!(
            crash_info
                .log_messages
                .iter()
                .any(|m| m.contains("#0 not sent")),
            "{:?}",
            crash_info.log_messages
        );
    }

    fn url_of(agent_url: Option<&str>, crash_endpoint: Option<&Endpoint>) -> String {
        traces_endpoint(agent_url, crash_endpoint)
            .unwrap()
            .url
            .to_string()
    }

    #[test]
    fn traces_go_to_the_blocks_agent_for_http_and_unix_agents() {
        let crash = Endpoint::from_slice("http://other:8126");
        assert_eq!(
            url_of(Some("http://agent:8126"), Some(&crash)),
            "http://agent:8126/v1.0/traces"
        );
        let based = traces_endpoint(Some("http://agent:8126/some/base"), None).unwrap();
        assert_eq!(based.url.path(), V1_TRACES_PATH);
        let unix = traces_endpoint(Some("unix:///var/run/datadog/apm.socket"), None).unwrap();
        assert_eq!(unix.url.scheme_str(), Some("unix"));
        assert_eq!(unix.url.path(), V1_TRACES_PATH);
    }

    #[test]
    fn traces_without_an_agent_url_go_to_the_crash_trackers_agent() {
        let mut crash =
            Endpoint::from_slice("http://agent:8126/telemetry/proxy/api/v2/apmtelemetry");
        crash.timeout_ms = 1234;
        crash.test_token = Some("token".into());
        let endpoint = traces_endpoint(None, Some(&crash)).unwrap();
        assert_eq!(endpoint.url.to_string(), "http://agent:8126/v1.0/traces");
        assert_eq!(endpoint.timeout_ms, 1234);
        assert_eq!(endpoint.test_token.as_deref(), Some("token"));
        let unix = Endpoint::from_slice("unix:///var/run/datadog/apm.socket");
        assert_eq!(
            url_of(None, Some(&unix)),
            traces_uri(unix.url.clone()).unwrap().to_string()
        );
    }

    #[test]
    fn traces_without_an_agent_url_need_an_agent_endpoint() {
        let mut intake = Endpoint::from_slice("https://intake.datadoghq.com");
        intake.api_key = Some("key".into());
        let file = Endpoint::from_slice("file:///tmp/crash.json");
        for (crash_endpoint, why) in [
            (None, "no endpoint"),
            (Some(&intake), "intake"),
            (Some(&file), "file"),
        ] {
            let err = traces_endpoint(None, crash_endpoint)
                .unwrap_err()
                .to_string();
            assert!(err.contains(why), "{err}");
        }
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

    fn submission_to(url: &str) -> Submission {
        let mut block = block();
        block.agent_url = Some(url.to_string());
        prepare(&block, &crash(START + 1, None), None).unwrap()
    }

    #[cfg_attr(miri, ignore)] // binds a socket
    #[tokio::test]
    async fn submit_posts_v1_with_the_receivers_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_once(listener, "200 OK"));
        assert_eq!(submit(submission_to(&url)).await.unwrap(), 200);

        let seen = server.await.unwrap().to_ascii_lowercase();
        assert!(seen.starts_with("post /v1.0/traces http/1.1"), "{seen}");
        assert!(seen.contains("datadog-meta-lang: python\r\n"));
        assert!(seen.contains("datadog-meta-tracer-version: v0.33.0\r\n"));
        assert!(seen.contains("datadog-client-computed-top-level: yes\r\n"));
        assert!(seen.contains("content-type: application/msgpack\r\n"));
        assert!(seen.contains("x-datadog-trace-count: 1\r\n"));
    }

    #[cfg_attr(miri, ignore)] // binds a socket
    #[tokio::test]
    async fn submit_fails_on_rejection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_once(listener, "415 Unsupported Media Type"));
        let err = submit(submission_to(&url)).await.unwrap_err();
        assert!(err.to_string().contains("415"), "{err}");
        server.await.unwrap();
    }
}
