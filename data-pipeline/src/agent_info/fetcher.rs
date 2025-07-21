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
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, error, info};

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
/// Once the run method has been started, the fetcher will
/// update the global info state based on the given refresh interval. You can access the current
/// state with [`crate::agent_info::get_agent_info`]
///
/// # Response observer
/// When the fetcher is created it also returns a [`ResponseObserver`] which can be used to check
/// the `Datadog-Agent-State` header of an agent response and trigger early refresh if a new state
/// is detected.
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
/// let (mut fetcher, _response_observer) = data_pipeline::agent_info::AgentInfoFetcher::new(
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
    trigger_rx: Option<mpsc::Receiver<()>>,
}

impl AgentInfoFetcher {
    /// Return a new `AgentInfoFetcher` fetching the `info_endpoint` on each `refresh_interval`
    /// and updating the stored info.
    ///
    /// Returns a tuple of (fetcher, trigger_component) where:
    /// - `fetcher`: The AgentInfoFetcher to be run in a background task
    /// - `response_observer`: The ResponseObserver component for checking HTTP responses
    pub fn new(info_endpoint: Endpoint, refresh_interval: Duration) -> (Self, ResponseObserver) {
        // The trigger channel stores a single message to avoid multiple triggers.
        let (trigger_tx, trigger_rx) = mpsc::channel(1);

        let fetcher = Self {
            info_endpoint,
            refresh_interval,
            trigger_rx: Some(trigger_rx),
        };

        let response_observer = ResponseObserver::new(trigger_tx);

        (fetcher, response_observer)
    }
}

impl Worker for AgentInfoFetcher {
    /// Start fetching the info endpoint with the given interval.
    ///
    /// # Warning
    /// This method does not return and should be called within a dedicated task.
    async fn run(&mut self) {
        // Skip the first fetch if some info is present to avoid calling the /info endpoint
        // at fork for heavy-forking environment.
        if AGENT_INFO_CACHE.load().is_none() {
            self.fetch_and_update().await;
        }

        // Main loop waiting for a trigger event or the end of the refresh interval to trigger the
        // fetch.
        loop {
            match &mut self.trigger_rx {
                Some(trigger_rx) => {
                    tokio::select! {
                        // Wait for manual trigger (new state from headers)
                        trigger = trigger_rx.recv() => {
                            if trigger.is_some() {
                                self.fetch_and_update().await;
                            } else {
                                // The channel has been closed
                                self.trigger_rx = None;
                            }
                        }
                        // Regular periodic fetch timer
                        _ = sleep(self.refresh_interval) => {
                            self.fetch_and_update().await;
                        }
                    };
                }
                None => {
                    // If the trigger channel is closed we only use timed fetch.
                    sleep(self.refresh_interval).await;
                    self.fetch_and_update().await;
                }
            }
        }
    }
}

impl AgentInfoFetcher {
    /// Fetch agent info and update cache if needed
    async fn fetch_and_update(&self) {
        let current_info = AGENT_INFO_CACHE.load();
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

/// Component for observing HTTP responses and triggering agent info fetches.
///
/// This component checks HTTP responses for the `Datadog-Agent-State` header and
/// sends trigger messages to the agent info fetcher when a new state is detected.
#[derive(Debug, Clone)]
pub struct ResponseObserver {
    trigger_tx: mpsc::Sender<()>,
}

impl ResponseObserver {
    /// Create a new ResponseObserver with the given channel sender.
    pub fn new(trigger_tx: mpsc::Sender<()>) -> Self {
        Self { trigger_tx }
    }

    /// Check the given HTTP response for agent state changes and trigger a fetch if needed.
    ///
    /// This method examines the `Datadog-Agent-State` header in the response and compares
    /// it with the previously seen state. If the state has changed, it sends a trigger
    /// message to the agent info fetcher.
    pub fn check_response<T>(&self, response: &hyper::Response<T>) {
        if let Some(agent_state) = response.headers().get(DATADOG_AGENT_STATE) {
            if let Ok(state_str) = agent_state.to_str() {
                let current_state = AGENT_INFO_CACHE.load();
                if current_state.as_ref().map(|s| s.state_hash.as_str()) != Some(state_str) {
                    match self.trigger_tx.try_send(()) {
                        Ok(_) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            debug!(
                                "Response observer channel full, fetch has already been triggered"
                            );
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            debug!("Agent info fetcher channel closed, unable to trigger refresh");
                        }
                    }
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
        let (mut fetcher, _response_observer) =
            AgentInfoFetcher::new(endpoint.clone(), Duration::from_millis(100));
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
        // give way more time than necessary.
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
    #[tokio::test]
    async fn test_agent_info_trigger_different_state() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", "new_state")
                    .body(r#"{"version":"triggered"}"#);
            })
            .await;

        // Populate the cache with initial state
        AGENT_INFO_CACHE.store(Some(Arc::new(AgentInfo {
            state_hash: "old_state".to_string(),
            info: serde_json::from_str(r#"{"version":"old"}"#).unwrap(),
        })));

        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());
        let (mut fetcher, response_observer) =
            // Interval is too long to fetch during the test
            AgentInfoFetcher::new(endpoint, Duration::from_secs(3600));

        tokio::spawn(async move {
            fetcher.run().await;
        });

        // Create a mock HTTP response with the new agent state
        let response = hyper::Response::builder()
            .status(200)
            .header("datadog-agent-state", "new_state")
            .body(())
            .unwrap();

        // Use the trigger component to check the response
        response_observer.check_response(&response);

        // Wait for the fetch to complete
        const MAX_ATTEMPTS: u32 = 500;
        const SLEEP_DURATION_MS: u64 = 10;

        let mut attempts = 0;
        while mock.hits_async().await == 0 && attempts < MAX_ATTEMPTS {
            attempts += 1;
            tokio::time::sleep(Duration::from_millis(SLEEP_DURATION_MS)).await;
        }

        // Should trigger a fetch since the state is different
        mock.assert_hits_async(1).await;

        // Wait for the cache to be updated with proper timeout
        let mut attempts = 0;

        while attempts < MAX_ATTEMPTS {
            let updated_info = AGENT_INFO_CACHE.load();
            if let Some(info) = updated_info.as_ref() {
                if info.state_hash == "new_state" {
                    break;
                }
            }
            attempts += 1;
            tokio::time::sleep(Duration::from_millis(SLEEP_DURATION_MS)).await;
        }

        // Verify the cache was updated
        let updated_info = AGENT_INFO_CACHE.load();
        assert!(updated_info.is_some());
        assert_eq!(updated_info.as_ref().unwrap().state_hash, "new_state");
        assert_eq!(
            updated_info
                .as_ref()
                .unwrap()
                .info
                .version
                .as_ref()
                .unwrap(),
            "triggered"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_agent_info_trigger_same_state() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("datadog-agent-state", "same_state")
                    .body(r#"{"version":"same"}"#);
            })
            .await;

        // Populate the cache with the same state
        AGENT_INFO_CACHE.store(Some(Arc::new(AgentInfo {
            state_hash: "same_state".to_string(),
            info: serde_json::from_str(r#"{"version":"same"}"#).unwrap(),
        })));

        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());
        let (mut fetcher, response_observer) =
            AgentInfoFetcher::new(endpoint, Duration::from_secs(3600)); // Very long interval

        tokio::spawn(async move {
            fetcher.run().await;
        });

        // Create a mock HTTP response with the same agent state
        let response = hyper::Response::builder()
            .status(200)
            .header("datadog-agent-state", "same_state")
            .body(())
            .unwrap();

        // Use the trigger component to check the response
        response_observer.check_response(&response);

        // Wait to ensure no fetch occurs
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Should not trigger a fetch since the state is the same
        mock.assert_hits_async(0).await;
    }
}
