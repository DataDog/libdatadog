// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use async_trait::async_trait;
use dyn_clone::DynClone;
use hyper::{Body, Request, Response};

use datadog_trace_utils::trace_utils;

use crate::trace_flusher;

#[async_trait]
pub trait TraceProcessor: DynClone {
    async fn process_traces(&self, req: Request<Body>) -> Response<Body>;
}
dyn_clone::clone_trait_object!(TraceProcessor);

#[derive(Clone)]
pub struct ServerlessTraceProcessor {
    pub trace_flusher: Box<dyn trace_flusher::TraceFlusher + Send + Sync>,
}

#[async_trait]
impl TraceProcessor for ServerlessTraceProcessor {
    async fn process_traces(&self, req: Request<Body>) -> Response<Body> {
        println!("in post_trace");

        let trace = trace_utils::deserialize_trace_from_hyper_req_body(req).await;

        self.trace_flusher.flush_traces(trace);

        Response::builder().body(Body::default()).unwrap()
    }
}
