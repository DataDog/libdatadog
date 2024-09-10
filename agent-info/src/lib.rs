// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Provides utilities to get config from the /info endpoint of an agent
#![deny(missing_docs)]

use std::sync::Arc;

use arc_swap::ArcSwapOption;

pub mod schema {
    //! This module provides struct representing the info endpoint response
    use serde::Deserialize;
    use std::collections::HashMap;

    /// Wrapper for an agent info response storing the state hash from the agent
    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct AgentInfo {
        /// Hash of the info
        pub state_hash: String,
        /// Info response from the agent
        pub info: AgentInfoStruct,
    }

    /// Schema of an agent info response
    #[allow(missing_docs)]
    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct AgentInfoStruct {
        /// Version of the agent
        pub version: Option<String>,
        /// Commit of the version of the agent
        pub git_commit: Option<String>,
        /// List of available endpoints
        pub endpoints: Option<Vec<String>>,
        /// List of feature flags
        pub feature_flags: Option<Vec<String>>,
        pub client_drop_p0s: Option<bool>,
        pub span_meta_structs: Option<bool>,
        pub long_running_spans: Option<bool>,
        pub evp_proxy_allowed_headers: Option<Vec<String>>,
        /// Configuration of the agent
        pub config: Option<Config>,
        /// List of keys mapped to peer tags
        pub peer_tags: Option<Vec<String>>,
    }

    #[allow(missing_docs)]
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

    #[allow(missing_docs)]
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

    #[allow(missing_docs)]
    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct HttpObfuscationConfig {
        pub remove_query_string: bool,
        pub remove_path_digits: bool,
    }

    #[allow(missing_docs)]
    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct RedisObfuscationConfig {
        pub enabled: bool,
        pub remove_all_args: bool,
    }

    #[allow(missing_docs)]
    #[derive(Clone, Deserialize, Default, Debug, PartialEq)]
    pub struct MemcachedObfuscationConfig {
        pub enabled: bool,
        pub keep_command: bool,
    }
}

mod fetcher {
    use crate::{schema::AgentInfo, AgentInfoArc};
    use anyhow::{anyhow, Result};
    use arc_swap::ArcSwapOption;
    use ddcommon::{connector::Connector, Endpoint};
    use hyper::{self, body::Buf, header::HeaderName};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::sleep;

    const DATADOG_AGENT_STATE: HeaderName = HeaderName::from_static("datadog-agent-state");

    #[derive(Debug)]
    enum FetchInfoStatus {
        SameState,
        NewState(Box<AgentInfo>),
    }

    /// Fetch info from the given info_endpoint and compare its state to the current state hash.
    ///
    /// If the state hash is different from the current one:
    /// - Return a `FetchInfoStatus::NewState` of the info struct
    /// - Else return `FetchInfoStatus::SameState`
    async fn fetch_info_with_state(
        info_endpoint: &Endpoint,
        current_state_hash: Option<&str>,
    ) -> Result<FetchInfoStatus> {
        let req = info_endpoint
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
        let info = Box::new(AgentInfo {
            state_hash,
            info: serde_json::from_reader(body_bytes.reader())?,
        });
        Ok(FetchInfoStatus::NewState(info))
    }

    /// Fetch the info endpoint once and return the info
    ///
    /// Can be used for one-time access to the agent's info. If you need to access the info over
    /// long period use `AgentInfoFetcher` to keep the info up-to-date.
    ///
    /// # Example
    /// ```no_run
    /// # use anyhow::Result;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// // Define the endpoint
    /// let endpoint = ddcommon::Endpoint::from_url("http://localhost:8126/info".parse().unwrap());
    /// // Fetch the info
    /// let agent_info = agent_info::fetch_info(&endpoint).await.unwrap();
    /// println!("Agent version is {}", agent_info.info.version.unwrap());
    /// # Ok(())
    /// # }
    /// ``
    pub async fn fetch_info(info_endpoint: &Endpoint) -> Result<Box<AgentInfo>> {
        match fetch_info_with_state(info_endpoint, None).await? {
            FetchInfoStatus::NewState(info) => Ok(info),
            FetchInfoStatus::SameState => Err(anyhow!("Invalid state header")),
        }
    }

    /// Fetch the info endpoint and update an ArcSwap based on a given time interval
    ///
    /// Once the fetcher has been created you can get an Arc of the config by calling `get_info`.
    /// You can then start the run method, the fetcher will update the AgentInfoArc based on the
    /// given refresh interval
    ///
    /// # Example
    /// ```no_run
    /// # use anyhow::Result;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// // Define the endpoint
    /// let endpoint = ddcommon::Endpoint::from_url("http://localhost:8126/info".parse().unwrap());
    /// // Create the fetcher
    /// let fetcher =
    ///     agent_info::AgentInfoFetcher::new(endpoint, std::time::Duration::from_secs(5 * 60));
    /// // Get the Arc to access the info
    /// let agent_info_arc = fetcher.get_info();
    /// // Start the runner
    /// tokio::spawn(async move {
    ///     fetcher.run().await;
    /// });
    ///
    /// // Access the info
    /// if let Some(agent_info) = agent_info_arc.load().as_ref() {
    ///     println!(
    ///         "Agent version is {}",
    ///         agent_info.info.version.as_ref().unwrap()
    ///     );
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub struct AgentInfoFetcher {
        info_endpoint: Endpoint,
        info: AgentInfoArc,
        refresh_interval: Duration,
    }

    impl AgentInfoFetcher {
        /// Return a new `AgentInfoFetcher` fetching the `info_endpoint` on each `refresh_interval`
        /// and updating the stored info
        pub fn new(info_endpoint: Endpoint, fetch_interval: Duration) -> Self {
            Self {
                info_endpoint,
                info: Arc::new(ArcSwapOption::new(None)),
                refresh_interval: fetch_interval,
            }
        }

        /// Start fetching the info endpoint with the given interval
        ///
        /// Warning: This method does not return and should be called within a dedicated task.
        pub async fn run(&self) {
            loop {
                let current_info = self.info.load();
                let current_hash = current_info.as_ref().map(|info| info.state_hash.as_str());
                let res = fetch_info_with_state(&self.info_endpoint, current_hash).await;
                if let Ok(FetchInfoStatus::NewState(new_info)) = res {
                    self.info.store(Some(Arc::new(*new_info)));
                }
                sleep(self.refresh_interval).await; // Wait 5 min between each call to /info
            }
        }

        /// Return an AgentInfoArc storing the info received by the agent.
        ///
        /// When the fetcher is running it updates the AgentInfoArc when the agent's info changes.
        pub fn get_info(&self) -> AgentInfoArc {
            self.info.clone()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use httpmock::prelude::*;

        const TEST_INFO: &str = r#"{
        "version": "0.0.0",
        "git_commit": "0101010",
        "endpoints": [
                "/v0.4/traces",
                "/v0.6/stats"
        ],
        "client_drop_p0s": true,
        "span_meta_structs": true,
        "long_running_spans": true,
        "evp_proxy_allowed_headers": [
                "Content-Type",
                "Accept-Encoding"
        ],
        "config": {
                "default_env": "none",
                "target_tps": 10,
                "max_eps": 200,
                "receiver_port": 8126,
                "receiver_socket": "",
                "connection_limit": 0,
                "receiver_timeout": 0,
                "max_request_bytes": 26214400,
                "statsd_port": 8125,
                "max_memory": 0,
                "max_cpu": 0,
                "analyzed_spans_by_service": {},
                "obfuscation": {
                        "elastic_search": true,
                        "mongo": true,
                        "sql_exec_plan": false,
                        "sql_exec_plan_normalize": false,
                        "http": {
                                "remove_query_string": false,
                                "remove_path_digits": false
                        },
                        "remove_stack_traces": false,
                        "redis": {
                                "Enabled": true,
                                "RemoveAllArgs": false
                        },
                        "memcached": {
                                "Enabled": true,
                                "KeepCommand": false
                        }
                }
        },
        "peer_tags": ["db.hostname","http.host","aws.s3.bucket"]
        }"#;

        const TEST_INFO_HASH: &str =
            "8c732aba385d605b010cd5bd12c03fef402eaefce989f0055aa4c7e92fe30077";

        #[tokio::test]
        async fn test_fetch_info_without_state() {
            let server = MockServer::start();
            let mock = server
                .mock_async(|when, then| {
                    when.path("/info");
                    then.status(200)
                        .header("content-type", "application/json")
                        .header(DATADOG_AGENT_STATE.to_string(), TEST_INFO_HASH)
                        .body(TEST_INFO);
                })
                .await;
            let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

            let info_status = fetch_info_with_state(&endpoint, None).await.unwrap();
            mock.assert();
            assert!(
                matches!(info_status, FetchInfoStatus::NewState(info) if *info == AgentInfo {
                            state_hash: TEST_INFO_HASH.to_string(),
                            info: serde_json::from_str(TEST_INFO).unwrap(),
                        }
                )
            );
        }

        #[tokio::test]
        async fn test_fetch_info_with_state() {
            let server = MockServer::start();
            let mock = server
                .mock_async(|when, then| {
                    when.path("/info");
                    then.status(200)
                        .header("content-type", "application/json")
                        .header(DATADOG_AGENT_STATE.to_string(), TEST_INFO_HASH)
                        .body(TEST_INFO);
                })
                .await;
            let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

            let new_state_info_status = fetch_info_with_state(&endpoint, Some("abbaabbaabbaabbaa"))
                .await
                .unwrap();
            let same_state_info_status = fetch_info_with_state(&endpoint, Some(TEST_INFO_HASH))
                .await
                .unwrap();

            mock.assert_hits(2);
            assert!(
                matches!(new_state_info_status, FetchInfoStatus::NewState(info) if *info == AgentInfo {
                            state_hash: TEST_INFO_HASH.to_string(),
                            info: serde_json::from_str(TEST_INFO).unwrap(),
                        }
                )
            );
            assert!(matches!(same_state_info_status, FetchInfoStatus::SameState));
        }

        #[tokio::test]
        async fn test_agent_info_fetcher_run() {
            let server = MockServer::start();
            let mock_v1 = server
                .mock_async(|when, then| {
                    when.path("/info");
                    then.status(200)
                        .header("content-type", "application/json")
                        .header(DATADOG_AGENT_STATE.to_string(), "1")
                        .body(r#"{"version":"1"}"#);
                })
                .await;
            let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());
            let fetcher = AgentInfoFetcher::new(endpoint.clone(), Duration::from_millis(100));
            let info = fetcher.get_info();
            assert!(info.load().is_none());
            tokio::spawn(async move {
                fetcher.run().await;
            });

            // Wait for first fetch
            while mock_v1.hits_async().await == 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let version_1 = info.load().as_ref().unwrap().info.version.clone().unwrap();
            assert_eq!(version_1, "1");

            // Update the info endpoint
            mock_v1.delete_async().await;
            let mock_v2 = server
                .mock_async(|when, then| {
                    when.path("/info");
                    then.status(200)
                        .header("content-type", "application/json")
                        .header(DATADOG_AGENT_STATE.to_string(), "2")
                        .body(r#"{"version":"2"}"#);
                })
                .await;

            // Wait for second fetch
            while mock_v2.hits_async().await == 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            let version_2 = info.load().as_ref().unwrap().info.version.clone().unwrap();
            assert_eq!(version_2, "2");
        }
    }
}

/// Stores an AgentInfo in an ArcSwap to be updated by an AgentInfoFetcher
pub type AgentInfoArc = Arc<ArcSwapOption<schema::AgentInfo>>;

pub use fetcher::{fetch_info, AgentInfoFetcher};
