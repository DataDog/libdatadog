// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use datadog_trace_protobuf::pb;
use dyn_clone::DynClone;

use datadog_trace_utils::trace_utils;
use tokio::sync::mpsc::Receiver;

const BUFFER_FLUSH_SIZE: usize = 3;

type Buffer = Arc<Mutex<Vec<Vec<pb::Span>>>>;

#[async_trait]
pub trait TraceFlusher: DynClone {
    /// Starts a trace flusher that listens for traces sent to the tokio mpsc Receiver,
    /// implementing flushing logic that calls flush_traces.
    async fn start_trace_flusher(&self, mut rx: Receiver<Vec<Vec<pb::Span>>>);
    /// Flushes traces to the Datadog trace intake.
    fn flush_traces(&self, traces: Vec<Vec<pb::Span>>);
}
dyn_clone::clone_trait_object!(TraceFlusher);

#[derive(Clone)]
pub struct ServerlessTraceFlusher {}

#[async_trait]
impl TraceFlusher for ServerlessTraceFlusher {
    async fn start_trace_flusher(&self, mut rx: Receiver<Vec<Vec<pb::Span>>>) {
        let buffer_handle: Buffer = Arc::new(Mutex::new(Vec::with_capacity(BUFFER_FLUSH_SIZE)));

        // receive traces from http endpoint handlers and add them to the buffer. flush if
        // the buffer gets to BUFFER_FLUSH_SIZE size.
        while let Some(mut traces) = rx.recv().await {
            let buffer_handle = buffer_handle.clone();
            let mut buffer = buffer_handle.lock().unwrap();

            buffer.append(&mut traces);
            if buffer.len() >= BUFFER_FLUSH_SIZE {
                println!("Attempting to flush buffer");
                self.flush_traces(buffer.to_vec());
                buffer.clear();
            }
        }
    }

    fn flush_traces(&self, traces: Vec<Vec<pb::Span>>) {
        let agent_payload = trace_utils::construct_agent_payload(traces);

        let serialized_agent_payload = trace_utils::serialize_agent_payload(agent_payload);

        match trace_utils::send(serialized_agent_payload) {
            Ok(_) => {}
            Err(e) => {
                println!("Error sending trace: {:?}", e);
            }
        }
    }
}
