use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;

use prost::Message;

use datadog_trace_protobuf::pb;

use datadog_trace_utils::trace_sender;
use datadog_trace_utils::trace_type_converter;

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A `Service` is needed for every connection, so this
    // creates one from our `hello_world` function.
    let make_svc = make_service_fn(|_conn| async {
        // service_fn converts our function into a `Service`
        Ok::<_, Infallible>(service_fn(endpoint_handler))
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], 8126));

    let server = Server::bind(&addr).serve(make_svc);

    println!("Listening on http://{}", addr);

    server.await?;

    Ok(())
}

async fn endpoint_handler(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    match (req.method(), req.uri().path()) {
        (&Method::PUT, "/v0.4/traces") => {
            let res = traces(req).await;
            Ok(res)
        }
        // Return the 404 Not Found for other routes.
        _ => {
            let mut not_found = Response::default();
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

fn construct_agent_payload(spans: Vec<pb::Span>) -> pb::AgentPayload {
    let chunks = vec![pb::TraceChunk {
        priority: 1,
        origin: "ffi-origin".to_string(),
        spans,
        tags: HashMap::new(),
        dropped_trace: false,
    }];

    let tracer_payloads = vec![pb::TracerPayload {
        app_version: "mini-agent-1.0.0".to_string(),
        language_name: "mini-agent-nodejs".to_string(),
        container_id: "mini-agent-containerid".to_string(),
        chunks,
        env: "mini-agent-env".to_string(),
        hostname: "mini-agent-hostname".to_string(),
        language_version: "mini-agent-nodejs-version".to_string(),
        runtime_id: "mini-agent-runtime-id".to_string(),
        tags: HashMap::new(),
        tracer_version: "tracer-v-1".to_string(),
    }];

    pb::AgentPayload {
        host_name: "ffi-test-hostname".to_string(),
        env: "ffi-test-env".to_string(),
        agent_version: "ffi-agent-version".to_string(),
        error_tps: 60.0,
        target_tps: 60.0,
        tags: HashMap::new(),
        tracer_payloads,
    }
}

pub fn serialize_agent_payload(payload: &pb::AgentPayload) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.reserve(payload.encoded_len());
    payload.encode(&mut buf).unwrap();
    buf
}

async fn traces(req: Request<Body>) -> Response<Body> {
    println!("in post_trace");

    let spans = trace_type_converter::deserialize_trace_from_hyper_req_body(req).await;

    let protobuf_spans = trace_type_converter::convert_to_pb_trace(spans);

    let agent_payload = construct_agent_payload(protobuf_spans);

    println!("spans: {:#?}", agent_payload);

    let serialized_agent_payload = serialize_agent_payload(&agent_payload);

    match trace_sender::send(serialized_agent_payload) {
        Ok(_) => {}
        Err(e) => {
            panic!("Error sending trace: {:?}", e);
        }
    }

    Response::builder().body(Body::default()).unwrap()
}
