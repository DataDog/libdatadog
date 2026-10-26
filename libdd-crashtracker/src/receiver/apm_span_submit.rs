// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Submits a finished APM span to the local Datadog agent when the crashtracker
//! receiver parses a `DD_CRASHTRACK_BEGIN_APM_SPAN` block. This lets a
//! long-running span that was active at crash time appear as a finished span in
//! the Datadog UI.

use crate::crash_info::ApmSpanSnapshot;

use libdd_tinybytes::BytesString;
use libdd_trace_utils::msgpack_encoder::v04::to_vec_from_v04;
use libdd_trace_utils::span::v04::SpanBytes;

/// Parse a decimal-encoded 128-bit trace ID string.
fn parse_trace_id(s: &str) -> anyhow::Result<u128> {
    Ok(s.parse()?)
}

/// Resolve the agent URL from the environment or fall back to the default.
fn agent_url() -> String {
    if let Ok(url) = std::env::var("DD_TRACE_AGENT_URL") {
        return url;
    }
    if let Ok(url) = std::env::var("DD_AGENT_URL") {
        return url;
    }
    let host = std::env::var("DD_AGENT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("DD_TRACE_AGENT_PORT").unwrap_or_else(|_| "8126".to_string());
    format!("http://{}:{}", host, port)
}

/// Build a `SpanBytes` from the crash span snapshot.
fn build_span(snapshot: &ApmSpanSnapshot) -> anyhow::Result<SpanBytes> {
    let trace_id = parse_trace_id(&snapshot.trace_id)?;
    let span_id: u64 = snapshot.span_id.parse()?;
    let parent_id: u64 = snapshot.parent_id.parse()?;
    let start: i64 = snapshot.start_unix_ns.parse()?;
    let duration: i64 = snapshot.duration_ns.parse()?;

    let mut meta: Vec<(BytesString, BytesString)> = vec![
        ("error.type".into(), "crash".into()),
        (
            "error.message".into(),
            "Process crashed during this span".into(),
        ),
    ];

    let upper_bits = (trace_id >> 64) as u64;
    if upper_bits != 0 {
        meta.push(("_dd.p.tid".into(), format!("{upper_bits:016x}").into()));
    }

    let mut metrics: Vec<(BytesString, f64)> = vec![
        // Mark this as a long-running span terminated by a crash. The agent
        // uses this to replace any partial snapshots already received.
        ("_dd.was_long_running".into(), 1.0),
    ];
    if snapshot.has_sampling_priority {
        metrics.push((
            "_sampling_priority_v1".into(),
            f64::from(snapshot.sampling_priority),
        ));
    }

    Ok(SpanBytes {
        name: snapshot.name.clone().into(),
        service: snapshot.service.clone().into(),
        resource: snapshot.resource.clone().into(),
        r#type: snapshot.span_type.clone().into(),
        trace_id,
        span_id,
        parent_id,
        start,
        duration,
        error: snapshot.error,
        meta: meta.into_iter().collect(),
        metrics: metrics.into_iter().collect(),
        ..Default::default()
    })
}

/// Encode a single span as a v0.4 msgpack payload: `[[span]]`.
fn encode_trace_payload(span: SpanBytes) -> Vec<u8> {
    to_vec_from_v04(&[vec![span]])
}

/// Send a finished span to the local Datadog agent for a crash span snapshot.
pub(crate) async fn submit_crash_span(snapshot: &ApmSpanSnapshot) -> anyhow::Result<()> {
    let span = build_span(snapshot)?;
    let url = agent_url();

    eprintln!(
        "crashtracker: submitting finished span \
         (trace_id={}, span_id={}) to {}",
        snapshot.trace_id, snapshot.span_id, url
    );

    let payload = encode_trace_payload(span);
    let uri: http::Uri = format!("{}/v0.4/traces", url.trim_end_matches('/')).parse()?;

    let req = http::Request::builder()
        .method(http::Method::PUT)
        .uri(uri)
        .header(http::header::CONTENT_TYPE, "application/msgpack")
        .header("X-Datadog-Trace-Count", "1")
        .body(payload.into())?;

    let client = libdd_common::http_common::new_client_periodic();
    let result = client.request(req).await;

    match &result {
        Ok(resp) => eprintln!(
            "crashtracker: span submitted successfully (status={})",
            resp.status()
        ),
        Err(e) => eprintln!("crashtracker: failed to submit span to agent: {e}"),
    }

    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_snapshot() -> ApmSpanSnapshot {
        ApmSpanSnapshot {
            schema_version: 2,
            trace_id: "12345".to_string(),
            span_id: "67890".to_string(),
            parent_id: "11111".to_string(),
            start_unix_ns: "1700000000000000000".to_string(),
            duration_ns: "500000000".to_string(),
            error: 1,
            has_sampling_priority: true,
            sampling_priority: 2,
            service: "my-service".to_string(),
            name: "http.request".to_string(),
            resource: "GET /foo".to_string(),
            span_type: "web".to_string(),
        }
    }

    #[test]
    fn build_span_basic_fields() {
        let snapshot = test_snapshot();
        let span = build_span(&snapshot).unwrap();
        assert_eq!(span.trace_id, 12345u128);
        assert_eq!(span.span_id, 67890);
        assert_eq!(span.parent_id, 11111);
        assert_eq!(span.start, 1700000000000000000i64);
        assert_eq!(span.duration, 500000000i64);
        assert_eq!(span.error, 1);
        assert_eq!(span.name.as_ref(), "http.request");
        assert_eq!(span.service.as_ref(), "my-service");
        assert_eq!(span.resource.as_ref(), "GET /foo");
        assert_eq!(span.r#type.as_ref(), "web");
    }

    #[test]
    fn build_span_128bit_trace_id_injects_p_tid() {
        let hi: u64 = 0xdeadbeefcafebabe;
        let expected = (u128::from(hi) << 64) | u128::from(12345u64);
        let snapshot = ApmSpanSnapshot {
            trace_id: expected.to_string(),
            ..test_snapshot()
        };
        let span = build_span(&snapshot).unwrap();
        assert_eq!(span.trace_id, expected);
        let p_tid = span
            .meta
            .iter()
            .find(|(k, _)| k.as_ref() == "_dd.p.tid")
            .map(|(_, v)| v.as_ref().to_string());
        assert_eq!(p_tid.as_deref(), Some("deadbeefcafebabe"));
    }

    #[test]
    fn build_span_64bit_trace_id_omits_p_tid() {
        let snapshot = test_snapshot();
        let span = build_span(&snapshot).unwrap();
        let p_tid = span
            .meta
            .iter()
            .find(|(k, _)| k.as_ref() == "_dd.p.tid");
        assert!(p_tid.is_none());
    }

    #[test]
    fn build_span_metrics_include_long_running_marker() {
        let snapshot = test_snapshot();
        let span = build_span(&snapshot).unwrap();
        let was_lr = span
            .metrics
            .iter()
            .find(|(k, _)| k.as_ref() == "_dd.was_long_running")
            .map(|(_, v)| *v);
        assert_eq!(was_lr, Some(1.0));
    }

    #[test]
    fn build_span_sampling_priority() {
        let snapshot = test_snapshot();
        let span = build_span(&snapshot).unwrap();
        let priority = span
            .metrics
            .iter()
            .find(|(k, _)| k.as_ref() == "_sampling_priority_v1")
            .map(|(_, v)| *v);
        assert_eq!(priority, Some(2.0));
    }

    #[test]
    fn build_span_no_sampling_priority_when_absent() {
        let mut snapshot = test_snapshot();
        snapshot.has_sampling_priority = false;
        let span = build_span(&snapshot).unwrap();
        let priority = span
            .metrics
            .iter()
            .find(|(k, _)| k.as_ref() == "_sampling_priority_v1");
        assert!(priority.is_none());
    }

    #[test]
    fn parse_trace_id_64bit() {
        assert_eq!(parse_trace_id("12345").unwrap(), 12345u128);
    }

    #[test]
    fn parse_trace_id_128bit() {
        let expected = (u128::from(1u64) << 64) | u128::from(42u64);
        assert_eq!(parse_trace_id(&expected.to_string()).unwrap(), expected);
    }

    #[test]
    fn encode_trace_payload_produces_non_empty_msgpack() {
        let snapshot = test_snapshot();
        let span = build_span(&snapshot).unwrap();
        let payload = encode_trace_payload(span);
        assert!(!payload.is_empty());
    }
}
