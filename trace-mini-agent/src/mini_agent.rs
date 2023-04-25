// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use hyper::service::{make_service_fn, service_fn};
use hyper::{http, Body, Method, Request, Response, Server, StatusCode};
use log::{error, info};
use serde_json::json;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::{trace_flusher, trace_processor};
use datadog_trace_protobuf::pb;

const MINI_AGENT_PORT: usize = 8126;
const TRACE_ENDPOINT_PATH: &str = "/v0.4/traces";
const INFO_ENDPOINT_PATH: &str = "/info";
const TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;

pub struct MiniAgent {
    pub trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
    pub trace_flusher: Arc<dyn trace_flusher::TraceFlusher + Send + Sync>,
}

impl MiniAgent {
    #[tokio::main]
    pub async fn start_mini_agent(&self) -> Result<(), Box<dyn std::error::Error>> {
        // setup a channel to send processed traces to our flusher tx is passed through each endpoint_handler
        // to the trace processor, which uses it to send de-serialized processed trace payloads to our trace flusher.
        let (tx, rx): (Sender<pb::TracerPayload>, Receiver<pb::TracerPayload>) =
            mpsc::channel(TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE);

        // start our trace flusher. receives trace payloads and handles buffering + deciding when to flush to backend.
        let trace_flusher = self.trace_flusher.clone();
        tokio::spawn(async move {
            let trace_flusher = trace_flusher.clone();
            trace_flusher.start_trace_flusher(rx).await;
        });

        // setup our hyper http server, where the endpoint_handler handles incoming requests
        let trace_processor = self.trace_processor.clone();
        let make_svc = make_service_fn(move |_| {
            let trace_processor = trace_processor.clone();
            let tx = tx.clone();

            let service = service_fn(move |req| {
                MiniAgent::trace_endpoint_handler(req, trace_processor.clone(), tx.clone())
            });

            async move { Ok::<_, Infallible>(service) }
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], MINI_AGENT_PORT as u16));
        let server_builder = Server::try_bind(&addr)?;

        let server = server_builder.serve(make_svc);

        info!("Mini Agent started: listening on port {MINI_AGENT_PORT}");

        // start hyper http server
        if let Err(e) = server.await {
            error!("Server error: {e}");
            return Err(e.into());
        }

        Ok(())
    }

    async fn trace_endpoint_handler(
        req: Request<Body>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        tx: Sender<pb::TracerPayload>,
    ) -> http::Result<Response<Body>> {
        match (req.method(), req.uri().path()) {
            (&Method::PUT, TRACE_ENDPOINT_PATH) => {
                match trace_processor.process_traces(req, tx).await {
                    Ok(res) => Ok(res),
                    Err(err) => {
                        let error_message = format!("Error processing traces: {err}");
                        let response_body = json!({ "message": error_message }).to_string();
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(response_body))
                    }
                }
            }
            (_, INFO_ENDPOINT_PATH) => match Self::info_handler() {
                Ok(res) => Ok(res),
                Err(err) => {
                    let error_message = format!("Info endpoint error: {err}");
                    let response_body = json!({ "message": error_message }).to_string();
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::from(response_body))
                }
            },
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }
        }
    }

    fn info_handler() -> http::Result<Response<Body>> {
        let response_json = json!(
            {
                "endpoints": [
                    TRACE_ENDPOINT_PATH,
                    INFO_ENDPOINT_PATH
                ],
                "client_drop_p0s": true,
            }
        );
        Response::builder()
            .status(200)
            .body(Body::from(response_json.to_string()))
    }
}
