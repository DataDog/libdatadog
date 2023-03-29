// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use async_trait::async_trait;
use datadog_trace_protobuf::pb;
use dyn_clone::DynClone;
use hyper::{http, Body, Request, Response};

use datadog_trace_utils::trace_utils;
use tokio::sync::mpsc::Sender;

#[async_trait]
pub trait TraceProcessor: DynClone {
    async fn process_traces(
        &self,
        req: Request<Body>,
        tx: Sender<Vec<Vec<pb::Span>>>,
    ) -> http::Result<Response<Body>>;
}
dyn_clone::clone_trait_object!(TraceProcessor);

#[derive(Clone)]
pub struct ServerlessTraceProcessor {}

#[async_trait]
impl TraceProcessor for ServerlessTraceProcessor {
    async fn process_traces(
        &self,
        req: Request<Body>,
        tx: Sender<Vec<Vec<pb::Span>>>,
    ) -> http::Result<Response<Body>> {
        // deserialize traces from the request body, convert to protobuf structs (see trace-protobuf crate)
        let traces = match trace_utils::get_traces_from_request_body(req).await {
            Ok(res) => res,
            Err(err) => {
                return Response::builder().body(Body::from(format!(
                    "error deserializing trace from request body: {}",
                    err
                )));
            }
        };

        // send traces to our trace flusher
        match tx.send(traces).await {
            Ok(_) => Response::builder()
                .status(200)
                .body(Body::from("successfully buffered traces to be flushed.")),
            Err(_) => Response::builder()
                .status(500)
                .body(Body::from("error sending traces to the trace flusher.")),
        }
    }
}
