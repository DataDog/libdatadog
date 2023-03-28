use datadog_trace_mini_agent::{self, DefaultTraceProcessor, MiniAgent};

pub fn main() {

    let mini_agent = Box::new(MiniAgent {
        trace_processor: Box::new(DefaultTraceProcessor {}),
    });
    match mini_agent.start_mini_agent() {
        Ok(_) => (),
        Err(e) => {
            panic!("error when starting mini agent: {}", e)
        },
    };
}
