use std::{collections::HashMap, str::FromStr};

use hyper::http::uri::PathAndQuery;

// https://trace.agent.datadoghq.com/api/v0.2/traces

#[derive(Debug)]
pub struct Config {
    src: HashMap<String, String>,
}
#[derive(Debug)]
pub enum TracingProtocol {
    BackendProtobufV01,
    AgentV04,
}

#[derive(Debug)]
pub struct TracingConfig {
    pub url: String,
    pub http_headers: HashMap<String, String>,
    pub protocol: TracingProtocol,
}

pub struct SystemInfo {
    pub hostname: String,
    pub env: String,
}

impl Config {
    pub fn system_info(&self) -> SystemInfo {
        let hostname = self
            .get_str("DD_HOSTNAME")
            .unwrap_or("todohostname")
            .to_string(); // TODO: fetch host hostname
        let env = self.get_str("DD_ENV").unwrap_or("default").to_string(); // TODO: ?
        SystemInfo { hostname, env }
    }
    pub fn tracing_config(&self) -> TracingConfig {
        let api_key = self.src.get("DD_API_KEY");
        let serverless_preferred = !self.get_bool("DD_DISABLE_SERVERLESS").unwrap_or(false);
        let mut http_headers: HashMap<String, String> = HashMap::new();

        match (serverless_preferred, api_key) {
            (true, Some(api_key)) => {
                http_headers.insert("DD-API-KEY".into(), api_key.into());
                TracingConfig {
                    url: "https://trace.agent.datadoghq.com/api/v0.2/traces".into(),
                    protocol: TracingProtocol::BackendProtobufV01,
                    http_headers,
                }
            }
            _ => {
                let agent_url = self
                    .get_str("DD_TRACE_AGENT_URL")
                    .and_then(|p| hyper::Uri::from_str(p).ok())
                    .unwrap_or(hyper::Uri::from_static("http://127.0.0.1:8126/"));

                let mut parts = agent_url.into_parts();
                parts.path_and_query = Some(PathAndQuery::from_static("/v0.4/traces"));

                let url = hyper::Uri::from_parts(parts).unwrap().to_string();

                TracingConfig {
                    protocol: TracingProtocol::AgentV04,
                    url,
                    http_headers,
                }
            }
        }
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.src.get(key).and_then(|s| {
            s.parse::<bool>()
                .ok()
                .or_else(|| s.parse::<u8>().ok().filter(|n| *n <= 1).map(|n| n == 1))
        })
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.src
            .get(key)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    }

    pub fn init() -> Self {
        Self {
            src: std::env::vars().collect(),
        }
    }
}
