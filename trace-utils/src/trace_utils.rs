// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use curl::easy::{Easy, List};
use hyper::{body::Buf, Body, Request};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{
    env,
    io::{Cursor, Read},
    str,
};

use prost::Message;

use datadog_trace_protobuf::pb;

const TRACE_INTAKE_URL: &str = "https://trace.agent.datadoghq.com/api/v0.2/traces";

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

pub async fn get_traces_from_request_body(
    req: Request<Body>,
) -> anyhow::Result<Vec<Vec<pb::Span>>> {
    let buffer = hyper::body::aggregate(req).await.unwrap();

    let traces: Vec<Vec<Span>> = match rmp_serde::from_read(buffer.reader()) {
        Ok(res) => res,
        Err(err) => {
            anyhow::bail!("error deserializing trace: {:#?}", err)
        }
    };

    let mut pb_traces = Vec::<Vec<pb::Span>>::new();
    for trace in traces {
        let mut pb_spans = Vec::<pb::Span>::new();
        for span in trace.iter() {
            let span = pb::Span {
                service: span.service.clone().unwrap_or_default(),
                name: span.name.clone(),
                resource: span.resource.clone(),
                trace_id: span.trace_id,
                span_id: span.span_id,
                parent_id: span.parent_id.unwrap_or_default(),
                start: span.start,
                duration: span.duration,
                error: span.error.unwrap_or(0),
                meta: span.meta.clone(),
                meta_struct: HashMap::new(),
                metrics: span.metrics.clone().unwrap_or_default(),
                r#type: "custom".to_string(),
            };

            pb_spans.push(span);
        }
        if !pb_spans.is_empty() {
            pb_traces.push(pb_spans);
        }
    }

    if pb_traces.is_empty() {
        anyhow::bail!("no traces deserialized from the request body.")
    }

    Ok(pb_traces)
}

pub fn construct_agent_payload(traces: Vec<Vec<pb::Span>>) -> pb::AgentPayload {
    let mut tracer_payloads = Vec::<pb::TracerPayload>::new();

    for trace in traces {
        let chunks = vec![pb::TraceChunk {
            priority: 1,
            origin: "ffi-origin".to_string(),
            spans: trace,
            tags: HashMap::new(),
            dropped_trace: false,
        }];

        let tracer_payload = pb::TracerPayload {
            app_version: "mini-agent-1.0.0".to_string(),
            language_name: "mini-agent-nodejs".to_string(),
            container_id: "mini-agent-containerid".to_string(),
            chunks,
            env: "mini-agent-env".to_string(),
            hostname: "mini-agent-hostname".to_string(),
            language_version: "mini-agent-nodejs-version".to_string(),
            runtime_id: "mini-agent-runtime-id".to_string(),
            tags: HashMap::new(),
            tracer_version: "tracer-v-1".to_string(),
        };

        tracer_payloads.push(tracer_payload);
    }

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

fn construct_headers() -> anyhow::Result<List> {
    let api_key = match env::var("DD_API_KEY") {
        Ok(key) => key,
        Err(_) => anyhow::bail!("oopsy, no DD_API_KEY was provided"),
    };
    let mut list = List::new();
    list.append(format!("User-agent: {}", "ffi-test").as_str())?;
    list.append(format!("Content-type: {}", "application/x-protobuf").as_str())?;
    list.append(format!("DD-API-KEY: {}", &api_key).as_str())?;
    list.append(format!("X-Datadog-Reported-Languages: {}", "nodejs").as_str())?;
    Ok(list)
}

pub fn serialize_agent_payload(payload: pb::AgentPayload) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.reserve(payload.encoded_len());
    payload.encode(&mut buf).unwrap();
    buf
}

pub fn send(data: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    let mut easy = Easy::new();
    let mut dst = Vec::new();
    let len = data.len();
    let mut data_cursor = Cursor::new(data);
    {
        easy.url(TRACE_INTAKE_URL)?;
        easy.post(true)?;
        easy.post_field_size(len as u64)?;
        easy.http_headers(construct_headers()?)?;

        let mut transfer = easy.transfer();

        transfer.read_function(|buf| Ok(data_cursor.read(buf).unwrap_or(0)))?;

        transfer.write_function(|result_data| {
            dst.extend_from_slice(result_data);
            match str::from_utf8(result_data) {
                Ok(_) => {
                    println!("successfully sent traces");
                }
                Err(e) => println!("failed to send traces: error: {}", e),
            };
            Ok(result_data.len())
        })?;

        transfer.perform()?;
    }
    Ok(dst)
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
            let res = trace_utils::get_traces_from_request_body(request).await;
            assert!(res.is_ok());
            assert_eq!(res.unwrap(), output);
        }
    }
}
