use base64::Engine;
use datadog_trace_protobuf::remoteconfig::{
    ClientAgent, ClientGetConfigsRequest, ClientGetConfigsResponse, ClientState, ClientTracer,
};
use ddcommon::{connector, parse_uri, Endpoint};
use http::header::CONTENT_TYPE;
use http::StatusCode;
use hyper::{Body, Client};
use std::io;

pub fn main() {
    async fn f() -> anyhow::Result<()> {
        let endpoint = Endpoint {
            url: parse_uri("http://staging_agent:8126/v0.7/config").unwrap(),
            api_key: None,
        };
        let config_req = ClientGetConfigsRequest {
            client: Some(datadog_trace_protobuf::remoteconfig::Client {
                state: Some(ClientState {
                    root_version: 1,
                    targets_version: 0,
                    config_states: vec![],
                    has_error: false,
                    error: "".to_string(),
                    backend_client_state: vec![],
                }),
                id: "globally unique id 2".to_string(),
                products: vec!["LIVE_DEBUGGING".to_string()],
                is_tracer: true,
                client_tracer: Some(ClientTracer {
                    runtime_id: "d83e3978-8737-4845-88b8-141b210fa3ba".to_string(),
                    language: "php".to_string(),
                    tracer_version: "0.90.0".to_string(),
                    service: "x.php".to_string(),
                    env: "None".to_string(),
                    extra_services: vec!["y.php".to_string()],
                    app_version: "".to_string(),
                    tags: vec![],
                }),
                is_agent: false,
                client_agent: None,
                last_seen: 0,
                capabilities: vec![],
            }),
            cached_target_files: vec![],
        };
        let json = serde_json::to_string(&config_req)?;

        let req = endpoint
            .into_request_builder(concat!("Sidecar/", env!("CARGO_PKG_VERSION")))?
            .header(CONTENT_TYPE, ddcommon::header::APPLICATION_JSON);
        let response = Client::builder()
            .build(connector::Connector::default())
            .request(req.body(Body::from(json))?)
            .await?;
        let status = response.status();
        let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
        if status != StatusCode::OK {
            let response_body = String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
            anyhow::bail!("Server did not accept traces: {response_body}");
        }

        println!("{}", &String::from_utf8_lossy(body_bytes.as_ref()));
        let res: ClientGetConfigsResponse =
            serde_json::from_str(&String::from_utf8_lossy(body_bytes.as_ref())).unwrap();
        println!(
            "{}",
            String::from_utf8_lossy(
                base64::engine::general_purpose::STANDARD
                    .decode(res.targets.as_slice())?
                    .as_slice()
            )
        );
        for f in res.target_files {
            println!("filepath: {}", f.path);
            println!(
                "{}",
                String::from_utf8_lossy(
                    base64::engine::general_purpose::STANDARD
                        .decode(f.raw.as_slice())?
                        .as_slice()
                )
            );
        }
        println!("configs: {}", res.client_configs.join(", "));
        Ok(())
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _g = runtime.enter();
    runtime.block_on(f()).unwrap();
}
