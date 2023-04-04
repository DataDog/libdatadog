// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use async_trait::async_trait;
use datadog_trace_protobuf::pb;
use hyper::{http, Body, Request, Response};
use tokio::sync::mpsc::Sender;

use datadog_trace_normalization::normalizer;
use datadog_trace_utils::trace_utils;

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
        let tracer_tags = trace_utils::get_tracer_tags_from_request_header(&parts.headers);

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

        let mut payload_env = "";
        let mut payload_app_version = "";

        for trace in traces.iter_mut() {
            match normalizer::normalize_trace(trace) {
                Ok(_) => (),
                Err(e) => println!("Error normalizing trace: {}", e),
            }

            let mut chunk = trace_utils::construct_trace_chunk(trace);

            let root_span_index = match trace_utils::get_root_span_index(trace) {
                Ok(res) => res,
                Err(e) => {
                    println!(
                        "Error getting the root span index of a trace, skipping. {}",
                        e,
                    );
                    continue;
                }
            };

            match normalizer::normalize_chunk(&mut chunk, root_span_index) {
                Ok(_) => (),
                Err(e) => println!("Error normalizing trace chunk: {}", e),
            }

            if !tracer_tags.client_computed_top_level {
                trace_utils::compute_top_level_span(trace);
            }

            trace_chunks.push(chunk);

            if payload_env.is_empty() {
                if let Some(payload_env_root) = trace[root_span_index].meta.get("env") {
                    payload_env = payload_env_root;
                }
            }
            if payload_app_version.is_empty() {
                if let Some(payload_app_version_root) = trace[root_span_index].meta.get("version") {
                    payload_app_version = payload_app_version_root;
                }
            }
        }

        let tracer_payload = trace_utils::construct_tracer_payload(
            trace_chunks,
            tracer_tags,
            payload_env,
            payload_app_version,
        );

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
