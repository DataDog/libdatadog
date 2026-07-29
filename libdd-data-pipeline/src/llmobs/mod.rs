// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! LLM Observability event extraction and export for APM spans.

use std::borrow::Borrow;
use std::str::FromStr;
use std::time::Duration;

use http::{HeaderMap, HeaderValue, Uri};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::{
    send_with_retry, RetryBackoffType, RetryStrategy, SendWithRetryError,
};
use libdd_trace_utils::span::{v04::Span, TraceData};
use serde_json::{json, Map, Value};
use tracing::{error, warn};

use crate::trace_exporter::error::{InternalErrorKind, RequestError, TraceExporterError};

const LLMOBS_META_STRUCT_KEY: &str = "_llmobs";
const EVP_PROXY_PATH: &str = "/evp_proxy/v2/api/v2/llmobs";
const EVP_SUBDOMAIN_HEADER: &str = "x-datadog-evp-subdomain";
const LLMOBS_INTAKE_SUBDOMAIN: &str = "llmobs-intake";
const LLMOBS_EVENT_TYPE: &str = "span";
const EVENT_SIZE_LIMIT: usize = 5 << 20;
const PAYLOAD_SIZE_LIMIT: usize = 5 << 20;
const DROPPED_IO_VALUE: &str =
    "[This value has been dropped because this span's size exceeds the 5MB size limit.]";
const MAX_RETRIES: u32 = 2;
const RETRY_DELAY_MS: u64 = 1_000;

/// LLM Observability transport configuration supplied by the host tracer.
#[derive(Clone, Debug)]
pub struct LlmObsConfig {
    /// Full direct-intake URL used when EVP proxy delivery fails.
    pub agentless_endpoint: String,
    /// API key for direct-intake authentication. An empty value disables direct fallback.
    pub api_key: String,
    /// Request timeout for both EVP proxy and direct intake.
    pub timeout: Duration,
}

/// Events separated according to the final APM sampling decision.
#[derive(Default)]
pub(crate) struct RoutedEvents {
    /// Events removed from APM spans because their trace has priority <= 0.
    pub standalone: Vec<Value>,
    /// Events retained on kept APM spans and available for request-failure rescue.
    pub rescue: Vec<Value>,
}

/// Extract LLMObs events and remove them only from traces the agent will reject.
pub(crate) fn route_events<T: TraceData>(traces: &mut Vec<Vec<Span<T>>>) -> RoutedEvents {
    let mut routed = RoutedEvents::default();

    for trace in traces.iter_mut() {
        let drop_apm = apm_disabled(trace);
        let sampled = !drop_apm && sampling_priority(trace).is_none_or(|priority| priority > 0.0);
        for span in trace {
            let Some(encoded) = span.meta_struct.get(LLMOBS_META_STRUCT_KEY) else {
                continue;
            };
            let event = match decode_event(span, encoded.borrow()) {
                Ok(event) => event,
                Err(error) => {
                    warn!(%error, "Failed to assemble LLMObs event from meta_struct");
                    span.meta_struct.remove_slow(LLMOBS_META_STRUCT_KEY);
                    continue;
                }
            };

            if sampled {
                routed.rescue.push(event);
            } else {
                routed.standalone.push(event);
                span.meta_struct.remove_slow(LLMOBS_META_STRUCT_KEY);
            }
        }
    }
    traces.retain(|trace| !apm_disabled(trace));

    routed
}

fn apm_disabled<T: TraceData>(trace: &[Span<T>]) -> bool {
    for span in trace {
        if span
            .metrics
            .get("_dd.apm.enabled")
            .is_some_and(|value| *value == 0.0)
        {
            return true;
        }
    }
    false
}

fn sampling_priority<T: TraceData>(trace: &[Span<T>]) -> Option<f64> {
    for span in trace {
        if let Some(priority) = span.metrics.get("_sampling_priority_v1") {
            return Some(*priority);
        }
    }
    None
}

fn decode_event<T: TraceData>(
    span: &Span<T>,
    encoded: &[u8],
) -> Result<Value, rmp_serde::decode::Error> {
    let data: Value = rmp_serde::from_slice(encoded)?;
    let data = data.as_object().cloned().unwrap_or_default();
    let trace_id = format!("{:032x}", span.trace_id);
    let span_id = span.span_id.to_string();

    let mut dd = object_field(&data, "_dd");
    dd.insert("span_id".to_string(), Value::String(span_id.clone()));
    dd.insert("trace_id".to_string(), Value::String(trace_id.clone()));
    dd.insert("apm_trace_id".to_string(), Value::String(trace_id.clone()));

    let tags = object_field(&data, "tags")
        .into_iter()
        .map(|(key, value)| Value::String(format!("{key}:{}", string_value(value))))
        .collect::<Vec<_>>();
    let mut meta = object_field(&data, "meta");
    if let Some(kind) = meta
        .remove("span")
        .and_then(|span| span.get("kind").cloned())
    {
        meta.insert("span.kind".to_string(), kind);
    }

    let mut event = Map::new();
    event.insert(
        "trace_id".to_string(),
        data.get("trace_id")
            .cloned()
            .unwrap_or_else(|| Value::String(trace_id)),
    );
    event.insert("span_id".to_string(), Value::String(span_id));
    event.insert(
        "parent_id".to_string(),
        data.get("parent_id")
            .cloned()
            .unwrap_or_else(|| Value::String("0".to_string())),
    );
    event.insert(
        "name".to_string(),
        data.get("name")
            .cloned()
            .unwrap_or_else(|| Value::String(span.name.borrow().to_string())),
    );
    event.insert("start_ns".to_string(), json!(span.start));
    event.insert("duration".to_string(), json!(span.duration));
    event.insert(
        "status".to_string(),
        Value::String(if span.error == 0 { "ok" } else { "error" }.to_string()),
    );
    event.insert("meta".to_string(), Value::Object(meta));
    event.insert(
        "metrics".to_string(),
        Value::Object(object_field(&data, "metrics")),
    );
    event.insert("tags".to_string(), Value::Array(tags));
    event.insert("_dd".to_string(), Value::Object(dd));

    for key in ["session_id", "span_links", "config"] {
        if let Some(value) = data.get(key) {
            event.insert(key.to_string(), value.clone());
        }
    }

    Ok(Value::Object(event))
}

fn object_field(data: &Map<String, Value>, key: &str) -> Map<String, Value> {
    data.get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn string_value(value: Value) -> String {
    match value {
        Value::String(value) => value,
        value => value.to_string(),
    }
}

/// Send standalone LLMObs events through EVP proxy, falling back to direct intake.
pub(crate) async fn send_events<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    agent_url: &Uri,
    config: &LlmObsConfig,
    tracer_version: &str,
    events: Vec<Value>,
) -> Result<(), TraceExporterError> {
    if events.is_empty() {
        return Ok(());
    }

    for body in encode_payloads(events, tracer_version)? {
        send_payload(capabilities, agent_url, config, body).await?;
    }
    Ok(())
}

async fn send_payload<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    agent_url: &Uri,
    config: &LlmObsConfig,
    body: Vec<u8>,
) -> Result<(), TraceExporterError> {
    let mut evp_headers = HeaderMap::new();
    evp_headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    evp_headers.insert(
        EVP_SUBDOMAIN_HEADER,
        HeaderValue::from_static(LLMOBS_INTAKE_SUBDOMAIN),
    );
    let evp_endpoint = endpoint_with_path(agent_url, EVP_PROXY_PATH, config.timeout)?;

    if send(&evp_endpoint, body.clone(), &evp_headers, capabilities)
        .await
        .is_ok()
    {
        return Ok(());
    }

    if config.api_key.is_empty() || config.agentless_endpoint.is_empty() {
        error!("LLMObs EVP proxy delivery failed and direct fallback is not configured");
        return Err(TraceExporterError::Internal(
            InternalErrorKind::InvalidWorkerState(
                "LLMObs delivery failed and no API key/direct endpoint is configured".to_string(),
            ),
        ));
    }

    warn!("LLMObs EVP proxy delivery failed; falling back to direct intake");
    let agentless_url = Uri::from_str(&config.agentless_endpoint).map_err(|error| {
        TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
            "Invalid LLMObs agentless endpoint: {error}"
        )))
    })?;
    let mut direct_headers = HeaderMap::new();
    direct_headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    direct_headers.insert(
        "dd-api-key",
        HeaderValue::from_str(&config.api_key).map_err(|error| {
            TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
                "Invalid LLMObs API key header: {error}"
            )))
        })?,
    );
    let direct_endpoint = Endpoint {
        url: agentless_url,
        timeout_ms: config.timeout.as_millis() as u64,
        ..Endpoint::default()
    };
    send(&direct_endpoint, body, &direct_headers, capabilities).await
}

fn encode_payloads(
    events: Vec<Value>,
    tracer_version: &str,
) -> Result<Vec<Vec<u8>>, TraceExporterError> {
    let mut payloads = Vec::new();
    let mut wrappers = Vec::new();
    let mut payload_size = 2;

    for mut event in events {
        if serde_json::to_vec(&event).is_ok_and(|encoded| encoded.len() > EVENT_SIZE_LIMIT) {
            truncate_event_io(&mut event);
        }
        let wrapper = json!({
            "_dd.stage": "raw",
            "_dd.tracer_version": tracer_version,
            "event_type": LLMOBS_EVENT_TYPE,
            "spans": [event],
        });
        let wrapper_size = serde_json::to_vec(&wrapper)
            .map_err(serialization_error)?
            .len();
        let separator_size = usize::from(!wrappers.is_empty());
        if !wrappers.is_empty() && payload_size + separator_size + wrapper_size > PAYLOAD_SIZE_LIMIT
        {
            payloads.push(serde_json::to_vec(&wrappers).map_err(serialization_error)?);
            wrappers.clear();
            payload_size = 2;
        }
        payload_size += usize::from(!wrappers.is_empty()) + wrapper_size;
        wrappers.push(wrapper);
    }

    if !wrappers.is_empty() {
        payloads.push(serde_json::to_vec(&wrappers).map_err(serialization_error)?);
    }
    Ok(payloads)
}

fn truncate_event_io(event: &mut Value) {
    let Some(event) = event.as_object_mut() else {
        return;
    };
    let meta = event
        .entry("meta")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(meta) = meta.as_object_mut() {
        let dropped = || json!({ "value": DROPPED_IO_VALUE });
        meta.insert("input".to_string(), dropped());
        meta.insert("output".to_string(), dropped());
    }
    event.insert("collection_errors".to_string(), json!(["dropped_io"]));
}

fn serialization_error(error: serde_json::Error) -> TraceExporterError {
    TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(error.to_string()))
}

fn endpoint_with_path(
    base: &Uri,
    path: &str,
    timeout: Duration,
) -> Result<Endpoint, TraceExporterError> {
    let mut parts = base.clone().into_parts();
    parts.path_and_query = Some(path.parse().map_err(|error| {
        TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
            "Invalid LLMObs EVP path: {error}"
        )))
    })?);
    let url = Uri::from_parts(parts).map_err(|error| {
        TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
            "Invalid LLMObs EVP endpoint: {error}"
        )))
    })?;
    Ok(Endpoint {
        url,
        timeout_ms: timeout.as_millis() as u64,
        ..Endpoint::default()
    })
}

async fn send<C: HttpClientCapability + SleepCapability>(
    endpoint: &Endpoint,
    body: Vec<u8>,
    headers: &HeaderMap,
    capabilities: &C,
) -> Result<(), TraceExporterError> {
    let strategy = RetryStrategy::new(
        MAX_RETRIES,
        RETRY_DELAY_MS,
        RetryBackoffType::Exponential,
        None,
    );
    match send_with_retry(capabilities, endpoint, body, headers, &strategy).await {
        Ok(_) => Ok(()),
        Err(SendWithRetryError::Http(response, _)) => {
            let status = response.status();
            let body = String::from_utf8_lossy(response.body());
            Err(TraceExporterError::Request(RequestError::new(
                status, &body,
            )))
        }
        Err(SendWithRetryError::Timeout(_)) => Err(TraceExporterError::Io(std::io::Error::from(
            std::io::ErrorKind::TimedOut,
        ))),
        Err(SendWithRetryError::Network(error, _)) => Err(TraceExporterError::from(error)),
        Err(SendWithRetryError::ResponseBody(_)) => Err(TraceExporterError::Internal(
            InternalErrorKind::InvalidWorkerState(
                "Failed to read LLMObs response body".to_string(),
            ),
        )),
        Err(SendWithRetryError::Build(_)) => Err(TraceExporterError::Internal(
            InternalErrorKind::InvalidWorkerState("Failed to build LLMObs request".to_string()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use bytes::Bytes as HttpBytes;
    use http::{Request, Response, StatusCode};
    use libdd_capabilities::http::HttpError;
    use libdd_capabilities::{HttpClientCapability, MaybeSend, SleepCapability};
    use libdd_tinybytes::{Bytes, BytesString};
    use libdd_trace_utils::span::{v04::Span, BytesData};
    use serde_json::{json, Value};

    use super::{encode_payloads, route_events, send_events, LlmObsConfig, EVENT_SIZE_LIMIT};

    #[derive(Clone, Debug, Default)]
    struct TestCapabilities {
        requests: Arc<Mutex<Vec<(String, http::HeaderMap, HttpBytes)>>>,
        statuses: Arc<Mutex<VecDeque<StatusCode>>>,
    }

    impl HttpClientCapability for TestCapabilities {
        fn new_client() -> Self {
            Self::default()
        }

        fn request(
            &self,
            request: Request<HttpBytes>,
        ) -> impl std::future::Future<Output = Result<Response<HttpBytes>, HttpError>> + MaybeSend
        {
            let requests = self.requests.clone();
            let statuses = self.statuses.clone();
            async move {
                requests.lock().expect("requests lock").push((
                    request.uri().to_string(),
                    request.headers().clone(),
                    request.body().clone(),
                ));
                let status = statuses
                    .lock()
                    .expect("statuses lock")
                    .pop_front()
                    .unwrap_or(StatusCode::ACCEPTED);
                Response::builder()
                    .status(status)
                    .body(HttpBytes::new())
                    .map_err(|error| HttpError::InvalidRequest(error.into()))
            }
        }
    }

    impl SleepCapability for TestCapabilities {
        fn new() -> Self {
            Self::default()
        }

        fn sleep(&self, _duration: Duration) -> impl std::future::Future<Output = ()> + MaybeSend {
            std::future::ready(())
        }
    }

    fn llmobs_span(priority: f64) -> Span<BytesData> {
        let mut span = Span {
            name: BytesString::from_static("operation"),
            trace_id: 0x123,
            span_id: 456,
            start: 100,
            duration: 20,
            ..Span::default()
        };
        span.metrics
            .insert(BytesString::from_static("_sampling_priority_v1"), priority);
        span.meta_struct.insert(
            BytesString::from_static("_llmobs"),
            Bytes::from(
                rmp_serde::to_vec_named(&json!({
                    "name": "llm",
                    "trace_id": "llm-trace",
                    "parent_id": "0",
                    "tags": {"ml_app": "test"},
                    "meta": {"span": {"kind": "llm"}},
                    "metrics": {},
                    "_dd": {}
                }))
                .expect("test LLMObs data should serialize"),
            ),
        );
        span
    }

    #[test]
    fn rejected_trace_is_scrubbed_and_routed_standalone() {
        let mut traces = vec![vec![llmobs_span(0.0)]];

        let routed = route_events(&mut traces);

        assert_eq!(routed.standalone.len(), 1);
        assert!(routed.rescue.is_empty());
        assert!(!traces[0][0].meta_struct.contains_key("_llmobs"));
        assert_eq!(routed.standalone[0]["span_id"], "456");
        assert_eq!(routed.standalone[0]["trace_id"], "llm-trace");
        assert_eq!(routed.standalone[0]["meta"]["span.kind"], "llm");
    }

    #[test]
    fn kept_trace_retains_meta_struct_and_rescue_event() {
        let mut traces = vec![vec![llmobs_span(1.0)]];

        let routed = route_events(&mut traces);

        assert!(routed.standalone.is_empty());
        assert_eq!(routed.rescue.len(), 1);
        assert!(traces[0][0].meta_struct.contains_key("_llmobs"));
    }

    #[test]
    fn malformed_meta_struct_is_scrubbed_without_routing() {
        let mut span = llmobs_span(0.0);
        span.meta_struct.insert(
            BytesString::from_static("_llmobs"),
            Bytes::from_static(&[0xc1]),
        );
        let mut traces = vec![vec![span]];

        let routed = route_events(&mut traces);

        assert!(routed.standalone.is_empty());
        assert!(routed.rescue.is_empty());
        assert!(!traces[0][0].meta_struct.contains_key("_llmobs"));
    }

    #[test]
    fn apm_disabled_trace_is_routed_standalone_and_removed() {
        let mut span = llmobs_span(1.0);
        span.metrics
            .insert(BytesString::from_static("_dd.apm.enabled"), 0.0);
        let mut traces = vec![vec![span]];

        let routed = route_events(&mut traces);

        assert_eq!(routed.standalone.len(), 1);
        assert!(routed.rescue.is_empty());
        assert!(traces.is_empty());
    }

    #[tokio::test]
    async fn evp_failure_falls_back_to_direct_intake() {
        let capabilities = TestCapabilities::default();
        capabilities
            .statuses
            .lock()
            .expect("statuses lock")
            .extend([
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::ACCEPTED,
            ]);
        let config = LlmObsConfig {
            agentless_endpoint: "http://direct.test/api/v2/llmobs".to_string(),
            api_key: "test-key".to_string(),
            timeout: Duration::from_secs(1),
        };

        send_events(
            &capabilities,
            &"http://agent.test:8126".parse().expect("agent URL"),
            &config,
            "1.2.3",
            vec![json!({"name": "llm"})],
        )
        .await
        .expect("direct fallback should succeed");

        let requests = capabilities.requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[0].0,
            "http://agent.test:8126/evp_proxy/v2/api/v2/llmobs"
        );
        assert_eq!(requests[0].1["x-datadog-evp-subdomain"], "llmobs-intake");
        assert_eq!(requests[3].0, "http://direct.test/api/v2/llmobs");
        assert_eq!(requests[3].1["dd-api-key"], "test-key");
    }

    #[test]
    fn oversized_event_drops_input_and_output() {
        let event = json!({
            "meta": {
                "input": {"value": "x".repeat(EVENT_SIZE_LIMIT)},
                "output": {"value": "output"}
            }
        });

        let payloads = encode_payloads(vec![event], "1.2.3").expect("payload should encode");
        let payload: Value =
            serde_json::from_slice(&payloads[0]).expect("payload should be valid JSON");
        let event = &payload[0]["spans"][0];

        assert_eq!(event["collection_errors"], json!(["dropped_io"]));
        assert!(event["meta"]["input"]["value"]
            .as_str()
            .is_some_and(|value| value.contains("5MB")));
        assert!(event["meta"]["output"]["value"]
            .as_str()
            .is_some_and(|value| value.contains("5MB")));
    }

    #[test]
    fn payloads_are_split_at_five_megabytes() {
        let event = json!({
            "meta": {"metadata": {"value": "x".repeat(3 << 20)}}
        });

        let payloads =
            encode_payloads(vec![event.clone(), event], "1.2.3").expect("payloads should encode");

        assert_eq!(payloads.len(), 2);
    }
}
