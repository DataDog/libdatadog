// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hyper::{http, Body, Request, Response};
use log::{error, info};
use serde_json::json;

use datadog_trace_protobuf::pb;
use datadog_trace_utils::stats_utils;

#[async_trait]
pub trait StatsProcessor {
    /// Deserializes traces from a hyper request body and sends them through
    /// the provided tokio mpsc Sender.
    async fn process_stats(&self, req: Request<Body>) -> http::Result<Response<Body>>;
}

#[derive(Clone)]
pub struct ServerlessStatsProcessor {}

#[async_trait]
impl StatsProcessor for ServerlessStatsProcessor {
    async fn process_stats(&self, req: Request<Body>) -> http::Result<Response<Body>> {
        let body = req.into_body();

        info!("Recieved trace stats to process");

        // deserialize trace stats from the request body, convert to protobuf structs (see trace-protobuf crate)
        let stats: pb::ClientStatsPayload =
            match stats_utils::get_stats_from_request_body(body).await {
                Ok(res) => res,
                Err(err) => {
                    let error_message =
                        format!("Error deserializing trace stats from request body: {}", err);
                    error!("{}", error_message);
                    let body = json!({ "message": error_message }).to_string();
                    return Response::builder().status(500).body(Body::from(body));
                }
            };

        let mut stats_payload = stats_utils::construct_stats_payload(stats);

        let start = SystemTime::now();
        let timestamp = start.duration_since(UNIX_EPOCH).unwrap().as_nanos();
        stats_payload.stats[0].stats[0].start = timestamp as u64;

        info!("StatsPayload: {:#?}", stats_payload);

        let data = match stats_utils::serialize_stats_payload(stats_payload) {
            Ok(res) => res,
            Err(err) => {
                let error_message = format!("Error serializing stats payload: {}", err);
                error!("{}", error_message);
                let body = json!({ "message": error_message }).to_string();
                return Response::builder().status(500).body(Body::from(body));
            }
        };

        if let Err(err) = stats_utils::send_stats_payload(data).await {
            let error_message = format!("Error sending trace stats: {}", err);
            error!("{}", error_message);
            let body = json!({ "message": error_message }).to_string();
            return Response::builder().status(500).body(Body::from(body));
        };

        info!("Successfully processed trace stats.");
        let body = json!({ "message": "Successfully processed trace stats." }).to_string();
        Response::builder().status(200).body(Body::from(body))
    }
}
