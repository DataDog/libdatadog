use std::borrow::Cow;
use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use datadog_trace_protobuf::pb::{TracerPayload, AgentPayload};
use datadog_trace_protobuf::prost::Message;
use hyper::service::{make_service_fn, service_fn, Service};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use ddcommon::HttpClient;

use tokio::net::UnixListener;
use tokio::sync::mpsc::Sender;

use crate::connections::UnixListenerTracked;
use crate::data::v04::{self};

// Example traced app: go install github.com/DataDog/trace-examples/go/heartbeat@latest
#[derive(Debug, Clone)]
struct V04Handler {
    builder: v04::AssemblerBuilder,
    payload_sender: Sender<TracerPayload>,
}

impl V04Handler {
    fn new(tx: Sender<TracerPayload>) -> Self {
        Self {
            builder: Default::default(),
            payload_sender: tx,
        }
    }
}

#[derive(Debug)]
struct MiniAgent {
    v04_handler: V04Handler,
}

impl MiniAgent {
    fn new(tx: Sender<TracerPayload>) -> Self {
        Self {
            v04_handler: V04Handler::new(tx.clone()),
        }
    }
}

impl Service<Request<Body>> for MiniAgent {
    type Response = Response<Body>;
    type Error = anyhow::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        match (req.method(), req.uri().path()) {
            // exit, shutting down the subprocess process.
            (&Method::GET, "/exit") => {
                std::process::exit(0);
            }
            (&Method::POST, "/v0.4/traces") => {
                let handler = self.v04_handler.clone();

                Box::pin(async move { handler.handle(req).await })
            }

            // Return the 404 Not Found for other routes.
            _ => Box::pin(async move {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }),
        }
    }
}

impl V04Handler {
    async fn handle(&self, mut req: Request<Body>) -> anyhow::Result<Response<Body>> {
        let body = hyper::body::to_bytes(req.body_mut()).await?;
        let src: v04::Payload = rmp_serde::from_slice(&body)?;

        let payload = self
            .builder
            .with_headers(req.headers())
            .assemble_payload(src);

        self.payload_sender.send(payload).await?;

        Ok(Response::default())
    }
}

struct MiniAgentSpawner {
    payload_sender: Sender<TracerPayload>,
}

impl<'t, Target> Service<&'t Target> for MiniAgentSpawner {
    type Response = MiniAgent;
    type Error = Box<dyn Error + Send + Sync>;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: &'t Target) -> Self::Future {
        let agent = MiniAgent::new(self.payload_sender.clone());

        Box::pin(async { Ok(agent) })
    }
}

struct Uploader {
    tracing_config: crate::config::TracingConfig,
    system_info: crate::config::SystemInfo,
    client: HttpClient

}

impl Uploader {
    fn init(cfg: &crate::config::Config) -> Self {
        let client = hyper::Client::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build(ddcommon::connector::Connector::new());

        Self {
            tracing_config: cfg.tracing_config(),
            system_info: cfg.system_info(),
            client: client,
        }
    }

    pub async fn submit(&self, payloads: Vec<TracerPayload>) -> anyhow::Result<()> {
        let req = match self.tracing_config.protocol {
            crate::config::TracingProtocol::BackendProtobufV01 => {
                let payload = AgentPayload {
                    host_name: self.system_info.hostname.clone(),
                    env: self.system_info.env.clone(),
                    tracer_payloads: payloads,
                    tags: HashMap::new(), //TODO: parse DD_TAGS
                    agent_version: "libdatadog".into(),
                    target_tps: 100.0,
                    error_tps: 100.0,
                };

                let mut req_builder = Request::builder()
                    .method(Method::POST)
                    .header("Content-Type", "application/x-protobuf")
                    .header("X-Datadog-Reported-Languages", "rust,TODO")
                    .uri(&self.tracing_config.url);
                    
                for (key, value) in &self.tracing_config.http_headers {
                    req_builder = req_builder.header(key, value);
                }
                let data = payload.encode_to_vec();

                req_builder.body(data.into())?
            },
            crate::config::TracingProtocol::AgentV04 => {
                let data: Vec<v04::Trace> = payloads.iter().flat_map(|p| p.chunks.iter().map(|c| c.into())).collect();
                let data = v04::Payload{
                    traces: data,
                };
                let data = serde_json::to_vec(&data)?;
                // let data = rmp_serde::to_vec(&data)?;


                let mut req_builder = Request::builder()
                    .method(Method::POST)
                    .header("Content-Type", "application/json")
                    .uri(&self.tracing_config.url);
                
                for (key, value) in &self.tracing_config.http_headers {
                    req_builder = req_builder.header(key, value);
                }
                req_builder.body(data.into())?
            },
        };
        eprintln!("\n\n\req: {:?}\n\n\n", req);

        let mut resp = self.client.request(req).await?;
        let data = hyper::body::to_bytes(resp.body_mut()).await?;
        eprintln!("\n\n\nresp: {:?} \n {}\n\n\n", resp, String::from_utf8(data.to_vec()).unwrap());
        

        Ok(())
    }
}

pub(crate) async fn main(listener: UnixListener) -> anyhow::Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TracerPayload>(1);
    let uploader = Uploader::init(&crate::config::Config::init());
    tokio::spawn(async move {
        let mut payloads = vec![];
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                // if there are no connections for 1 second, exit the main loop
                Some(d) = rx.recv() => {
                    payloads.push(d);
                }

                _ = interval.tick() => {
                    if payloads.len() == 0 {
                        continue
                    }
                    match uploader.submit(payloads.drain(..).collect()).await {
                        Ok(()) => {},
                        Err(e) => {eprintln!("q-----------------------------------------\n{:?}\n", e)}
                    }
                }
            }
        }
    });

    let listener = UnixListenerTracked::from(listener);
    let watcher = listener.watch();
    let server = Server::builder(listener).serve(MiniAgentSpawner { payload_sender: tx });
    tokio::select! {
        // if there are no connections for 1 second, exit the main loop
        _ = watcher.wait_for_no_instances(Duration::from_secs(1)) => {
            Ok(())
        }
        res = server => {
            res?;
            Ok(())
        }
    }
}
