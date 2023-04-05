// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use async_trait::async_trait;
use datadog_trace_protobuf::pb;
use hyper::{http, Body, Request, Response};

use datadog_trace_utils::trace_utils;
use tokio::sync::mpsc::Sender;

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
        let traces = match trace_utils::get_traces_from_request_body(body).await {
            Ok(res) => res,
            Err(err) => {
                return Response::builder().body(Body::from(format!(
                    "Error deserializing trace from request body: {}",
                    err
                )));
            }
        };

        let trace_chunks: Vec<pb::TraceChunk> = traces
            .iter()
            .map(|trace| trace_utils::construct_trace_chunk(trace.to_vec()))
            .collect();

        let tracer_payload = trace_utils::construct_tracer_payload(trace_chunks, tracer_tags);

        // send trace payload to our trace flusher
        match tx.send(tracer_payload).await {
            Ok(_) => Response::builder()
                .status(200)
                .body(Body::from("Successfully buffered traces to be flushed.")),
            Err(e) => Response::builder().status(500).body(Body::from(format!(
                "Error sending traces to the trace flusher. Error: {}",
                e
            ))),
        }
    }
}
