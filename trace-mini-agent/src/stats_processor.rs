// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use ddcommon::hyper_migration;
use hyper::{http, StatusCode};
use tokio::sync::mpsc::Sender;
use tracing::debug;

use datadog_trace_protobuf::pb;
use datadog_trace_utils::stats_utils;

use crate::config::Config;
use crate::http_utils::{self, log_and_create_http_response};

#[async_trait]
pub trait StatsProcessor {
    /// Deserializes trace stats from a hyper request body and sends them through
    /// the provided tokio mpsc Sender.
    async fn process_stats(
        &self,
        config: Arc<Config>,
        req: hyper_migration::HttpRequest,
        tx: Sender<pb::ClientStatsPayload>,
    ) -> http::Result<hyper_migration::HttpResponse>;
}

#[derive(Clone)]
pub struct ServerlessStatsProcessor {}

#[async_trait]
impl StatsProcessor for ServerlessStatsProcessor {
    async fn process_stats(
        &self,
        config: Arc<Config>,
        req: hyper_migration::HttpRequest,
        tx: Sender<pb::ClientStatsPayload>,
    ) -> http::Result<hyper_migration::HttpResponse> {
        debug!("Received trace stats to process");
        let (parts, body) = req.into_parts();

        if let Some(response) = http_utils::verify_request_content_length(
            &parts.headers,
            config.max_request_content_length,
            "Error processing trace stats",
        ) {
            return response;
        }

        // deserialize trace stats from the request body, convert to protobuf structs (see
        // trace-protobuf crate)
        let mut stats: pb::ClientStatsPayload =
            match stats_utils::get_stats_from_request_body(body).await {
                Ok(res) => res,
                Err(err) => {
                    return log_and_create_http_response(
                        &format!("Error deserializing trace stats from request body: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

        if !stats.stats.is_empty() {
            let timestamp = UNIX_EPOCH.elapsed().unwrap_or_default().as_nanos();
            stats.stats[0].start = timestamp as u64;
        }

        // send trace payload to our trace flusher
        match tx.send(stats).await {
            Ok(_) => {
                return log_and_create_http_response(
                    "Successfully buffered stats to be flushed.",
                    StatusCode::ACCEPTED,
                );
            }
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error sending stats to the stats flusher: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    }
}
