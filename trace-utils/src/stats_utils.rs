// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use flate2::{write::GzEncoder, Compression};
use hyper::{body::Buf, Body, Client, Method, Request, StatusCode};
use hyper_rustls::HttpsConnectorBuilder;
use log::debug;
use std::{env, io::Write};

use datadog_trace_protobuf::pb;

const STATS_INTAKE_URL: &str = "https://trace.agent.datadoghq.com/api/v0.2/stats";

pub async fn get_stats_from_request_body(body: Body) -> anyhow::Result<pb::ClientStatsPayload> {
    let buffer = hyper::body::aggregate(body).await.unwrap();

    let client_stats_payload: pb::ClientStatsPayload = match rmp_serde::from_read(buffer.reader()) {
        Ok(res) => res,
        Err(err) => {
            anyhow::bail!("Error deserializing stats from request body: {}", err)
        }
    };

    if client_stats_payload.stats.is_empty() {
        debug!("Empty trace stats payload received");
        anyhow::bail!("No stats in stats payload");
    }
    Ok(client_stats_payload)
}

pub fn construct_stats_payload(stats: pb::ClientStatsPayload) -> pb::StatsPayload {
    pb::StatsPayload {
        agent_hostname: "".to_string(),
        agent_env: "".to_string(),
        stats: vec![stats],
        agent_version: "".to_string(),
        client_computed: true,
    }
}

pub fn serialize_stats_payload(payload: pb::StatsPayload) -> anyhow::Result<Vec<u8>> {
    let msgpack = rmp_serde::to_vec_named(&payload)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&msgpack)?;
    match encoder.finish() {
        Ok(res) => Ok(res),
        Err(e) => anyhow::bail!("Error serializing stats payload: {}", e),
    }
}

pub async fn send_stats_payload(data: Vec<u8>) -> anyhow::Result<()> {
    let api_key = match env::var("DD_API_KEY") {
        Ok(key) => key,
        Err(_) => anyhow::bail!("Sending trace stats failed. Missing DD_API_KEY"),
    };

    let req = Request::builder()
        .method(Method::POST)
        .uri(STATS_INTAKE_URL)
        .header("Content-Type", "application/msgpack")
        .header("Content-Encoding", "gzip")
        .header("DD-API-KEY", &api_key)
        .header("X-Datadog-Reported-Languages", "nodejs")
        .body(Body::from(data.clone()))?;

    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_only()
        .enable_http1()
        .build();
    let client: Client<_, hyper::Body> = Client::builder().build(https);
    match client.request(req).await {
        Ok(response) => {
            if response.status() != StatusCode::ACCEPTED {
                let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                let response_body = String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                anyhow::bail!("Server did not accept trace stats: {}", response_body);
            }
            Ok(())
        }
        Err(e) => anyhow::bail!("Failed to send trace stats: {}", e),
    }
}
