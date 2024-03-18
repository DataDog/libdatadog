use std::hash::Hash;
use std::str::FromStr;
use hyper::{Body, Client, Method, Uri};
use hyper::http::uri::PathAndQuery;
use serde::Serialize;
use uuid::Uuid;
use ddcommon::connector::Connector;
use ddcommon::Endpoint;
use crate::debugger_defs::DebuggerPayload;

pub const PROD_INTAKE_SUBDOMAIN: &str = "http-intake.logs";

const DIRECT_TELEMETRY_URL_PATH: &str = "/v1/input";
const AGENT_TELEMETRY_URL_PATH: &str = "/debugger/v1/input";

#[derive(Default)]
pub struct Config {
    pub endpoint: Option<Endpoint>,
}

impl Config {
    pub fn set_endpoint(&mut self, mut endpoint: Endpoint) -> anyhow::Result<()> {
        let mut uri_parts = endpoint.url.into_parts();
        if uri_parts.scheme.is_some() && uri_parts.scheme.as_ref().unwrap().as_str() != "file" {
            uri_parts.path_and_query = Some(PathAndQuery::from_static(
                if endpoint.api_key.is_some() {
                    DIRECT_TELEMETRY_URL_PATH
                } else {
                    AGENT_TELEMETRY_URL_PATH
                },
            ));
        }

        endpoint.url = Uri::from_parts(uri_parts)?;
        self.endpoint = Some(endpoint);
        Ok(())
    }
}

pub fn encode<S: Eq + Hash + Serialize>(data: Vec<DebuggerPayload<S>>) -> Vec<u8> {
    serde_json::to_vec(&data).unwrap()
}

pub async fn send(payload: &[u8], endpoint: &Endpoint) -> anyhow::Result<()> {
    let mut req = hyper::Request::builder()
        .header(
            hyper::header::USER_AGENT,
            concat!("Tracer/", env!("CARGO_PKG_VERSION")),
        )
        .header("Content-type", "application/json")
        .method(Method::POST);

    let mut url = endpoint.url.clone();
    if endpoint.api_key.is_some() {
        // TODO DD-REQUEST-ID header necessary?
        req = req.header("DD-EVP-ORIGIN", "agent-debugger");
        let mut parts = url.into_parts();
        let mut query = String::from(parts.path_and_query.unwrap().as_str());
        query.push_str("?ddtags=host:");
        query.push_str(""); // TODO hostname
        // TODO container tags and such
        parts.path_and_query = Some(PathAndQuery::from_str(&query)?);
        url = Uri::from_parts(parts)?;
    }
    //             "env:" + config.getEnv(),
    //             "version:" + config.getVersion(),
    //             "debugger_version:" + DDTraceCoreInfo.VERSION,
    //             "agent_version:" + DebuggerAgent.getAgentVersion(),
    //             "host_name:" + config.getHostName());

    // SAFETY: we ensure the reference exists across the request
    let req = req.uri(url).body(Body::from(unsafe { std::mem::transmute::<&[u8], &[u8]>(payload) }))?;

    match Client::builder()
        .build(Connector::default())
        .request(req)
        .await
    {
        Ok(response) => {
            if response.status().as_u16() >= 400  {
                let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                let response_body =
                    String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                anyhow::bail!("Server did not accept traces: {response_body}");
            }
            Ok(())
        }
        Err(e) => anyhow::bail!("Failed to send traces: {e}"),
    }
}

pub fn generate_new_id() -> Uuid {
    Uuid::new_v4()
}
