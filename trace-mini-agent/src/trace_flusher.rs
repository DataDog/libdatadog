use async_trait::async_trait;
use dyn_clone::DynClone;

use datadog_trace_utils::trace_utils;

#[async_trait]
pub trait TraceFlusher: DynClone {
    fn flush_traces(&self, trace: Vec<trace_utils::Span>);
}
dyn_clone::clone_trait_object!(TraceFlusher);

#[derive(Clone)]
pub struct ServerlessTraceFlusher {}

impl TraceFlusher for ServerlessTraceFlusher {
    fn flush_traces(&self, trace: Vec<trace_utils::Span>) {
        let mut protobuf_trace = trace_utils::convert_to_pb_trace(trace);

        trace_utils::add_enclosing_span(&mut protobuf_trace);

        let agent_payload = trace_utils::construct_agent_payload(protobuf_trace);

        println!("spans: {:#?}", agent_payload);

        let serialized_agent_payload = trace_utils::serialize_agent_payload(agent_payload);

        match trace_utils::send(serialized_agent_payload) {
            Ok(_) => {}
            Err(e) => {
                panic!("Error sending trace: {:?}", e);
            }
        }
    }
}
