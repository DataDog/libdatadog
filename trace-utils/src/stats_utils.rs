// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_protobuf::pb;
use ddcommon::Endpoint;
use flate2::{write::GzEncoder, Compression};
use hyper::{body::Buf, Body, Method, Request};
use log::debug;
use std::io::Write;

pub async fn get_stats_from_request_body(body: Body) -> anyhow::Result<pb::ClientStatsPayload> {
    let buffer = hyper::body::aggregate(body).await?;

    let client_stats_payload: pb::ClientStatsPayload = match rmp_serde::from_read(buffer.reader()) {
        Ok(res) => res,
        Err(err) => {
            anyhow::bail!("Error deserializing stats from request body: {err}")
        }
    };

    if client_stats_payload.stats.is_empty() {
        debug!("Empty trace stats payload received");
        anyhow::bail!("No stats in stats payload");
    }
    Ok(client_stats_payload)
}

pub fn construct_stats_payload(stats: Vec<pb::ClientStatsPayload>) -> pb::StatsPayload {
    pb::StatsPayload {
        agent_hostname: "".to_string(),
        agent_env: "".to_string(),
        stats,
        agent_version: "".to_string(),
        client_computed: true,
        split_payload: false,
    }
}

pub fn create_stats_request(
    payload: pb::StatsPayload,
    target: &Endpoint,
    api_key: &str,
) -> anyhow::Result<Request<Body>> {
    let msgpack = rmp_serde::to_vec_named(&payload)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&msgpack)?;
    let data = match encoder.finish() {
        Ok(res) => res,
        Err(e) => anyhow::bail!("Error serializing stats payload: {e}"),
    };

    Ok(Request::builder()
        .method(Method::POST)
        .uri(target.url.clone())
        .header("Content-Type", "application/msgpack")
        .header("Content-Encoding", "gzip")
        .header("DD-API-KEY", api_key)
        .body(Body::from(data))?)
}
