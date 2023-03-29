use datadog_trace_mini_agent::{mini_agent, trace_flusher, trace_processor};

pub fn main() {
    let trace_processor = Box::new(trace_processor::ServerlessTraceProcessor {
        trace_flusher: Box::new(trace_flusher::ServerlessTraceFlusher {}),
    });

    let mini_agent = Box::new(mini_agent::MiniAgent { trace_processor });

    match mini_agent.start_mini_agent() {
        Ok(_) => (),
        Err(e) => {
            panic!("error when starting mini agent: {}", e)
        }
    };
}
