// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use hyper::http::HeaderValue;
use hyper::HeaderMap;
use hyper::{body::Buf, Body, Client, Method, Request};
use hyper_tls::HttpsConnector;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{env, str};

use prost::Message;

use datadog_trace_protobuf::pb;

const TRACE_INTAKE_URL: &str = "https://trace.agent.datadoghq.com/api/v0.2/traces";

macro_rules! parse_header {
    (
        $header_map:ident,
        { $($header:literal => $($field:ident).+ ,)+ }
    ) => {
        $(
            if let Some(h) = $header_map.get($header) {
                if let Ok(h) = h.to_str() {
                    $($field).+ = h;
                }
            }
        )+
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct Span {
    service: Option<String>,
    name: String,
    resource: String,
    trace_id: u64,
    span_id: u64,
    parent_id: Option<u64>,
    start: i64,
    duration: i64,
    error: Option<i32>,
    meta: HashMap<String, String>,
    metrics: Option<HashMap<String, f64>>,
}

pub async fn get_traces_from_request_body(body: Body) -> anyhow::Result<Vec<Vec<pb::Span>>> {
    let buffer = hyper::body::aggregate(body).await.unwrap();

    let traces: Vec<Vec<Span>> = match rmp_serde::from_read(buffer.reader()) {
        Ok(res) => res,
        Err(err) => {
            anyhow::bail!("Error deserializing trace from request body: {}", err)
        }
    };

    let mut pb_traces = Vec::<Vec<pb::Span>>::new();
    for trace in traces {
        let mut pb_spans = Vec::<pb::Span>::new();
        for span in trace {
            let span = pb::Span {
                service: span.service.unwrap_or_default(),
                name: span.name,
                resource: span.resource,
                trace_id: span.trace_id,
                span_id: span.span_id,
                parent_id: span.parent_id.unwrap_or_default(),
                start: span.start,
                duration: span.duration,
                error: span.error.unwrap_or(0),
                meta: span.meta,
                meta_struct: HashMap::new(),
                metrics: span.metrics.unwrap_or_default(),
                r#type: "custom".to_string(),
            };

            pb_spans.push(span);
        }
        if !pb_spans.is_empty() {
            pb_traces.push(pb_spans);
        }
    }

    if pb_traces.is_empty() {
        anyhow::bail!("No traces deserialized from the request body.")
    }

    Ok(pb_traces)
}

#[derive(Default)]
pub struct TracerTags<'a> {
    lang: &'a str,
    lang_version: &'a str,
    lang_interpreter: &'a str,
    lang_vendor: &'a str,
    tracer_version: &'a str,
}

pub fn get_tracer_tags_from_request_header(headers: &HeaderMap<HeaderValue>) -> TracerTags {
    let mut tags = TracerTags::default();
    parse_header!(
        headers,
        {
            "datadog-meta-lang" => tags.lang,
            "datadog-meta-lang-version" => tags.lang_version,
            "datadog-meta-lang-interpreter" => tags.lang_interpreter,
            "datadog-meta-lang-vendor" => tags.lang_vendor,
            "datadog-meta-tracer-version" => tags.tracer_version,
        }
    );
    tags
}

pub fn construct_agent_payload(tracer_payloads: Vec<pb::TracerPayload>) -> pb::AgentPayload {
    pb::AgentPayload {
        host_name: "ffi-test-hostname".to_string(),
        env: "ffi-test-env".to_string(),
        agent_version: "ffi-agent-version".to_string(),
        error_tps: 60.0,
        target_tps: 60.0,
        tags: HashMap::new(),
        tracer_payloads,
    }
}

pub fn construct_trace_chunk(trace: Vec<pb::Span>) -> pb::TraceChunk {
    pb::TraceChunk {
        priority: 1,
        origin: "".to_string(),
        spans: trace,
        tags: HashMap::new(),
        dropped_trace: false,
    }
}

pub fn construct_tracer_payload(
    chunks: Vec<pb::TraceChunk>,
    tracer_tags: TracerTags,
) -> pb::TracerPayload {
    pb::TracerPayload {
        app_version: "placeholder_version".to_string(),
        language_name: tracer_tags.lang.to_string(),
        container_id: "".to_string(),
        env: "placeholder_env".to_string(),
        runtime_id: "".to_string(),
        chunks,
        hostname: "".to_string(),
        language_version: tracer_tags.lang_version.to_string(),
        tags: HashMap::new(),
        tracer_version: tracer_tags.tracer_version.to_string(),
    }
}

pub fn serialize_agent_payload(payload: pb::AgentPayload) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.reserve(payload.encoded_len());
    payload.encode(&mut buf).unwrap();
    buf
}

pub async fn send(data: Vec<u8>) -> anyhow::Result<()> {
    let api_key = match env::var("DD_API_KEY") {
        Ok(key) => key,
        Err(_) => anyhow::bail!("oopsy, no DD_API_KEY was provided"),
    };

    let req = Request::builder()
        .method(Method::POST)
        .uri(TRACE_INTAKE_URL)
        .header("User-agent", "ffi-test")
        .header("Content-type", "application/x-protobuf")
        .header("DD-API-KEY", &api_key)
        .header("X-Datadog-Reported-Languages", "nodejs")
        .body(Body::from(data))?;

    let https = HttpsConnector::new();
    let client = Client::builder().build::<_, hyper::Body>(https);
    match client.request(req).await {
        Ok(res) => {
            match hyper::body::to_bytes(res.into_body()).await {
                Ok(res) => println!("Successfully sent traces. Response body: {:#?}", res),
                Err(e) => println!("Successfully sent trace. Error when reading response body: {}", e),
            }
        }
        Err(e) => println!("Failed to send traces: {}", e),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use datadog_trace_protobuf::pb;
    use serde_json::json;

    use hyper::Request;

    use crate::trace_utils;

    #[tokio::test]
    async fn test_get_traces_from_request_body() {
        let pairs = vec![
            (
                json!([{
                    "service": "test-service",
                    "name": "test-service-name",
                    "resource": "test-service-resource",
                    "trace_id": 111,
                    "span_id": 222,
                    "parent_id": 333,
                    "start": 1,
                    "duration": 5,
                    "error": 0,
                    "meta": {},
                    "metrics": {},
                }]),
                vec![vec![pb::Span {
                    service: "test-service".to_string(),
                    name: "test-service-name".to_string(),
                    resource: "test-service-resource".to_string(),
                    trace_id: 111,
                    span_id: 222,
                    parent_id: 333,
                    start: 1,
                    duration: 5,
                    error: 0,
                    meta: HashMap::new(),
                    metrics: HashMap::new(),
                    meta_struct: HashMap::new(),
                    r#type: "custom".to_string(),
                }]],
            ),
            (
                json!([{
                    "name": "test-service-name",
                    "resource": "test-service-resource",
                    "trace_id": 111,
                    "span_id": 222,
                    "start": 1,
                    "duration": 5,
                    "meta": {},
                }]),
                vec![vec![pb::Span {
                    service: "".to_string(),
                    name: "test-service-name".to_string(),
                    resource: "test-service-resource".to_string(),
                    trace_id: 111,
                    span_id: 222,
                    parent_id: 0,
                    start: 1,
                    duration: 5,
                    error: 0,
                    meta: HashMap::new(),
                    metrics: HashMap::new(),
                    meta_struct: HashMap::new(),
                    r#type: "custom".to_string(),
                }]],
            ),
        ];

        for (trace_input, output) in pairs {
            let bytes = rmp_serde::to_vec(&vec![&trace_input]).unwrap();
            let request = Request::builder()
                .body(hyper::body::Body::from(bytes))
                .unwrap();
            let res = trace_utils::get_traces_from_request_body(request.into_body()).await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap(), output);
        }
    }
}
