#![deny(clippy::all)]

use curl::easy::{Easy, List};
use prost::Message;
use std::collections::HashMap;
use std::env;
use std::time::SystemTime;
use serde::{Deserialize, Serialize};
use std::str;
use std::io::Cursor;
use std::io::Read;

// use std::ffi::c_char;
// use std::ffi::CStr;
// use napi::{CallContext, Error, JsNumber, JsObject, JsUnknown, Result, Status};

pub mod pb {
    include!("pb.rs");
}

mod normalize_utils_tests {
    include!("./normalization/tests/normalize_utils_tests.rs");
}

mod normalizer_tests {
    include!("./normalization/tests/normalizer_tests.rs");
}

pub mod normalizer {
    include!("./normalization/normalizer.rs");
}

pub mod normalize_utils {
    include!("./normalization/normalize_utils.rs");
}

pub mod errors {
    include!("./normalization/errors.rs");
}

#[macro_use]
extern crate napi_derive;

#[derive(Debug, Deserialize, Serialize)]
#[repr(C)]
pub struct Span {
    service: String,
    name: String,
    resource: String,
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    duration: i64,
    error: i32,
}

fn construct_headers() -> std::io::Result<List> {
    let api_key;
    match env::var("DD_API_KEY") {
        Ok(key) => api_key = key,
        Err(_) => panic!("oopsy, no DD_API_KEY was provided"),
    };
    let mut list = List::new();
    list.append(format!("User-agent: {}", "ffi-test").as_str())?;
    list.append(format!("Content-type: {}", "application/x-protobuf").as_str())?;
    list.append(format!("DD-API-KEY: {}", &api_key).as_str())?;
    list.append(format!("X-Datadog-Reported-Languages: {}", "nodejs").as_str())?;
    Ok(list)
}

fn send(data: Vec<u8>) -> std::io::Result<Vec<u8>> {
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
                },
                Err(e) => panic!("Invalid UTF-8 sequence: {}", e),
            };
            Ok(result_data.len())
        })?;

        transfer.perform()?;
    }
    return Ok(dst);
}

#[no_mangle]
#[napi]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn send_trace(trace_str: String, before_time: i64) {
    let duration_since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let timestamp_nanos: i64 = duration_since_epoch.as_micros() as i64;

    let time_diff = timestamp_nanos - before_time;

    println!(
        "Time taken to launch FFI function: {:?}ms",
        time_diff as f64 / 1000.0
    );

    println!("SENDING TRACE FROM RUST");

    let r_str = trace_str.as_str();

    let spans: Vec<Span> = serde_json::from_str(r_str).expect("Couldn't unwrap JSON");

    let mut tracer_payloads = Vec::<pb::TracerPayload>::new();
    let mut tags = HashMap::new();
    tags.insert("ffi-tag-a".to_string(), "ffi-valuetag-a".to_string());

    let mut span_to_send = Vec::<pb::Span>::new();

    let mut min_start_date = i64::MAX;
    let mut max_end_date = 0;
    let mut trace_id = 0;
    let mut span_id = 0;

    let mut meta_map = HashMap::new();
    meta_map.insert("poc".to_string(), "true".to_string());
    meta_map.insert("napi_rs".to_string(), "true".to_string());
    meta_map.insert("_dd.origin".to_string(), "ffi-service".to_string());
    
    let mut metrics_map = HashMap::new();
    metrics_map.insert("_dd.agent_psr".to_string(), 1 as f64);
    metrics_map.insert("_sample_rate".to_string(), 1 as f64);
    metrics_map.insert("_sampling_priority_v1".to_string(), 1 as f64);
    metrics_map.insert("_top_level".to_string(), 1 as f64);

    for single_span in spans.iter() {
        let span = pb::Span {
            service: single_span.service.clone(),
            name: single_span.name.clone(),
            resource: single_span.resource.clone(),
            trace_id: single_span.trace_id,
            span_id: single_span.span_id,
            parent_id: single_span.parent_id,
            start: single_span.start,
            duration: single_span.duration,
            error: single_span.error,
            meta: meta_map.clone(),
            meta_struct: HashMap::new(),
            metrics: metrics_map.clone(),
            r#type: "custom".to_string(),
        };

        if single_span.start < min_start_date {
            span_id = single_span.span_id;
            min_start_date = single_span.start;
        }

        if single_span.start + single_span.duration > max_end_date {
            max_end_date = single_span.start + single_span.duration;
        }

        trace_id = single_span.trace_id;
        span_to_send.push(span);
    }

    // create the enclosing span
    let enclosing_span = pb::Span {
        service: "ffi-service".to_string(),
        name: "gcp.cloud-function".to_string(),
        resource: "gcp.cloud-function".to_string(),
        trace_id: trace_id,
        span_id: span_id + 1,
        parent_id: 0,
        start: min_start_date,
        duration: max_end_date - min_start_date,
        error: 0,
        meta: meta_map.clone(),
        meta_struct: HashMap::new(),
        metrics: metrics_map.clone(),
        r#type: "custom".to_string(),
    };

    span_to_send.push(enclosing_span);

    for single_span in span_to_send.iter_mut() {
        if single_span.span_id == span_id {
            single_span.parent_id = span_id + 1;
        }
    }

    println!("[trace_sender] spans = {:?}", span_to_send);

    let trace_chunk = pb::TraceChunk {
        priority: 1,
        origin: "ffi-origin".to_string(),
        spans: span_to_send,
        tags: tags.clone(),
        dropped_trace: false,
    };

    let mut chunks = Vec::<pb::TraceChunk>::new();
    chunks.push(trace_chunk);

    let single_payload = pb::TracerPayload {
        app_version: "ffi-1.0.0".to_string(),
        language_name: "ffi-nodejs".to_string(),
        container_id: "ffi-containerid".to_string(),
        chunks: chunks,
        env: "ffi-env".to_string(),
        hostname: "ffi-hostname".to_string(),
        language_version: "ffi-nodejs-version".to_string(),
        runtime_id: "ffi-runtime-id".to_string(),
        tags: tags.clone(),
        tracer_version: "tracer-v-1".to_string(),
    };
    tracer_payloads.push(single_payload);

    let agent_payload = pb::AgentPayload {
        host_name: "ffi-test-hostname".to_string(),
        env: "ffi-test-env".to_string(),
        agent_version: "ffi-agent-version".to_string(),
        error_tps: 60.0,
        target_tps: 60.0,
        tags: tags.clone(),
        tracer_payloads: tracer_payloads,
    };

    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(n) => {
            println!("before send {:?}", n.as_millis())
        }
        Err(_) => panic!("SystemTime error"),
    }

    let encoded = serialize_agent_payload(&agent_payload);

    match send(encoded) {
        Ok(_) => {}
        Err(e) => {
            panic!("Error sending trace: {:?}", e);
        }
    }

    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
      Ok(n) => {
          println!("before send {:?}", n.as_millis())
      }
      Err(_) => panic!("SystemTime error"),
    }
}

pub fn serialize_agent_payload(payload: &pb::AgentPayload) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.reserve(payload.encoded_len());
    payload.encode(&mut buf).unwrap();
    buf
}

