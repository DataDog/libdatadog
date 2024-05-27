// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use flate2::{write::GzEncoder, Compression};
use hyper::{body::Buf, Body, Client, Method, Request, StatusCode};
use log::debug;
use std::io::Write;

use datadog_trace_protobuf::pb;
use ddcommon::connector::Connector;
use ddcommon::Endpoint;

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

pub fn serialize_stats_payload(payload: pb::StatsPayload) -> anyhow::Result<Vec<u8>> {
    let msgpack = rmp_serde::to_vec_named(&payload)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&msgpack)?;
    match encoder.finish() {
        Ok(res) => Ok(res),
        Err(e) => anyhow::bail!("Error serializing stats payload: {e}"),
    }
}

pub async fn send_stats_payload(
    data: Vec<u8>,
    target: &Endpoint,
    api_key: &str,
) -> anyhow::Result<()> {
    let req = Request::builder()
        .method(Method::POST)
        .uri(target.url.clone())
        .header("Content-Type", "application/msgpack")
        .header("Content-Encoding", "gzip")
        .header("DD-API-KEY", api_key)
        .body(Body::from(data.clone()))?;

    let client: Client<_, hyper::Body> = Client::builder().build(Connector::default());
    match client.request(req).await {
        Ok(response) => {
            if response.status() != StatusCode::ACCEPTED {
                let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                let response_body = String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                anyhow::bail!("Server did not accept trace stats: {response_body}");
            }
            Ok(())
        }
        Err(e) => anyhow::bail!("Failed to send trace stats: {e}"),
    }
}
