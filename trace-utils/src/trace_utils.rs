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

#[derive(Debug, Deserialize, Serialize, Clone)]
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

pub async fn deserialize_trace_from_hyper_req_body(req: Request<Body>) -> Vec<Span> {
    let buffer = hyper::body::aggregate(req).await.unwrap();

    let vecs: Vec<Vec<Span>> = match rmp_serde::from_read(buffer.reader()) {
        Ok(res) => res,
        Err(err) => {
            println!("error deserializing trace: {:#?}", err);
            panic!("sad")
        }
    };

    println!("vecs deserialized from hyper req body: {:#?}", vecs);

    return vecs.get(0).unwrap().to_vec();
}

pub fn convert_to_pb_trace(trace: Vec<Span>) -> Vec<pb::Span> {
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

    pb_spans
}

pub fn add_enclosing_span(trace: &mut Vec<pb::Span>) {
    let mut min_start_date = i64::MAX;
    let mut max_end_date = 0;
    let mut trace_id = 0;
    let mut span_id = 0;

    for span in trace.iter() {
        if span.start < min_start_date {
            span_id = span.span_id;
            min_start_date = span.start;
        }

        if span.start + span.duration > max_end_date {
            max_end_date = span.start + span.duration;
        }

        trace_id = span.trace_id;
    }

    // create the enclosing span
    let enclosing_span = pb::Span {
        service: "mini-agent-service".to_string(),
        name: "gcp.cloud-function".to_string(),
        resource: "gcp.cloud-function".to_string(),
        trace_id,
        span_id: span_id + 1,
        parent_id: 0,
        start: min_start_date,
        duration: max_end_date - min_start_date,
        error: 0,
        meta: HashMap::new(),
        meta_struct: HashMap::new(),
        metrics: HashMap::new(),
        r#type: "custom".to_string(),
    };

    trace.push(enclosing_span);

    for span in trace.iter_mut() {
        if span.span_id == span_id {
            span.parent_id = span_id + 1;
        }
    }
}

pub fn construct_agent_payload(spans: Vec<pb::Span>) -> pb::AgentPayload {
    let chunks = vec![pb::TraceChunk {
        priority: 1,
        origin: "ffi-origin".to_string(),
        spans,
        tags: HashMap::new(),
        dropped_trace: false,
    }];

    let tracer_payloads = vec![pb::TracerPayload {
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
    }];

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

fn construct_headers() -> std::io::Result<List> {
    let api_key = match env::var("DD_API_KEY") {
        Ok(key) => key,
        Err(_) => panic!("oopsy, no DD_API_KEY was provided"),
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

pub fn send(data: Vec<u8>) -> std::io::Result<Vec<u8>> {
    let mut easy = Easy::new();
    let mut dst = Vec::new();
    let len = data.len();
    let mut data_cursor = Cursor::new(data);
    {
        easy.url("https://trace.agent.datadoghq.com/api/v0.2/traces")?;
        easy.post(true)?;
        easy.post_field_size(len as u64)?;
        easy.http_headers(construct_headers()?)?;

        let mut transfer = easy.transfer();

        transfer.read_function(|buf| Ok(data_cursor.read(buf).unwrap_or(0)))?;

        println!("PERFORMING SEND NOW");

        transfer.write_function(|result_data| {
            dst.extend_from_slice(result_data);
            match str::from_utf8(result_data) {
                Ok(v) => {
                    println!("sent-----------------");
                    println!("successfully sent:::::: {:?}", v);
                }
                Err(e) => panic!("Invalid UTF-8 sequence: {}", e),
            };
            Ok(result_data.len())
        })?;

        transfer.perform()?;
    }
    Ok(dst)
}
