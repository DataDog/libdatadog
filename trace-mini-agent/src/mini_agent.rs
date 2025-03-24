// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::hyper_migration;
use hyper::service::service_fn;
use hyper::{http, Method, Response, StatusCode};
use serde_json::json;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{debug, error};

use crate::http_utils::log_and_create_http_response;
use crate::{config, env_verifier, stats_flusher, stats_processor, trace_flusher, trace_processor};
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::SendData;

const MINI_AGENT_PORT: usize = 8126;
const TRACE_ENDPOINT_PATH: &str = "/v0.4/traces";
const STATS_ENDPOINT_PATH: &str = "/v0.6/stats";
const INFO_ENDPOINT_PATH: &str = "/info";
const TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;
const STATS_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;

pub struct MiniAgent {
    pub config: Arc<config::Config>,
    pub trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
    pub trace_flusher: Arc<dyn trace_flusher::TraceFlusher + Send + Sync>,
    pub stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
    pub stats_flusher: Arc<dyn stats_flusher::StatsFlusher + Send + Sync>,
    pub env_verifier: Arc<dyn env_verifier::EnvVerifier + Send + Sync>,
}

impl MiniAgent {
    pub async fn start_mini_agent(&self) -> Result<(), Box<dyn std::error::Error>> {
        let now = Instant::now();

        // verify we are in a google cloud funtion environment. if not, shut down the mini agent.
        let mini_agent_metadata = Arc::new(
            self.env_verifier
                .verify_environment(
                    self.config.verify_env_timeout,
                    &self.config.env_type,
                    &self.config.os,
                )
                .await,
        );

        debug!(
            "Time taken to fetch Mini Agent metadata: {} ms",
            now.elapsed().as_millis()
        );

        // setup a channel to send processed traces to our flusher. tx is passed through each
        // endpoint_handler to the trace processor, which uses it to send de-serialized
        // processed trace payloads to our trace flusher.
        let (trace_tx, trace_rx): (Sender<SendData>, Receiver<SendData>) =
            mpsc::channel(TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE);

        // start our trace flusher. receives trace payloads and handles buffering + deciding when to
        // flush to backend.
        let trace_flusher = self.trace_flusher.clone();
        tokio::spawn(async move {
            let trace_flusher = trace_flusher.clone();
            trace_flusher.start_trace_flusher(trace_rx).await;
        });

        // channels to send processed stats to our stats flusher.
        let (stats_tx, stats_rx): (
            Sender<pb::ClientStatsPayload>,
            Receiver<pb::ClientStatsPayload>,
        ) = mpsc::channel(STATS_PAYLOAD_CHANNEL_BUFFER_SIZE);

        // start our stats flusher.
        let stats_flusher = self.stats_flusher.clone();
        let stats_config = self.config.clone();
        tokio::spawn(async move {
            let stats_flusher = stats_flusher.clone();
            stats_flusher
                .start_stats_flusher(stats_config, stats_rx)
                .await;
        });

        // setup our hyper http server, where the endpoint_handler handles incoming requests
        let trace_processor = self.trace_processor.clone();
        let stats_processor = self.stats_processor.clone();
        let endpoint_config = self.config.clone();

        let service = service_fn(move |req| {
            let trace_processor = trace_processor.clone();
            let trace_tx = trace_tx.clone();

            let stats_processor = stats_processor.clone();
            let stats_tx = stats_tx.clone();

            let endpoint_config = endpoint_config.clone();
            let mini_agent_metadata = Arc::clone(&mini_agent_metadata);

            MiniAgent::trace_endpoint_handler(
                endpoint_config.clone(),
                req.map(hyper_migration::Body::incoming),
                trace_processor.clone(),
                trace_tx.clone(),
                stats_processor.clone(),
                stats_tx.clone(),
                Arc::clone(&mini_agent_metadata),
            )
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], MINI_AGENT_PORT as u16));
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        debug!("Mini Agent started: listening on port {MINI_AGENT_PORT}");
        debug!(
            "Time taken start the Mini Agent: {} ms",
            now.elapsed().as_millis()
        );
        let server = hyper::server::conn::http1::Builder::new();
        let mut joinset = tokio::task::JoinSet::new();
        loop {
            let conn = tokio::select! {
                con_res = listener.accept() => match con_res {
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::ConnectionAborted
                                | io::ErrorKind::ConnectionReset
                                | io::ErrorKind::ConnectionRefused
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => {
                        error!("Server error: {e}");
                        return Err(e.into());
                    }
                    Ok((conn, _)) => conn,
                },
                finished = async {
                    match joinset.join_next().await {
                        Some(finished) => finished,
                        None => std::future::pending().await,
                    }
                } => match finished {
                    Err(e) if e.is_panic() => {
                        std::panic::resume_unwind(e.into_panic());
                    },
                    Ok(()) | Err(_) => continue,
                },
            };
            let conn = hyper_util::rt::TokioIo::new(conn);
            let server = server.clone();
            let service = service.clone();
            joinset.spawn(async move {
                if let Err(e) = server.serve_connection(conn, service).await {
                    error!("Connection error: {e}");
                }
            });
        }
    }

    async fn trace_endpoint_handler(
        config: Arc<config::Config>,
        req: hyper_migration::HttpRequest,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendData>,
        stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
        stats_tx: Sender<pb::ClientStatsPayload>,
        mini_agent_metadata: Arc<trace_utils::MiniAgentMetadata>,
    ) -> http::Result<hyper_migration::HttpResponse> {
        match (req.method(), req.uri().path()) {
            (&Method::PUT | &Method::POST, TRACE_ENDPOINT_PATH) => {
                match trace_processor
                    .process_traces(config, req, trace_tx, mini_agent_metadata)
                    .await
                {
                    Ok(res) => Ok(res),
                    Err(err) => log_and_create_http_response(
                        &format!("Error processing traces: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                }
            }
            (&Method::PUT | &Method::POST, STATS_ENDPOINT_PATH) => {
                match stats_processor.process_stats(config, req, stats_tx).await {
                    Ok(res) => Ok(res),
                    Err(err) => log_and_create_http_response(
                        &format!("Error processing trace stats: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                }
            }
            (_, INFO_ENDPOINT_PATH) => match Self::info_handler(config.dd_dogstatsd_port) {
                Ok(res) => Ok(res),
                Err(err) => log_and_create_http_response(
                    &format!("Info endpoint error: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ),
            },
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }
        }
    }

    fn info_handler(dd_dogstatsd_port: u16) -> http::Result<hyper_migration::HttpResponse> {
        let response_json = json!(
            {
                "endpoints": [
                    TRACE_ENDPOINT_PATH,
                    STATS_ENDPOINT_PATH,
                    INFO_ENDPOINT_PATH
                ],
                "client_drop_p0s": true,
                "config": {
                    "statsd_port": dd_dogstatsd_port
                }
            }
        );
        Response::builder()
            .status(200)
            .body(hyper_migration::Body::from(response_json.to_string()))
    }
}
