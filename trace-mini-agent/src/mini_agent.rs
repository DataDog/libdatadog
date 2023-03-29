// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use std::convert::Infallible;
use std::net::SocketAddr;

use crate::trace_processor;

pub struct MiniAgent {
    pub trace_processor: Box<dyn trace_processor::TraceProcessor + Send + Sync>,
}

impl MiniAgent {
    #[tokio::main]
    pub async fn start_mini_agent(&self) -> Result<(), Box<dyn std::error::Error>> {
        let trace_processor = self.trace_processor.clone();

        let make_svc = make_service_fn(move |_conn| {
            let trace_processor = trace_processor.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    MiniAgent::endpoint_handler(req, trace_processor.clone())
                }))
            }
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], 8126));

        let server = Server::bind(&addr).serve(make_svc);

        println!("Listening on http://{}", addr);

        server.await?;

        Ok(())
    }

    async fn endpoint_handler(
        req: Request<Body>,
        trace_processor: Box<dyn trace_processor::TraceProcessor + Send + Sync>,
    ) -> Result<Response<Body>, Infallible> {
        match (req.method(), req.uri().path()) {
            (&Method::PUT, "/v0.4/traces") => {
                let res = trace_processor.process_traces(req).await;
                Ok(res)
            }
            // Return the 404 Not Found for other routes.
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }
        }
    }
}
