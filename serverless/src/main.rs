// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use env_logger::{Builder, Env, Target};
use log::{error, info};
use std::sync::Arc;

use datadog_trace_mini_agent::{
    config, env_verifier, mini_agent, stats_flusher, stats_processor, trace_flusher,
    trace_processor,
};

pub fn main() {
    let env = Env::new().filter_or("DD_LOG_LEVEL", "info");
    Builder::from_env(env).target(Target::Stdout).init();

    info!("Starting serverless trace mini agent");

    let env_verifier = Arc::new(env_verifier::ServerlessEnvVerifier::default());

    let trace_flusher = Arc::new(trace_flusher::ServerlessTraceFlusher {});
    let trace_processor = Arc::new(trace_processor::ServerlessTraceProcessor {});

    let stats_flusher = Arc::new(stats_flusher::ServerlessStatsFlusher {});
    let stats_processor = Arc::new(stats_processor::ServerlessStatsProcessor {});

    let config = match config::Config::new() {
        Ok(c) => c,
        Err(e) => {
            error!("Error creating config on serverless trace mini agent startup: {e}");
            return;
        }
    };

    let mini_agent = Box::new(mini_agent::MiniAgent {
        config: Arc::new(config),
        env_verifier,
        trace_processor,
        trace_flusher,
        stats_processor,
        stats_flusher,
    });

    if let Err(e) = mini_agent.start_mini_agent() {
        error!("Error when starting serverless trace mini agent: {e}");
    }
}
