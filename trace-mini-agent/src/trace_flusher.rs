// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use async_trait::async_trait;
use log::error;
use tokio::sync::mpsc::Receiver;

use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils;

const BUFFER_FLUSH_SIZE: usize = 1;

#[async_trait]
pub trait TraceFlusher {
    /// Starts a trace flusher that listens for trace payloads sent to the tokio mpsc Receiver,
    /// implementing flushing logic that calls flush_traces.
    async fn start_trace_flusher(&self, mut rx: Receiver<pb::TracerPayload>);
    /// Flushes traces to the Datadog trace intake.
    async fn flush_traces(&self, traces: Vec<pb::TracerPayload>);
}

#[derive(Clone)]
pub struct ServerlessTraceFlusher {}

#[async_trait]
impl TraceFlusher for ServerlessTraceFlusher {
    async fn start_trace_flusher(&self, mut rx: Receiver<pb::TracerPayload>) {
        let mut buffer: Vec<pb::TracerPayload> = Vec::with_capacity(BUFFER_FLUSH_SIZE);

        // receive trace payloads from http endpoint handlers and add them to the buffer. flush if
        // the buffer gets to BUFFER_FLUSH_SIZE size.
        while let Some(tracer_payload) = rx.recv().await {
            buffer.push(tracer_payload);
            if buffer.len() >= BUFFER_FLUSH_SIZE {
                self.flush_traces(buffer.to_vec()).await;
                buffer.clear();
            }
        }
    }

    async fn flush_traces(&self, traces: Vec<pb::TracerPayload>) {
        if traces.is_empty() {
            return;
        }

        let agent_payload = trace_utils::construct_agent_payload(traces);
        let serialized_agent_payload = trace_utils::serialize_agent_payload(agent_payload);

        if let Err(e) = trace_utils::send(serialized_agent_payload).await {
            error!("Error sending trace: {:?}", e);
        }
    }
}
