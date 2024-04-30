// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use hyper::{Body, Client, Request, StatusCode};
use hyper_rustls::HttpsConnectorBuilder;
use log::{debug, error, info};
use std::{sync::Arc, time};
use tokio::sync::{mpsc::Receiver, Mutex};

use datadog_trace_protobuf::pb;
use datadog_trace_utils::stats_utils;

use crate::config::Config;

#[async_trait]
pub trait StatsFlusher {
    /// Starts a stats flusher that listens for stats payloads sent to the tokio mpsc Receiver,
    /// implementing flushing logic that calls flush_stats.
    async fn start_stats_flusher(
        &self,
        config: Arc<Config>,
        mut rx: Receiver<pb::ClientStatsPayload>,
    );
    /// Flushes stats to the Datadog trace stats intake.
    async fn flush_stats(&self, config: Arc<Config>, traces: Vec<pb::ClientStatsPayload>);
}

#[derive(Clone)]
pub struct ServerlessStatsFlusher {}

#[async_trait]
impl StatsFlusher for ServerlessStatsFlusher {
    async fn start_stats_flusher(
        &self,
        config: Arc<Config>,
        mut rx: Receiver<pb::ClientStatsPayload>,
    ) {
        let buffer: Arc<Mutex<Vec<pb::ClientStatsPayload>>> = Arc::new(Mutex::new(Vec::new()));

        let buffer_producer = buffer.clone();
        let buffer_consumer = buffer.clone();

        tokio::spawn(async move {
            while let Some(stats_payload) = rx.recv().await {
                let mut buffer = buffer_producer.lock().await;
                buffer.push(stats_payload);
            }
        });

        loop {
            tokio::time::sleep(time::Duration::from_secs(config.stats_flush_interval)).await;

            let mut buffer = buffer_consumer.lock().await;
            if !buffer.is_empty() {
                self.flush_stats(config.clone(), buffer.to_vec()).await;
                buffer.clear();
            }
        }
    }

    async fn flush_stats(&self, config: Arc<Config>, stats: Vec<pb::ClientStatsPayload>) {
        if stats.is_empty() {
            return;
        }
        info!("Flushing {} stats", stats.len());

        let stats_payload = stats_utils::construct_stats_payload(stats);

        debug!("Stats payload to be sent: {stats_payload:?}");

        let stats_request = match stats_utils::create_stats_request(
            stats_payload,
            &config.trace_stats_intake,
            config.trace_stats_intake.api_key.as_ref().unwrap(),
        ) {
            Ok(req) => req,
            Err(err) => {
                error!("Failed to serialize stats payload, dropping stats: {err}");
                return;
            }
        };

        match send_stats_payload(stats_request).await {
            Ok(_) => info!("Successfully flushed stats"),
            Err(e) => {
                error!("Error sending stats: {e:?}")
            }
        }
    }
}

pub async fn send_stats_payload(req: Request<Body>) -> anyhow::Result<()> {
    let connector = HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_only()
        .enable_http1()
        .build();

    let client: Client<_, hyper::Body> = Client::builder().build(connector);
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
