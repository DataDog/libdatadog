// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use env_logger::Env;
use log::{error, info};
use std::sync::Arc;

use datadog_trace_mini_agent::{mini_agent, trace_flusher, trace_processor};

pub fn main() {
    let env = Env::new().filter_or("DD_LOG_LEVEL", "info");

    env_logger::init_from_env(env);

    info!("Starting serverless trace mini agent");

    let trace_flusher = Arc::new(trace_flusher::ServerlessTraceFlusher {});

    let trace_processor = Arc::new(trace_processor::ServerlessTraceProcessor {});

    let mini_agent = Box::new(mini_agent::MiniAgent {
        trace_processor,
        trace_flusher,
    });

    if let Err(e) = mini_agent.start_mini_agent() {
        error!("error when starting serverless mini agent: {}", e);
    }
}
