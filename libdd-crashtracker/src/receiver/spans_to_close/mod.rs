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
//! `libdd_trace_model` mutation traits, so no SDK format is decoded here. They
//! are sent like any tracer's traces, through `libdd_trace_utils`' `SendData`.
//!
//! The SDK is trusted to describe complete, correct spans. A block the
//! receiver cannot finish is not sent, and the report says why.

use crate::crash_info::{
    Attributes, CrashInfo, ScalarValue, SpanToClose, SpansToClose, TraceToClose,
};
use chrono::{DateTime, Utc};
use http::uri::PathAndQuery;
use libdd_capabilities::HttpClientCapability;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::{Endpoint, parse_uri};
use libdd_trace_model::{AttributesMut, SpanMut, TraceChunkMut, ValueTypes};
use libdd_trace_utils::msgpack_encoder::v1 as encode_v1;
use libdd_trace_utils::send_data::{SendData, SendDataBuilder};
use libdd_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
use libdd_trace_utils::span::BytesData;
use libdd_trace_utils::span::v1::{
    AttributeValue, Span, SpanKind, TraceChunk, TracerPayload, TracerPayloadBytes,
};
use libdd_trace_utils::tracer_header_tags::{TracerGenericTags, TracerHeaderTags};
use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
use std::sync::Arc;
use std::time::Duration;

const SUBMIT_TIMEOUT: Duration = Duration::from_secs(3);
/// One attempt: a refusal (an agent without v1 included) will not change on a
/// retry, and the submission has [`SUBMIT_TIMEOUT`] in all.
const NO_RETRIES: u32 = 0;
/// Bounds the resources used when a report contains many 64 KiB blocks.
const MAX_CONCURRENT_SUBMISSIONS: usize = 4;
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

/// A block's finished spans, ready for the agent's v1 traces endpoint.
pub(crate) struct Submission {
    send: SendData,
}

type Text = <BytesData as ValueTypes>::Text;
type AttributeMap = <BytesData as ValueTypes>::Map;

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
    let mut payload = TracerPayloadBytes::from(block);
    for chunk in &mut payload.chunks {
        finish_chunk(chunk, crash);
    }
    let size = encode_v1::to_encoded_byte_len_from_v1(&payload) as usize;
    let send = SendDataBuilder::new(
        size,
        TracerPayloadCollection::V1(Box::new(payload)),
        header_tags(block),
        &endpoint,
    )
    .with_retry_strategy(RetryStrategy::new(
        NO_RETRIES,
        0,
        RetryBackoffType::Constant,
        None,
    ))
    .build();
    Ok(Submission { send })
}

/// What the agent reads off a trace request's headers, from what the block
/// says about its process.
fn header_tags(block: &SpansToClose) -> TracerHeaderTags<'_> {
    let p = &block.process;
    TracerHeaderTags {
        lang: &p.language,
        lang_version: &p.language_version,
        tracer_version: &p.tracer_version,
        container_id: &p.container_id,
        generic: TracerGenericTags {
            // The SDK decides which spans are top-level. This keeps the agent
            // from recomputing that on a chunk of only the spans the crash
            // ended, which would mark a span top-level whenever its parent is
            // absent from the chunk.
            client_computed_top_level: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Ends every span of the chunk at the crash and keeps the chunk.
fn finish_chunk(chunk: &mut TraceChunk<BytesData>, crash: &CrashDetails) {
    if chunk.priority.is_none() {
        chunk.set_priority(USER_KEEP);
    }
    for span in chunk.spans_mut() {
        end_at_crash(span, crash);
    }
}

fn end_at_crash(span: &mut Span<BytesData>, crash: &CrashDetails) {
    span.set_duration(crash.crash_unix_ns.saturating_sub(span.start).max(0));
    span.set_error(true);
    for (key, value) in crash.attributes() {
        span.set_attribute(key.into(), AttributeValue::String(text(value)));
    }
}

/// The block as a v1 payload.
impl From<&SpansToClose> for TracerPayloadBytes {
    fn from(block: &SpansToClose) -> Self {
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

/// The trace as a v1 chunk. Top-level markers are attributes like any other:
/// a value the SDK staged is sent, and the receiver writes none.
impl From<&TraceToClose> for TraceChunk<BytesData> {
    fn from(trace: &TraceToClose) -> Self {
        TraceChunk {
            trace_id: trace.trace_id.to_be_bytes(),
            priority: trace.priority,
            origin: text(&trace.origin),
            sampling_mechanism: trace.sampling_mechanism,
            dropped_trace: false,
            attributes: attributes(&trace.attributes),
            spans: trace.spans.iter().map(Span::from).collect(),
        }
    }
}

impl From<&SpanToClose> for Span<BytesData> {
    fn from(span: &SpanToClose) -> Self {
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

fn text(s: &str) -> Text {
    s.to_owned().into()
}

/// Attributes as v1 holds them.
fn attributes(attributes: &Attributes) -> AttributeMap {
    let mut map = AttributeMap::with_capacity(attributes.len());
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

/// Where the block's spans go: its own agent URL, else the agent the crash
/// tracker reports to, keeping that endpoint's settings (timeout, test token,
/// resolver). An endpoint with an API key is the errors intake, and a file
/// endpoint writes the report locally; neither takes traces.
fn traces_endpoint(
    agent_url: Option<&str>,
    crash_endpoint: Option<&Endpoint>,
) -> anyhow::Result<Endpoint> {
    if let Some(agent_url) = agent_url {
        return Ok(Endpoint::from_url(traces_uri(
            agent_uri(agent_url)?,
            V1_TRACES_PATH,
        )?));
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
    ensure_agent_scheme(&crash_endpoint.url)?;
    Ok(Endpoint {
        url: traces_uri(crash_endpoint.url.clone(), V1_TRACES_PATH)?,
        ..crash_endpoint.clone()
    })
}

/// The block's agent URL, refused here, while the report can still say why,
/// unless it is a scheme agents serve traces on.
fn agent_uri(agent_url: &str) -> anyhow::Result<http::Uri> {
    let uri = parse_uri(agent_url)?;
    ensure_agent_scheme(&uri)?;
    Ok(uri)
}

/// Agents take traces over HTTP or a Unix socket.
fn ensure_agent_scheme(uri: &http::Uri) -> anyhow::Result<()> {
    match uri.scheme_str() {
        Some("http" | "unix") => Ok(()),
        scheme => anyhow::bail!(
            "unsupported agent scheme {:?} in {uri}, expected http or unix",
            scheme.unwrap_or("")
        ),
    }
}

/// The traces URL at `path` on `agent`'s scheme and authority, for `http://`
/// and `unix://` agents alike.
fn traces_uri(agent: http::Uri, path: &'static str) -> anyhow::Result<http::Uri> {
    let mut parts = agent.into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(path));
    Ok(http::Uri::from_parts(parts)?)
}

/// Sends submissions with bounded concurrency and waits for them all. The
/// report is already out, so failures only go to stderr.
pub(crate) async fn submit_all(submissions: Vec<Submission>) {
    if submissions.is_empty() {
        return;
    }
    let capabilities = Arc::new(NativeCapabilities::new_client());
    let mut submissions = submissions.into_iter();
    let mut set = tokio::task::JoinSet::new();
    for submission in submissions.by_ref().take(MAX_CONCURRENT_SUBMISSIONS) {
        spawn_submission(&mut set, Arc::clone(&capabilities), submission);
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("crashtracker: failed to submit spans to close: {e:#}"),
            Err(e) => eprintln!("crashtracker: spans to close submission panicked: {e}"),
        }
        if let Some(submission) = submissions.next() {
            spawn_submission(&mut set, Arc::clone(&capabilities), submission);
        }
    }
}

fn spawn_submission(
    set: &mut tokio::task::JoinSet<anyhow::Result<u16>>,
    capabilities: Arc<NativeCapabilities>,
    submission: Submission,
) {
    set.spawn(async move { submit(&capabilities, submission).await });
}

/// Sends one submission and returns the agent's status. Fails on a
/// connection error, timeout, or non-2xx status. An agent without v1 is not
/// retried as v0.4.
async fn submit(capabilities: &NativeCapabilities, submission: Submission) -> anyhow::Result<u16> {
    let result = tokio::time::timeout(SUBMIT_TIMEOUT, submission.send.send(capabilities))
        .await
        .map_err(|_| anyhow::anyhow!("timed out after {SUBMIT_TIMEOUT:?}"))?;
    // An error status still leaves its response here; a request that got
    // none leaves only the error counters.
    let response = match result.last_result {
        Ok(response) => response,
        Err(e) if result.errors_timeout > 0 => return Err(e.context("request timed out")),
        Err(e) if result.errors_network > 0 => return Err(e.context("network error")),
        Err(e) => return Err(e),
    };
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
    use libdd_trace_utils::span::SliceData;

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

    /// The submission's payload as the agent receives it: v1 msgpack.
    fn body(submission: &Submission) -> Vec<u8> {
        let TracerPayloadCollection::V1(payload) = submission.send.get_payloads() else {
            panic!("expected a v1 payload");
        };
        encode_v1::to_vec_from_v1(payload)
    }

    #[test]
    fn builds_finishes_and_encodes_the_described_spans() {
        let crash = crash(
            START + 42_000_000_000,
            Some("Crash during training (SIGSEGV)"),
        );
        let submission = prepare(&block(), &crash, None).unwrap();
        assert_eq!(submission.send.get_payloads().size(), 1);
        assert_eq!(
            submission.send.get_target().url.to_string(),
            "http://agent:8126/v1.0/traces"
        );

        let body = body(&submission);
        let (payload, used) = decode_v1::from_slice(&body).unwrap();
        assert_eq!(used, body.len());
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
        let body = body(&submission);
        let (payload, _) = decode_v1::from_slice(&body).unwrap();
        let chunk = &payload.chunks[0];
        assert_eq!(chunk.priority, Some(0));
        let rank = &chunk.spans[0];
        assert_eq!(rank.duration, 0);
        assert_eq!(
            string_attr(rank, "error.message"),
            Some("Process crashed during training")
        );
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
        let body = body(&submissions[0]);
        let (payload, _) = decode_v1::from_slice(&body).unwrap();
        assert!(payload.chunks[0].spans[0].duration >= before - START);
    }

    #[cfg_attr(miri, ignore)] // CrashInfo::test_instance spawns a process
    #[test]
    fn blocks_that_cannot_be_finished_are_noted_not_sent() {
        let mut crash_info = CrashInfo::test_instance(1);
        let mut bad = block();
        bad.agent_url = Some("not a url".to_string());
        // An unsupported scheme is refused here too, not when sending.
        let mut unsupported = block();
        unsupported.agent_url = Some("ftp://agent:21".to_string());
        crash_info.spans_to_close = vec![bad, block(), unsupported];
        assert_eq!(prepare_all(&mut crash_info, None).len(), 1);
        let logs = &crash_info.log_messages;
        assert!(logs.iter().any(|m| m.contains("#0 not sent")), "{logs:?}");
        assert!(
            logs.iter()
                .any(|m| m.contains("#2 not sent") && m.contains("unsupported agent scheme")),
            "{logs:?}"
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
            traces_uri(unix.url.clone(), V1_TRACES_PATH)
                .unwrap()
                .to_string()
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
    async fn serve_once(
        listener: &tokio::net::TcpListener,
        status: &'static str,
    ) -> (String, Vec<u8>) {
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
        let head_end = req
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map_or(req.len(), |i| i + 4);
        let body = req.split_off(head_end);
        (String::from_utf8_lossy(&req).into_owned(), body)
    }

    fn submission_to(url: &str) -> Submission {
        let mut block = block();
        block.agent_url = Some(url.to_string());
        prepare(&block, &crash(START + 1, None), None).unwrap()
    }

    async fn submit_once(submission: Submission) -> anyhow::Result<u16> {
        submit(&NativeCapabilities::new_client(), submission).await
    }

    /// The request head a submission of `block` sends to a local agent.
    async fn head_sent(mut block: SpansToClose) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        block.agent_url = Some(format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { serve_once(&listener, "200 OK").await });
        let submission = prepare(&block, &crash(START + 1, None), None).unwrap();
        assert_eq!(submit_once(submission).await.unwrap(), 200);
        server.await.unwrap().0.to_ascii_lowercase()
    }

    #[cfg_attr(miri, ignore)] // binds a socket
    #[tokio::test]
    async fn submit_posts_v1_with_the_processs_headers() {
        let seen = head_sent(block()).await;
        assert!(seen.starts_with("post /v1.0/traces http/1.1"), "{seen}");
        assert!(seen.contains("datadog-meta-lang: python\r\n"), "{seen}");
        assert!(
            seen.contains("datadog-meta-lang-version: 3.12\r\n"),
            "{seen}"
        );
        assert!(
            seen.contains("datadog-meta-tracer-version: v0.33.0\r\n"),
            "{seen}"
        );
        assert!(
            seen.contains("datadog-client-computed-top-level: true\r\n"),
            "{seen}"
        );
        assert!(
            seen.contains("datadog-send-real-http-status: 1\r\n"),
            "{seen}"
        );
        assert!(
            seen.contains("content-type: application/msgpack\r\n"),
            "{seen}"
        );
        assert!(seen.contains("x-datadog-trace-count: 1\r\n"), "{seen}");
        assert!(!seen.contains("datadog-container-id"), "{seen}");
    }

    #[cfg_attr(miri, ignore)] // binds a socket
    #[tokio::test]
    async fn a_value_no_header_can_hold_is_left_out_not_fatal() {
        let mut block = block();
        block.process.language = "py\nthon".to_string();
        let seen = head_sent(block).await;
        assert!(!seen.contains("datadog-meta-lang:"), "{seen}");
        assert!(
            seen.contains("datadog-meta-tracer-version: v0.33.0\r\n"),
            "{seen}"
        );
    }

    #[cfg_attr(miri, ignore)] // binds a socket
    #[tokio::test]
    async fn submit_all_limits_concurrency() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let submissions = (0..=MAX_CONCURRENT_SUBMISSIONS)
            .map(|_| submission_to(&url))
            .collect();
        let submitter = tokio::spawn(submit_all(submissions));

        let mut sockets = Vec::new();
        for _ in 0..MAX_CONCURRENT_SUBMISSIONS {
            let accepted = tokio::time::timeout(Duration::from_secs(1), listener.accept())
                .await
                .expect("submission did not connect")
                .unwrap();
            sockets.push(accepted.0);
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "more than {MAX_CONCURRENT_SUBMISSIONS} submissions ran concurrently"
        );

        drop(sockets);
        let last = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("queued submission did not connect")
            .unwrap();
        drop(last);
        submitter.await.unwrap();
    }

    #[cfg_attr(miri, ignore)] // binds a socket
    #[tokio::test]
    async fn submit_fails_on_rejection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server =
            tokio::spawn(async move { serve_once(&listener, "415 Unsupported Media Type").await });
        let err = submit_once(submission_to(&url)).await.unwrap_err();
        assert!(err.to_string().contains("415"), "{err}");
        server.await.unwrap();
    }

    #[cfg_attr(miri, ignore)] // binds a socket
    #[tokio::test]
    async fn submit_fails_when_the_agent_has_no_v1() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { serve_once(&listener, "404 Not Found").await });
        let err = submit_once(submission_to(&url)).await.unwrap_err();
        assert!(err.to_string().contains("404"), "{err}");

        let (head, _) = server.await.unwrap();
        assert!(
            head.to_ascii_lowercase().starts_with("post /v1.0/traces "),
            "{head}"
        );
    }

    #[test]
    fn agent_urls_must_be_http_or_unix() {
        for url in [
            "file:///tmp/traces",
            "windows:\\\\.\\pipe\\agent",
            "ftp://agent:21",
            "https://agent:8126",
        ] {
            let err = traces_endpoint(Some(url), None).unwrap_err();
            assert!(
                err.to_string().contains("unsupported agent scheme"),
                "{url}: {err}"
            );
        }
        let https_agent = Endpoint::from_slice("https://agent:8126");
        let err = traces_endpoint(None, Some(&https_agent)).unwrap_err();
        assert!(
            err.to_string().contains("unsupported agent scheme"),
            "{err}"
        );
    }

    #[test]
    fn staged_top_level_markers_are_sent_as_is() {
        let child = |attributes: &str| {
            format!(
                r#"{{"version":1,"agent_url":"http://agent:8126","traces":[{{
                    "trace_id":"000000000000000000000000000000ff","spans":[
                    {{"span_id":"0000000000000002","parent_id":"0000000000000001",
                      "service":"web","name":"child","start_unix_ns":5,
                      "attributes":{attributes}}}]}}]}}"#
            )
        };
        let sent = |attributes: &str| {
            let block = SpansToClose::parse(&child(attributes)).unwrap();
            let submission = prepare(&block, &crash(START + 1, None), None).unwrap();
            let body = body(&submission);
            let (payload, _) = decode_v1::from_slice(&body).unwrap();
            let span = &payload.chunks[0].spans[0];
            ["_top_level", "_dd.top_level"].map(|key| match attr(span, key) {
                None => None,
                Some(AttributeValue::Int(i)) => Some(*i as f64),
                Some(AttributeValue::Float(f)) => Some(*f),
                Some(other) => panic!("unexpected {other:?}"),
            })
        };
        // Either key, any value: sent untouched, and nothing added.
        assert_eq!(sent(r#"{"_dd.top_level":1}"#), [None, Some(1.0)]);
        assert_eq!(sent(r#"{"_dd.top_level":0}"#), [None, Some(0.0)]);
        assert_eq!(sent(r#"{"_top_level":0}"#), [Some(0.0), None]);
        // No marker stays absent, including when the parent is not in the block.
        assert_eq!(sent("{}"), [None, None]);
    }
}
