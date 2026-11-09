// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Ends the spans of an [`ApmTracePayload`] at the crash and submits them to
//! the agent.
//!
//! The collector stages the payload before the crash, encoded exactly as the
//! tracer sends traces, and copies it out verbatim when the process crashes
//! (it can only do async-signal-safe work). Everything crash-specific is
//! applied here, by decoding the payload, editing its spans and encoding it
//! again: in the v1 format strings are interned by first use, so a payload
//! cannot be edited in place without renumbering every later string.
//!
//! Every span in the payload is one the crash ends. Each gets:
//! - `duration` ending at the crash, when the crash time is known and after the span's start;
//! - `error = true`, `error.type = "crash"`, and an `error.message` naming the signal and crash
//!   report;
//! - `dd.crash_tracking.uuid`, linking the span to its crash report.
//!
//! If the payload cannot be decoded it is sent as staged: the tracer already
//! marks its spans as failed crashes, only without the crash's details.

use crate::crash_info::ApmTracePayload;
use http::uri::PathAndQuery;
use http_body_util::BodyExt;
use libdd_common::{Endpoint, http_common, parse_uri};
use libdd_trace_utils::msgpack_decoder::v1 as decode_v1;
use libdd_trace_utils::msgpack_encoder::v1 as encode_v1;
use libdd_trace_utils::span::v1::{AttributeValue, SpanSlice};
use std::borrow::Cow;
use std::time::Duration;

const V1_TRACES_PATH: &str = "/v1.0/traces";
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) const ERROR_TYPE: &str = "crash";
pub(crate) const CRASH_UUID_KEY: &str = "dd.crash_tracking.uuid";

/// What the crash report knows about the crash, stamped into every span.
pub(crate) struct CrashDetails<'a> {
    /// Wall-clock crash time in nanoseconds since the Unix epoch.
    pub crash_unix_ns: Option<u64>,
    pub error_message: &'a str,
    pub uuid: &'a str,
}

/// A request body ready for the agent, with the metadata its headers carry.
#[derive(Debug)]
pub(crate) struct TraceRequest {
    pub body: Vec<u8>,
    /// Chunks in the payload; `None` when sent as staged.
    pub trace_count: Option<usize>,
    pub language: String,
    pub language_version: String,
    pub tracer_version: String,
}

impl TraceRequest {
    /// The staged payload, unchanged.
    pub fn as_staged(data: Vec<u8>) -> Self {
        Self {
            body: data,
            trace_count: None,
            language: String::new(),
            language_version: String::new(),
            tracer_version: String::new(),
        }
    }
}

/// Decodes a v1 payload, ends its spans at the crash and encodes it again.
pub(crate) fn finalize_v1(data: &[u8], crash: &CrashDetails<'_>) -> anyhow::Result<TraceRequest> {
    let (mut payload, consumed) =
        decode_v1::from_slice(data).map_err(|e| anyhow::anyhow!("v1 decode: {e:?}"))?;
    anyhow::ensure!(
        consumed == data.len(),
        "v1 decode: {} trailing bytes",
        data.len() - consumed
    );
    for chunk in &mut payload.chunks {
        for span in &mut chunk.spans {
            end_at_crash(span, crash);
        }
    }
    Ok(TraceRequest {
        body: encode_v1::to_vec_from_v1(&payload),
        trace_count: Some(payload.chunks.len()),
        language: payload.language_name.to_string(),
        language_version: payload.language_version.to_string(),
        tracer_version: payload.tracer_version.to_string(),
    })
}

fn end_at_crash<'a>(span: &mut SpanSlice<'a>, crash: &CrashDetails<'a>) {
    let crash_ns = crash.crash_unix_ns.and_then(|ns| i64::try_from(ns).ok());
    // Never shorten a span or zero it: a clock that moved back keeps the
    // duration the tracer staged.
    if let Some(end) = crash_ns.filter(|end| *end > span.start) {
        span.duration = span.duration.max(end - span.start);
    }
    span.error = true;
    for (key, value) in [
        ("error.type", ERROR_TYPE),
        ("error.message", crash.error_message),
        (CRASH_UUID_KEY, crash.uuid),
    ] {
        span.attributes.insert_or_replace(
            Cow::Borrowed(key),
            AttributeValue::String(Cow::Borrowed(value)),
        );
    }
}

/// The crash payload's request: finalized when possible, else as staged.
/// The second value explains why it was sent as staged.
pub(crate) fn prepare(
    payload: &ApmTracePayload,
    crash: &CrashDetails<'_>,
) -> (TraceRequest, Option<String>) {
    match finalize_v1(&payload.data, crash) {
        Ok(request) => (request, None),
        Err(e) => (
            TraceRequest::as_staged(payload.data.clone()),
            Some(format!("{e:#}")),
        ),
    }
}

/// The agent URL from the collector, else from the environment the tracer
/// read it from.
pub(crate) fn agent_url(payload: &ApmTracePayload) -> String {
    if let Some(url) = &payload.agent_url {
        return url.clone();
    }
    let configured = ["DD_TRACE_AGENT_URL", "DD_AGENT_URL"]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|url| !url.is_empty());
    if let Some(url) = configured {
        return url;
    }
    let host = std::env::var("DD_AGENT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("DD_TRACE_AGENT_PORT").unwrap_or_else(|_| "8126".to_string());
    format!("http://{host}:{port}")
}

/// `{agent}/v1.0/traces`, for `http://` and `unix://` agent URLs alike.
pub(crate) fn traces_uri(agent_url: &str) -> anyhow::Result<http::Uri> {
    let mut parts = parse_uri(agent_url)?.into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(V1_TRACES_PATH));
    Ok(http::Uri::from_parts(parts)?)
}

/// POSTs the request to the agent's `/v1.0/traces` and returns the response
/// status. Fails on a connection error, timeout, or non-2xx status.
pub(crate) async fn submit(agent_url: &str, request: TraceRequest) -> anyhow::Result<u16> {
    let endpoint = Endpoint {
        url: traces_uri(agent_url)?,
        test_token: std::env::var("DD_TRACE_AGENT_TEST_SESSION_TOKEN")
            .ok()
            .map(Into::into),
        ..Default::default()
    };
    let user_agent = format!("libdatadog-crashtracker/{}", env!("CARGO_PKG_VERSION"));
    let mut builder = endpoint
        .to_request_builder(&user_agent)?
        .method(http::Method::POST)
        .header(http::header::CONTENT_TYPE, "application/msgpack")
        .header("Datadog-Client-Computed-Top-Level", "yes");
    if let Some(count) = request.trace_count {
        builder = builder.header("X-Datadog-Trace-Count", count.to_string());
    }
    for (name, value) in [
        ("Datadog-Meta-Lang", &request.language),
        ("Datadog-Meta-Lang-Version", &request.language_version),
        ("Datadog-Meta-Tracer-Version", &request.tracer_version),
    ] {
        if !value.is_empty() {
            builder = builder.header(name, value.as_str());
        }
    }
    let req = builder.body(http_common::Body::from(request.body))?;

    let client = http_common::new_client_periodic();
    let response = tokio::time::timeout(SUBMIT_TIMEOUT, client.request(req))
        .await
        .map_err(|_| anyhow::anyhow!("timed out after {SUBMIT_TIMEOUT:?}"))??;
    let status = response.status();
    // Drain the body so the connection closes cleanly.
    let _ = tokio::time::timeout(SUBMIT_TIMEOUT, response.into_body().collect()).await;
    anyhow::ensure!(status.is_success(), "agent replied {status}");
    Ok(status.as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_utils::span::SliceData;
    use libdd_trace_utils::span::v1::{SpanKind, TraceChunk, TracerPayload};

    const START: i64 = 1_700_000_000_000_000_000;

    fn ns(offset: i64) -> u64 {
        u64::try_from(START + offset).unwrap()
    }

    fn span<'a>(name: &'a str, env: &'a str) -> SpanSlice<'a> {
        let mut s = SpanSlice {
            service: "trainer".into(),
            name: name.into(),
            resource: name.into(),
            r#type: "custom".into(),
            span_id: 7,
            start: START,
            duration: 5,
            span_kind: SpanKind::Internal,
            env: env.into(),
            ..Default::default()
        };
        s.attributes.insert(
            "training.status".into(),
            AttributeValue::String("failed".into()),
        );
        s.attributes.insert(
            "error.message".into(),
            AttributeValue::String("Process crashed during training".into()),
        );
        s.attributes
            .insert("_dd.was_long_running".into(), AttributeValue::Float(1.0));
        s
    }

    /// A payload whose strings are interned after the span's attributes too
    /// (span env, payload env and hostname), so an in-place edit would
    /// misnumber them.
    fn staged_payload() -> Vec<u8> {
        let payload: TracerPayload<SliceData<'_>> = TracerPayload {
            language_name: "c".into(),
            language_version: "c23".into(),
            tracer_version: "v0.33.0".into(),
            env: "dev".into(),
            hostname: "gpu-host".into(),
            chunks: vec![TraceChunk {
                trace_id: [3; 16],
                priority: Some(1),
                spans: vec![span("training.rank", "dev")],
                ..Default::default()
            }],
            ..Default::default()
        };
        encode_v1::to_vec_from_v1(&payload)
    }

    fn crash(at: Option<u64>) -> CrashDetails<'static> {
        CrashDetails {
            crash_unix_ns: at,
            error_message: "Process crashed (SIGSEGV). Crash report: abc",
            uuid: "abc",
        }
    }

    fn string_attr<'s>(span: &'s SpanSlice<'_>, key: &str) -> Option<&'s str> {
        match span.attributes.get(key) {
            Some(AttributeValue::String(s)) => Some(s.as_ref()),
            _ => None,
        }
    }

    #[test]
    fn ends_spans_at_the_crash_with_its_details() {
        let staged = staged_payload();
        let crash_at = ns(42_000_000_000);
        let request = finalize_v1(&staged, &crash(Some(crash_at))).unwrap();
        assert_eq!(request.trace_count, Some(1));
        assert_eq!(request.language, "c");
        assert_eq!(request.tracer_version, "v0.33.0");

        let (payload, used) = decode_v1::from_slice(&request.body).unwrap();
        assert_eq!(used, request.body.len());
        let span = &payload.chunks[0].spans[0];
        assert_eq!(span.duration, 42_000_000_000);
        assert!(span.error);
        assert_eq!(string_attr(span, "error.type"), Some("crash"));
        assert_eq!(
            string_attr(span, "error.message"),
            Some("Process crashed (SIGSEGV). Crash report: abc")
        );
        assert_eq!(string_attr(span, CRASH_UUID_KEY), Some("abc"));
        assert_eq!(string_attr(span, "training.status"), Some("failed"));
        // Strings interned after the new attributes keep their meaning.
        assert_eq!(span.env, "dev");
        assert_eq!(span.r#type, "custom");
        assert_eq!(payload.env, "dev");
        assert_eq!(payload.hostname, "gpu-host");
        assert_eq!(payload.chunks[0].priority, Some(1));
        assert_eq!(payload.chunks[0].trace_id, [3; 16]);
    }

    #[test]
    fn keeps_the_staged_duration_without_a_usable_crash_time() {
        let staged = staged_payload();
        for at in [None, Some(ns(-1)), Some(ns(1))] {
            let request = finalize_v1(&staged, &crash(at)).unwrap();
            let (payload, _) = decode_v1::from_slice(&request.body).unwrap();
            let span = &payload.chunks[0].spans[0];
            assert_eq!(span.duration, 5, "crash at {at:?}");
            assert!(span.error);
        }
    }

    #[test]
    fn undecodable_payload_is_sent_as_staged() {
        let payload = ApmTracePayload {
            format: ApmTracePayload::FORMAT_V1.to_string(),
            data: vec![0xc1, 0x00],
            ..Default::default()
        };
        let (request, why) = prepare(&payload, &crash(None));
        assert_eq!(request.body, payload.data);
        assert_eq!(request.trace_count, None);
        assert!(why.is_some());
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut staged = staged_payload();
        staged.push(0xc0);
        assert!(finalize_v1(&staged, &crash(None)).is_err());
    }

    #[test]
    fn traces_uri_handles_http_and_unix_agents() {
        let http = traces_uri("http://agent:8126").unwrap();
        assert_eq!(http.to_string(), "http://agent:8126/v1.0/traces");
        let http = traces_uri("http://agent:8126/some/base").unwrap();
        assert_eq!(http.path(), V1_TRACES_PATH);
        let unix = traces_uri("unix:///var/run/datadog/apm.socket").unwrap();
        assert_eq!(unix.scheme_str(), Some("unix"));
        assert_eq!(unix.path(), V1_TRACES_PATH);
    }

    #[test]
    fn agent_url_prefers_the_collectors() {
        let payload = ApmTracePayload {
            agent_url: Some("unix:///tmp/apm.socket".to_string()),
            ..Default::default()
        };
        assert_eq!(agent_url(&payload), "unix:///tmp/apm.socket");
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

    #[tokio::test]
    async fn submit_posts_v1_with_tracer_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_once(listener, "200 OK"));
        let request = finalize_v1(&staged_payload(), &crash(None)).unwrap();
        assert_eq!(submit(&url, request).await.unwrap(), 200);

        let seen = server.await.unwrap().to_ascii_lowercase();
        assert!(seen.starts_with("post /v1.0/traces http/1.1"), "{seen}");
        assert!(seen.contains("content-type: application/msgpack"));
        assert!(seen.contains("x-datadog-trace-count: 1"));
        assert!(seen.contains("datadog-meta-lang: c\r\n"));
        assert!(seen.contains("datadog-meta-tracer-version: v0.33.0"));
    }

    #[tokio::test]
    async fn submit_fails_on_rejection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_once(listener, "415 Unsupported Media Type"));
        let err = submit(&url, TraceRequest::as_staged(vec![0x80]))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("415"), "{err}");
        server.await.unwrap();
    }
}
