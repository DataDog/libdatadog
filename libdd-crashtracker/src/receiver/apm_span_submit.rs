// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Submits a pre-encoded v0.4 msgpack trace payload to the local Datadog agent
//! when the crashtracker receiver parses a `DD_CRASHTRACK_BEGIN_APM_TRACE_PAYLOAD`
//! block. The collector pre-encodes the complete span (with duration stamped at
//! crash time) as msgpack; the receiver hex-decodes and POSTs the raw bytes.

use crate::crash_info::ApmTracePayload;

/// Resolve the agent URL from the environment or fall back to the default.
pub(crate) fn agent_url() -> String {
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

/// Submit a pre-encoded v0.4 msgpack trace payload to the agent.
///
/// `tracer_version` comes from the crash report's metadata block
/// (`library_version`), which is the dd-trace-c version string the tracer
/// emitted before the crash.
pub(crate) async fn submit_trace_payload(
    payload: &ApmTracePayload,
    tracer_version: &str,
) -> anyhow::Result<()> {
    let url = &payload.agent_url;

    eprintln!(
        "crashtracker: submitting pre-encoded trace payload ({} bytes) to {}",
        payload.data.len(),
        url
    );

    let uri: http::Uri = format!("{}/v0.4/traces", url.trim_end_matches('/')).parse()?;

    let mut req_builder = http::Request::builder()
        .method(http::Method::PUT)
        .uri(uri)
        .header(http::header::CONTENT_TYPE, "application/msgpack")
        .header("X-Datadog-Trace-Count", "1")
        .header("Datadog-Meta-Lang", "c")
        .header("Datadog-Meta-Tracer-Version", tracer_version)
        .header("Datadog-Client-Computed-Top-Level", "yes");

    // Forward the test session token when running against ddapm-test-agent.
    if let Ok(token) = std::env::var("DD_TRACE_AGENT_TEST_SESSION_TOKEN") {
        req_builder = req_builder.header("X-Datadog-Test-Session-Token", token);
    }

    let req = req_builder.body(payload.data.clone().into())?;

    let client = libdd_common::http_common::new_client_periodic();
    let result = client.request(req).await;

    match &result {
        Ok(resp) => eprintln!(
            "crashtracker: trace payload submitted (status={})",
            resp.status()
        ),
        Err(e) => eprintln!("crashtracker: failed to submit trace payload: {e}"),
    }

    result?;
    Ok(())
}
