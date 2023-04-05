// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use async_trait::async_trait;
use datadog_trace_protobuf::pb;
use hyper::{http, Body, Request, Response};
use log::{error, info};
use tokio::sync::mpsc::Sender;

use datadog_trace_normalization::normalizer;
use datadog_trace_utils::trace_utils;

macro_rules! parse_root_span_tags {
    (
        $meta_map:ident,
        { $($header:literal => $($field:ident).+ ,)+ }
    ) => {
        $(
            if let Some(tag) = $meta_map.get($header) {
                $($field).+ = tag;
            }
        )+
    }
}

#[async_trait]
pub trait TraceProcessor {
    /// Deserializes traces from a hyper request body and sends them through
    /// the provided tokio mpsc Sender.
    async fn process_traces(
        &self,
        req: Request<Body>,
        tx: Sender<pb::TracerPayload>,
    ) -> http::Result<Response<Body>>;
}

#[derive(Clone)]
pub struct ServerlessTraceProcessor {}

#[async_trait]
impl TraceProcessor for ServerlessTraceProcessor {
    async fn process_traces(
        &self,
        req: Request<Body>,
        tx: Sender<pb::TracerPayload>,
    ) -> http::Result<Response<Body>> {
        let (parts, body) = req.into_parts();

        info!("request parts: {:#?}", parts);

        let tracer_header_tags = trace_utils::get_tracer_header_tags(&parts.headers);

        // deserialize traces from the request body, convert to protobuf structs (see trace-protobuf crate)
        let mut traces = match trace_utils::get_traces_from_request_body(body).await {
            Ok(res) => res,
            Err(err) => {
                return Response::builder().body(Body::from(format!(
                    "Error deserializing trace from request body: {}",
                    err
                )));
            }
        };

        let mut trace_chunks: Vec<pb::TraceChunk> = Vec::new();

        let mut gathered_root_span_tags = false;
        let mut root_span_tags = trace_utils::RootSpanTags::default();

        for trace in traces.iter_mut() {
            match normalizer::normalize_trace(trace) {
                Ok(_) => (),
                Err(e) => error!("Error normalizing trace: {}", e),
            }

            let mut chunk = trace_utils::construct_trace_chunk(trace.to_vec());

            let root_span_index = match trace_utils::get_root_span_index(trace) {
                Ok(res) => res,
                Err(e) => {
                    error!(
                        "Error getting the root span index of a trace, skipping. {}",
                        e,
                    );
                    continue;
                }
            };

            match normalizer::normalize_chunk(&mut chunk, root_span_index) {
                Ok(_) => (),
                Err(e) => error!("Error normalizing trace chunk: {}", e),
            }

            if !tracer_header_tags.client_computed_top_level {
                trace_utils::compute_top_level_span(trace);
            }

            trace_chunks.push(chunk);

            if !gathered_root_span_tags {
                gathered_root_span_tags = true;
                let meta_map = &trace[root_span_index].meta;
                parse_root_span_tags!(
                    meta_map,
                    {
                        "env" => root_span_tags.env,
                        "version" => root_span_tags.app_version,
                        "_dd.hostname" => root_span_tags.hostname,
                        "runtime-id" => root_span_tags.runtime_id,
                    }
                );
            }
        }

        let tracer_payload =
            trace_utils::construct_tracer_payload(trace_chunks, tracer_header_tags, root_span_tags);

        // send trace payload to our trace flusher
        match tx.send(tracer_payload).await {
            Ok(_) => Response::builder()
                .status(200)
                .body(Body::from("Successfully buffered traces to be flushed.")),
            Err(e) => Response::builder().status(500).body(Body::from(format!(
                "Error sending traces to the trace flusher. {}",
                e
            ))),
        }
    }
}
