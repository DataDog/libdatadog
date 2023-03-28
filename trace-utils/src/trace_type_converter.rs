use std::collections::HashMap;

use datadog_trace_protobuf::pb;
use hyper::{body::Buf, Body, Request};
use serde::{Deserialize, Serialize};

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

    return vecs.get(0).unwrap().to_vec();
}

pub fn convert_to_pb_trace(trace: Vec<Span>) -> Vec<pb::Span> {
    let mut pb_spans = Vec::<pb::Span>::new();

    let mut min_start_date = i64::MAX;
    let mut max_end_date = 0;
    let mut trace_id = 0;
    let mut span_id = 0;

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

        if span.start < min_start_date {
            span_id = span.span_id;
            min_start_date = span.start;
        }

        if span.start + span.duration > max_end_date {
            max_end_date = span.start + span.duration;
        }

        trace_id = span.trace_id;
        pb_spans.push(span);
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

    pb_spans.push(enclosing_span);

    for span in pb_spans.iter_mut() {
        if span.span_id == span_id {
            span.parent_id = span_id + 1;
        }
    }

    pb_spans
}
