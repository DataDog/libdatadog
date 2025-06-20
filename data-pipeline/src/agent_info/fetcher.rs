// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Provides utilities to fetch the agent /info endpoint and an automatic fetcher to keep info
//! up-to-date

use super::{schema::AgentInfo, AGENT_INFO_CACHE};
use anyhow::{anyhow, Result};
use ddcommon::{hyper_migration, worker::Worker, Endpoint};
use http_body_util::BodyExt;
use hyper::{self, body::Buf, header::HeaderName};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info};

/// HTTP header containing the agent state hash.
const DATADOG_AGENT_STATE: HeaderName = HeaderName::from_static("datadog-agent-state");

/// Whether the agent reported the same value or not.
#[derive(Debug)]
pub enum FetchInfoStatus {
    /// Unchanged
    SameState,
    /// Has a new state
    NewState(Box<AgentInfo>),
}

/// Fetch info from the given info_endpoint and compare its state to the current state hash.
///
/// If the state hash is different from the current one:
/// - Return a `FetchInfoStatus::NewState` of the info struct
/// - Else return `FetchInfoStatus::SameState`
pub async fn fetch_info_with_state(
    info_endpoint: &Endpoint,
    current_state_hash: Option<&str>,
) -> Result<FetchInfoStatus> {
    let req = info_endpoint
        .to_request_builder(concat!("Libdatadog/", env!("CARGO_PKG_VERSION")))?
        .method(hyper::Method::GET)
        .body(hyper_migration::Body::empty());
    let client = hyper_migration::new_default_client();
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
    let body_bytes = res.into_body().collect().await?.aggregate();
    let info = Box::new(AgentInfo {
        state_hash,
        info: serde_json::from_reader(body_bytes.reader())?,
    });
    Ok(FetchInfoStatus::NewState(info))
}

/// Fetch the info endpoint once and return the info.
///
/// Can be used for one-time access to the agent's info. If you need to access the info several
/// times use `AgentInfoFetcher` to keep the info up-to-date.
///
/// # Example
/// ```no_run
/// # use anyhow::Result;
/// # #[tokio::main]
/// # async fn main() -> Result<()> {
/// // Define the endpoint
/// let endpoint = ddcommon::Endpoint::from_url("http://localhost:8126/info".parse().unwrap());
/// // Fetch the info
/// let agent_info = data_pipeline::agent_info::fetch_info(&endpoint)
///     .await
///     .unwrap();
/// println!("Agent version is {}", agent_info.info.version.unwrap());
/// # Ok(())
/// # }
/// ```
pub async fn fetch_info(info_endpoint: &Endpoint) -> Result<Box<AgentInfo>> {
    match fetch_info_with_state(info_endpoint, None).await? {
        FetchInfoStatus::NewState(info) => Ok(info),
        // Should never be reached since there is no previous state.
        FetchInfoStatus::SameState => Err(anyhow!("Invalid state header")),
    }
}

/// Fetch the info endpoint and update an ArcSwap keeping it up-to-date.
///
/// Once the fetcher has been created you can get an Arc of the config by calling `get_info`.
/// You can then start the run method, the fetcher will update the AgentInfoArc based on the
/// given refresh interval.
///
/// # Example
/// ```no_run
/// # use anyhow::Result;
/// # use ddcommon::worker::Worker;
/// # #[tokio::main]
/// # async fn main() -> Result<()> {
/// // Define the endpoint
/// use data_pipeline::agent_info;
/// let endpoint = ddcommon::Endpoint::from_url("http://localhost:8126/info".parse().unwrap());
/// // Create the fetcher
/// let mut fetcher = data_pipeline::agent_info::AgentInfoFetcher::new(
///     endpoint,
///     std::time::Duration::from_secs(5 * 60),
/// );
/// // Start the runner
/// tokio::spawn(async move {
///     fetcher.run().await;
/// });
///
/// // Get the Arc to access the info
/// let agent_info_arc = agent_info::get_agent_info();
///
/// // Access the info
/// if let Some(agent_info) = agent_info_arc.as_ref() {
///     println!(
///         "Agent version is {}",
///         agent_info.info.version.as_ref().unwrap()
///     );
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct AgentInfoFetcher {
    info_endpoint: Endpoint,
    refresh_interval: Duration,
}

impl AgentInfoFetcher {
    /// Return a new `AgentInfoFetcher` fetching the `info_endpoint` on each `refresh_interval`
    /// and updating the stored info.
    pub fn new(info_endpoint: Endpoint, refresh_interval: Duration) -> Self {
        Self {
            info_endpoint,
            refresh_interval,
        }
    }
}

impl Worker for AgentInfoFetcher {
    /// Start fetching the info endpoint with the given interval.
    ///
    /// # Warning
    /// This method does not return and should be called within a dedicated task.
    async fn run(&mut self) {
        loop {
            let current_info = AGENT_INFO_CACHE.load();
            if current_info.is_some() {
                sleep(self.refresh_interval).await;
            }
            let current_hash = current_info.as_ref().map(|info| info.state_hash.as_str());
            let res = fetch_info_with_state(&self.info_endpoint, current_hash).await;
            match res {
                Ok(FetchInfoStatus::NewState(new_info)) => {
                    info!("New /info state received");
                    AGENT_INFO_CACHE.store(Some(Arc::new(*new_info)));
                }
                Ok(FetchInfoStatus::SameState) => {
                    info!("Agent info is up-to-date")
                }
                Err(err) => {
                    error!(?err, "Error while fetching /info");
                }
            }
        }
    }
}

/// Check an `hyper::requests` for the agent state header and refresh info if needed.
///
/// Check if the state sent by the agent in the `Datadog-Agent-State` header is different from
/// the current state and triggers a refresh if needed. The refresh is spawned as a background task
/// running in the current runtime.
///
/// # Note
/// This will not trigger a fetch if the global cache is empty to avoid spamming the agent.
pub async fn check_response_for_new_state<T>(
    response: &hyper::Response<T>,
    info_endpoint: Arc<Endpoint>,
) {
    if let Some(agent_state) = response.headers().get(DATADOG_AGENT_STATE) {
        // If no info has been loaded yet, skip to let the AgentInfoFetcher fetch the first
        // version and avoid spamming the agent.
        if let Some(current_info) = AGENT_INFO_CACHE.load_full() {
            if let Ok(state) = agent_state.to_str() {
                if state != current_info.state_hash.as_str() {
                    tokio::spawn(async move {
                        let res =
                            fetch_info_with_state(&info_endpoint, Some(&current_info.state_hash))
                                .await;
                        match res {
                            Ok(FetchInfoStatus::NewState(new_info)) => {
                                info!("New /info state received");
                                AGENT_INFO_CACHE.store(Some(Arc::new(*new_info)));
                            }
                            Ok(FetchInfoStatus::SameState) => {
                                info!("Agent info is up-to-date")
                            }
                            Err(err) => {
                                error!(?err, "Error while fetching /info");
                            }
                        }
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod single_threaded_tests {
    use super::*;
    use crate::agent_info;
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

    const TEST_INFO_HASH: &str = "8c732aba385d605b010cd5bd12c03fef402eaefce989f0055aa4c7e92fe30077";

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_fetch_info_without_state() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", TEST_INFO_HASH)
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

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_fetch_info_with_state() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", TEST_INFO_HASH)
                    .body(TEST_INFO);
            })
            .await;
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

        let new_state_info_status = fetch_info_with_state(&endpoint, Some("state"))
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

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_fetch_info() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", TEST_INFO_HASH)
                    .body(TEST_INFO);
            })
            .await;
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

        let agent_info = fetch_info(&endpoint).await.unwrap();
        mock.assert();
        assert_eq!(
            *agent_info,
            AgentInfo {
                state_hash: TEST_INFO_HASH.to_string(),
                info: serde_json::from_str(TEST_INFO).unwrap(),
            }
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_agent_info_fetcher_run() {
        AGENT_INFO_CACHE.store(None);
        let server = MockServer::start();
        let mock_v1 = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", "1")
                    .body(r#"{"version":"1"}"#);
            })
            .await;
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());
        let mut fetcher = AgentInfoFetcher::new(endpoint.clone(), Duration::from_millis(100));
        assert!(agent_info::get_agent_info().is_none());
        tokio::spawn(async move {
            fetcher.run().await;
        });

        // Wait until the info is fetched
        while agent_info::get_agent_info().is_none() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let version_1 = agent_info::get_agent_info()
            .as_ref()
            .unwrap()
            .info
            .version
            .clone()
            .unwrap();
        assert_eq!(version_1, "1");
        mock_v1.assert_async().await;

        // Update the info endpoint
        mock_v1.delete_async().await;
        let mock_v2 = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", "2")
                    .body(r#"{"version":"2"}"#);
            })
            .await;

        // Wait for second fetch
        while mock_v2.hits_async().await == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // This check is not 100% deterministic, but between the time the mock returns the response
        // and we swap the atomic pointer holding the agent_info we only need to perform
        // very few operations. We wait for a maximum of 1s before failing the test and that should
        // give way more time than necesssary.
        for _ in 0..10 {
            let version_2 = agent_info::get_agent_info()
                .as_ref()
                .unwrap()
                .info
                .version
                .clone()
                .unwrap();
            if version_2 != version_1 {
                assert_eq!(version_2, "2");
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_check_response_no_current_info() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", TEST_INFO_HASH)
                    .body(TEST_INFO);
            })
            .await;

        AGENT_INFO_CACHE.store(None);

        let response = hyper::Response::builder()
            .status(200)
            .header("datadog-agent-state", TEST_INFO_HASH)
            .body(())
            .unwrap();

        let endpoint = Arc::new(Endpoint::from_url(server.url("/info").parse().unwrap()));
        check_response_for_new_state(&response, endpoint).await;

        // The background task should not be triggered, but we wait to make sure it fails if it is.
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Should not trigger a fetch since there's no current info
        mock.assert_hits_async(0).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_check_response_same_state() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", TEST_INFO_HASH)
                    .body(TEST_INFO);
            })
            .await;

        // Populate the cache
        AGENT_INFO_CACHE.store(Some(Arc::new(AgentInfo {
            state_hash: TEST_INFO_HASH.to_string(),
            info: serde_json::from_str(TEST_INFO).unwrap(),
        })));

        let response = hyper::Response::builder()
            .status(200)
            .header("datadog-agent-state", TEST_INFO_HASH)
            .body(())
            .unwrap();

        let endpoint = Arc::new(Endpoint::from_url(server.url("/info").parse().unwrap()));
        check_response_for_new_state(&response, endpoint).await;

        // The background task should not be triggered, but we wait to make sure it fails if it is.
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Should not trigger a fetch since the state matches
        mock.assert_hits_async(0).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_check_response_different_state() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", "new_state_hash")
                    .body(TEST_INFO);
            })
            .await;

        // Populate the cache
        AGENT_INFO_CACHE.store(Some(Arc::new(AgentInfo {
            state_hash: "old_state_hash".to_string(),
            info: serde_json::from_str(TEST_INFO).unwrap(),
        })));

        let response = hyper::Response::builder()
            .status(200)
            .header("datadog-agent-state", "new_state_hash")
            .body(())
            .unwrap();

        let endpoint = Arc::new(Endpoint::from_url(server.url("/info").parse().unwrap()));
        check_response_for_new_state(&response, endpoint).await;

        // Wait for the background task to complete with a timeout of 5 seconds
        let mut i = 0;
        while mock.hits_async().await == 0 && i < 10 {
            i += 1;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Should trigger a fetch since the state is different
        mock.assert_hits_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_check_response_no_state_header() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", TEST_INFO_HASH)
                    .body(TEST_INFO);
            })
            .await;

        // Populate the cache
        AGENT_INFO_CACHE.store(Some(Arc::new(AgentInfo {
            state_hash: TEST_INFO_HASH.to_string(),
            info: serde_json::from_str(TEST_INFO).unwrap(),
        })));

        let response = hyper::Response::builder().status(200).body(()).unwrap();

        let endpoint = Arc::new(Endpoint::from_url(server.url("/info").parse().unwrap()));
        check_response_for_new_state(&response, endpoint).await;

        // The background task should not be triggered, but we wait to make sure it fails if it is.
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Should not trigger a fetch since there's no state header
        mock.assert_hits_async(0).await;
    }
}
