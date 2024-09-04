// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use arc_swap::ArcSwapOption;

pub mod schema {
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct AgentInfo {
        pub state_hash: String,
        pub info: AgentInfoStruct,
    }

    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct AgentInfoStruct {
        pub version: String,
        pub git_commit: String,
        pub endpoints: Option<Vec<String>>,
        pub feature_flags: Option<Vec<String>>,
        pub client_drop_p0s: Option<bool>,
        pub span_meta_structs: Option<bool>,
        pub long_running_spans: Option<bool>,
        pub evp_proxy_allowed_headers: Option<Vec<String>>,
        pub config: Option<Config>,
        pub peer_tags: Option<Vec<String>>,
    }

    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct Config {
        pub default_env: Option<String>,
        pub target_tps: Option<f64>,
        pub max_eps: Option<f64>,
        pub receiver_port: Option<i32>,
        pub receiver_socket: Option<String>,
        pub connection_limit: Option<i32>,
        pub receiver_timeout: Option<i32>,
        pub max_request_bytes: Option<i64>,
        pub statsd_port: Option<i32>,
        pub max_memory: Option<f64>,
        pub max_cpu: Option<f64>,
        pub analyzed_spans_by_service: Option<HashMap<String, HashMap<String, f64>>>,
    }

    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct ObfuscationConfig {
        pub elastic_search: bool,
        pub mongo: bool,
        pub sql_exec_plan: bool,
        pub sql_exec_plan_normalize: bool,
        pub http: HttpObfuscationConfig,
        pub remove_stack_traces: bool,
        pub redis: RedisObfuscationConfig,
        pub memcached: MemcachedObfuscationConfig,
    }

    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct HttpObfuscationConfig {
        pub remove_query_string: bool,
        pub remove_path_digits: bool,
    }

    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct RedisObfuscationConfig {
        pub enabled: bool,
        pub remove_all_args: bool,
    }

    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct MemcachedObfuscationConfig {
        pub enabled: bool,
        pub keep_command: bool,
    }
}

mod fetcher {
    use crate::{
        schema::{AgentInfo, AgentInfoStruct},
        AgentInfoArc,
    };
    use anyhow::{anyhow, Result};
    use arc_swap::{access::Access, ArcSwapOption};
    use ddcommon::{connector::Connector, Endpoint};
    use hyper::{self, body::Buf, header::HeaderName};
    use serde_json;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::sleep;

    const DATADOG_AGENT_STATE: HeaderName = HeaderName::from_static("datadog-agent-state");

    #[derive(Debug)]
    enum FetchInfoStatus {
        SameState,
        NewState(AgentInfo),
    }

    async fn fetch_info(
        agent_endpoint: &Endpoint,
        current_state_hash: Option<&str>,
    ) -> Result<FetchInfoStatus> {
        // TODO: Do we need all the headers from Endpoint this may slow the request
        let req = agent_endpoint
            .into_request_builder(concat!("Libdatadog/", env!("CARGO_PKG_VERSION")))?
            .method(hyper::Method::GET)
            .body(hyper::Body::empty());
        let client = hyper::Client::builder().build(Connector::default());
        let res = client.request(req?).await?;
        let new_state_hash = res
            .headers()
            .get(DATADOG_AGENT_STATE)
            .ok_or_else(|| anyhow!("Agent state header not found"))?
            .to_str()?;
        if current_state_hash.is_some_and(|state| state == new_state_hash) {
            return Ok(FetchInfoStatus::SameState);
        }
        let state_hash = new_state_hash.to_string();
        let body_bytes = hyper::body::aggregate(res.into_body()).await?;
        let info: AgentInfoStruct = serde_json::from_reader(body_bytes.reader())?;
        Ok(FetchInfoStatus::NewState(AgentInfo { state_hash, info }))
    }

    pub struct AgentInfoFetcher {
        agent_endpoint: Endpoint,
        info: AgentInfoArc,
    }

    impl AgentInfoFetcher {
        pub fn new(agent_endpoint: Endpoint) -> Self {
            Self {
                agent_endpoint,
                info: Arc::new(ArcSwapOption::new(None)),
            }
        }

        pub async fn run(&self) {
            loop {
                let current_info = self.info.load();
                let current_hash = current_info.as_ref().map(|info| info.state_hash.as_str());
                if let Ok(FetchInfoStatus::NewState(new_info)) =
                    fetch_info(&self.agent_endpoint, current_hash).await
                {
                    self.info.store(Some(Arc::new(new_info)));
                }
                sleep(Duration::from_secs(60 * 5)).await; // Wait 5 min between each call to /info
            }
        }

        pub fn get_info(&self) -> AgentInfoArc {
            self.info.clone()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[tokio::test]
        async fn test_info() {
            let endpoint = Endpoint::from_url("http://localhost:8126/info".parse().unwrap());
            let info = fetch_info(&endpoint, None).await.unwrap();
            assert!(match info {
                FetchInfoStatus::NewState(_) => true,
                FetchInfoStatus::SameState => false,
            });
        }
    }
}

pub type AgentInfoArc = Arc<ArcSwapOption<schema::AgentInfo>>;

pub use fetcher::AgentInfoFetcher;

#[cfg(test)]
mod tests {}
