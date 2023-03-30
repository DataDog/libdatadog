// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use datadog_trace_protobuf::pb;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use std::convert::Infallible;
use std::net::SocketAddr;

use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::{trace_flusher, trace_processor};

const MINI_AGENT_PORT: usize = 8126;
const TRACE_ENDPOINT_PATH: &str = "/v0.4/traces";

type Trace = Vec<pb::Span>;

pub struct MiniAgent {
    pub trace_processor: Box<dyn trace_processor::TraceProcessor + Send + Sync>,
    pub trace_flusher: Box<dyn trace_flusher::TraceFlusher + Send + Sync>,
}

impl MiniAgent {
    #[tokio::main]
    pub async fn start_mini_agent(&self) -> Result<(), Box<dyn std::error::Error>> {
        // setup a channel to send processed traces to our flusher
        // tx is passed through each endpoint_handler to the trace processor, which uses it to send de-serialized processed
        // traces to our trace flusher.
        let (tx, rx): (Sender<Vec<Trace>>, Receiver<Vec<Trace>>) = mpsc::channel(10);

        // start our trace flusher. receives traces and handles buffering + deciding when to flush to backend.
        let trace_flusher = self.trace_flusher.clone();
        tokio::spawn(async move {
            let trace_flusher = trace_flusher.clone();
            println!("starting trace flusher");
            trace_flusher.start_trace_flusher(rx).await;
        });

        // setup our hyper http server, where the endpoint_handler handles incoming requests
        let trace_processor = self.trace_processor.clone();
        let make_svc = make_service_fn(move |_| {
            let trace_processor = trace_processor.clone();
            let tx = tx.clone();

            let service = service_fn(move |req| {
                MiniAgent::endpoint_handler(req, trace_processor.clone(), tx.clone())
            });

            async move { Ok::<_, Infallible>(service) }
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], MINI_AGENT_PORT as u16));
        let server = Server::bind(&addr).serve(make_svc);

        // start hyper http server
        if let Err(e) = server.await {
            println!("server error: {}", e);
        }

        Ok(())
    }

    async fn endpoint_handler(
        req: Request<Body>,
        trace_processor: Box<dyn trace_processor::TraceProcessor + Send + Sync>,
        tx: Sender<Vec<Vec<pb::Span>>>,
    ) -> Result<Response<Body>, Infallible> {
        match (req.method(), req.uri().path()) {
            (&Method::PUT, TRACE_ENDPOINT_PATH) => {
                match trace_processor.process_traces(req, tx).await {
                    Ok(res) => Ok(res),
                    Err(err) => Ok(Response::new(Body::from(format!(
                        "error processing traces: {}",
                        err
                    )))),
                }
            }
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }
        }
    }
}
