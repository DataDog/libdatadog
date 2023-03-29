// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use datadog_trace_mini_agent::{mini_agent, trace_flusher, trace_processor};

pub fn main() {
    let trace_flusher = Box::new(trace_flusher::ServerlessTraceFlusher {});

    let trace_processor = Box::new(trace_processor::ServerlessTraceProcessor {});

    let mini_agent = Box::new(mini_agent::MiniAgent {
        trace_processor,
        trace_flusher,
    });

    match mini_agent.start_mini_agent() {
        Ok(_) => (),
        Err(e) => {
            println!("error when starting serverless mini agent: {}", e)
        }
    };
}
