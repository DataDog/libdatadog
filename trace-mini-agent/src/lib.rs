use async_trait::async_trait;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use std::convert::Infallible;
use std::net::SocketAddr;

use dyn_clone::DynClone;

use datadog_trace_utils::trace_utils;

#[async_trait]
pub trait TraceProcessor: DynClone {
    async fn process_traces(&self, req: Request<Body>) -> Response<Body>;
}

dyn_clone::clone_trait_object!(TraceProcessor);

#[derive(Clone)]
pub struct DefaultTraceProcessor {
}

#[async_trait]
impl TraceProcessor for DefaultTraceProcessor {
    async fn process_traces(&self, req: Request<Body>) -> Response<Body> {
        println!("in post_trace");

        let spans = trace_utils::deserialize_trace_from_hyper_req_body(req).await;

        let mut protobuf_spans = trace_utils::convert_to_pb_trace(spans);

        trace_utils::add_enclosing_span(&mut protobuf_spans);

        let agent_payload = trace_utils::construct_agent_payload(protobuf_spans);

        println!("spans: {:#?}", agent_payload);

        let serialized_agent_payload = trace_utils::serialize_agent_payload(agent_payload);

        match trace_utils::send(serialized_agent_payload) {
            Ok(_) => {}
            Err(e) => {
                panic!("Error sending trace: {:?}", e);
            }
        }

        Response::builder().body(Body::default()).unwrap()
    }
}

pub struct MiniAgent {
    pub trace_processor: Box<dyn TraceProcessor + Send + Sync>
}

impl MiniAgent {
    #[tokio::main]
    pub async fn start_mini_agent(&self) -> Result<(), Box<dyn std::error::Error>> {

        let trace_processor = self.trace_processor.clone();

        let make_svc = make_service_fn(move |_conn| {
            let trace_processor = trace_processor.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req| MiniAgent::endpoint_handler(req, trace_processor.clone()))) 
            }
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], 8126));

        let server = Server::bind(&addr).serve(make_svc);

        println!("Listening on http://{}", addr);

        server.await?;

        Ok(())
    }

    async fn endpoint_handler(req: Request<Body>, trace_processor: Box<dyn TraceProcessor + Send + Sync>) -> Result<Response<Body>, Infallible> {
        match (req.method(), req.uri().path()) {
            (&Method::PUT, "/v0.4/traces") => {
                let res = trace_processor.process_traces(req).await;
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
}
