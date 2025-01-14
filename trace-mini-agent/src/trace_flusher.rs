// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use std::{sync::Arc, time};
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::{debug, error};

use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::SendData;

use crate::config::Config;

#[async_trait]
pub trait TraceFlusher {
    /// Starts a trace flusher that listens for trace payloads sent to the tokio mpsc Receiver,
    /// implementing flushing logic that calls flush_traces.
    async fn start_trace_flusher(&self, config: Arc<Config>, mut rx: Receiver<SendData>);
    /// Flushes traces to the Datadog trace intake.
    async fn flush_traces(&self, traces: Vec<SendData>, config: Arc<Config>);
}

#[derive(Clone)]
pub struct ServerlessTraceFlusher {}

#[async_trait]
impl TraceFlusher for ServerlessTraceFlusher {
    async fn start_trace_flusher(&self, config: Arc<Config>, mut rx: Receiver<SendData>) {
        let buffer: Arc<Mutex<Vec<SendData>>> = Arc::new(Mutex::new(Vec::new()));

        let buffer_producer = buffer.clone();
        let buffer_consumer = buffer.clone();

        tokio::spawn(async move {
            while let Some(tracer_payload) = rx.recv().await {
                let mut buffer = buffer_producer.lock().await;
                buffer.push(tracer_payload);
            }
        });

        loop {
            tokio::time::sleep(time::Duration::from_secs(config.trace_flush_interval)).await;

            let mut buffer = buffer_consumer.lock().await;
            if !buffer.is_empty() {
                self.flush_traces(buffer.to_vec(), config.clone()).await;
                buffer.clear();
            }
        }
    }

    async fn flush_traces(&self, traces: Vec<SendData>, config: Arc<Config>) {
        if traces.is_empty() {
            return;
        }
        debug!("Flushing {} traces", traces.len());

        for traces in trace_utils::coalesce_send_data(traces) {
            match traces
                .send_proxy(config.proxy_url.as_deref())
                .await
                .last_result
            {
                Ok(_) => debug!("Successfully flushed traces"),
                Err(e) => {
                    error!("Error sending trace: {e:?}")
                    // TODO: Retries
                }
            }
        }
    }
}
