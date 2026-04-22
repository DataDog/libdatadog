// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Provides utilities to fetch the agent /info endpoint and an automatic fetcher to keep info
//! up-to-date

use super::{
    schema::{AgentInfo, AgentInfoStruct},
    AGENT_INFO_CACHE,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bytes::Bytes;
use libdd_capabilities::{HttpClientTrait, MaybeSend};
use libdd_common::Endpoint;
use libdd_shared_runtime::Worker;
use sha2::{Digest, Sha256};
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, warn};

/// Whether the agent reported the same value or not.
#[derive(Debug)]
pub enum FetchInfoStatus {
    /// Unchanged
    SameState,
    /// Has a new state
    NewState(Box<AgentInfo>),
}

/// Fetch info from the given endpoint and compare state-related hashes.
///
/// If either the agent state hash or container tags hash is different from the current one:
/// - Return a `FetchInfoStatus::NewState` of the info struct
/// - Else return `FetchInfoStatus::SameState`
async fn fetch_info_with_state_and_container_tags<H: HttpClientTrait>(
    info_endpoint: &Endpoint,
    current_state_hash: Option<&str>,
    current_container_tags_hash: Option<&str>,
) -> Result<FetchInfoStatus> {
    let (new_state_hash, body_data, container_tags_hash) =
        fetch_and_hash_response::<H>(info_endpoint).await?;

    if current_state_hash.is_some_and(|state| state == new_state_hash)
        && (current_container_tags_hash.is_none()
            || current_container_tags_hash == container_tags_hash.as_deref())
    {
        return Ok(FetchInfoStatus::SameState);
    }

    let mut info_struct: AgentInfoStruct = serde_json::from_slice(&body_data)?;
    info_struct.container_tags_hash = container_tags_hash;

    let info = Box::new(AgentInfo {
        state_hash: new_state_hash,
        info: info_struct,
    });
    Ok(FetchInfoStatus::NewState(info))
}

/// Fetch info from the given info_endpoint and compare its state to the current state hash.
///
/// If the state hash is different from the current one:
/// - Return a `FetchInfoStatus::NewState` of the info struct
/// - Else return `FetchInfoStatus::SameState`
pub async fn fetch_info_with_state<H: HttpClientTrait>(
    info_endpoint: &Endpoint,
    current_state_hash: Option<&str>,
) -> Result<FetchInfoStatus> {
    fetch_info_with_state_and_container_tags::<H>(info_endpoint, current_state_hash, None).await
}

/// Fetch the info endpoint once and return the info.
///
/// Can be used for one-time access to the agent's info. If you need to access the info several
/// times use `AgentInfoFetcher` to keep the info up-to-date.
///
/// # Example
/// ```no_run
/// # use anyhow::Result;
/// # use libdd_capabilities_impl::NativeCapabilities;
/// # #[tokio::main]
/// # async fn main() -> Result<()> {
/// // Define the endpoint
/// let endpoint = libdd_common::Endpoint::from_url("http://localhost:8126/info".parse().unwrap());
/// // Fetch the info
/// let agent_info = libdd_data_pipeline::agent_info::fetch_info::<NativeCapabilities>(&endpoint)
///     .await
///     .unwrap();
/// println!("Agent version is {}", agent_info.info.version.unwrap());
/// # Ok(())
/// # }
/// ```
pub async fn fetch_info<H: HttpClientTrait>(info_endpoint: &Endpoint) -> Result<Box<AgentInfo>> {
    match fetch_info_with_state::<H>(info_endpoint, None).await? {
        FetchInfoStatus::NewState(info) => Ok(info),
        // Should never be reached since there is no previous state.
        FetchInfoStatus::SameState => Err(anyhow!("Invalid state header")),
    }
}

/// Fetch and hash the response from the agent info endpoint.
///
/// Returns a tuple of (state_hash, response_body_bytes, container_tags_hash).
/// The hash is calculated using SHA256 to match the agent's calculation method.
async fn fetch_and_hash_response<H: HttpClientTrait>(
    info_endpoint: &Endpoint,
) -> Result<(String, bytes::Bytes, Option<String>)> {
    let req = info_endpoint
        .to_request_builder(concat!("Libdatadog/", env!("CARGO_PKG_VERSION")))?
        .body(Bytes::new())
        .map_err(|e| anyhow!("Failed to build request: {}", e))?;

    let timeout = Duration::from_millis(info_endpoint.timeout_ms);
    let client = H::new_client();
    let res = tokio::time::timeout(timeout, client.request(req))
        .await
        .map_err(|_| anyhow!("Request to /info timed out after {:?}", timeout))??;

    // Extract the Datadog-Container-Tags-Hash header
    let container_tags_hash = res
        .headers()
        .get("Datadog-Container-Tags-Hash")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let body_data = res.into_body();
    let hash = format!("{:x}", Sha256::digest(&body_data));

    Ok((hash, body_data, container_tags_hash))
}

/// Fetch the info endpoint and update an ArcSwap keeping it up-to-date.
///
/// This type implements [`libdd_shared_runtime::Worker`] and is intended to be driven by a worker
/// runner such as [`libdd_shared_runtime::SharedRuntime`].
/// In that lifecycle, `trigger()` waits for the next refresh event and `run()` performs a single
/// fetch.
///
/// You can access the current state with [`crate::agent_info::get_agent_info`].
///
/// # Response observer
/// When the fetcher is created it also returns a [`ResponseObserver`] which can be used to check
/// the `Datadog-Agent-State` header of an agent response and trigger early refresh if a new state
/// is detected.
///
/// # Example
/// ```no_run
/// # use anyhow::Result;
/// # use libdd_capabilities_impl::NativeCapabilities;
/// # use libdd_shared_runtime::Worker;
/// # #[tokio::main]
/// # async fn main() -> Result<()> {
/// // Define the endpoint
/// use libdd_data_pipeline::agent_info;
/// let endpoint = libdd_common::Endpoint::from_url("http://localhost:8126/info".parse().unwrap());
/// // Create the fetcher
/// let (mut fetcher, _response_observer) = libdd_data_pipeline::agent_info::AgentInfoFetcher::<
///     NativeCapabilities,
/// >::new(
///     endpoint, std::time::Duration::from_secs(5 * 60)
/// );
/// // Start the fetcher on a shared runtime
/// let runtime = libdd_shared_runtime::SharedRuntime::new()?;
/// runtime.spawn_worker(fetcher, true)?;
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
/// `H` is the HTTP client implementation, see [`HttpClientTrait`]. Leaf crates
/// pin it to a concrete type.
#[derive(Debug)]
pub struct AgentInfoFetcher<H: HttpClientTrait> {
    info_endpoint: Endpoint,
    refresh_interval: Duration,
    trigger_rx: Option<mpsc::Receiver<()>>,
    trigger_tx: mpsc::Sender<()>,
    /// `H` must live on the struct because `Worker::run(&mut self)` (a fixed
    /// trait signature) calls `fetch_info_with_state::<H>()` internally.
    _phantom: PhantomData<H>,
}

impl<H: HttpClientTrait> AgentInfoFetcher<H> {
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
            trigger_tx: trigger_tx.clone(),
            _phantom: PhantomData,
        };

        let response_observer = ResponseObserver::new(trigger_tx);

        (fetcher, response_observer)
    }

    /// Drain message from the trigger channel.
    pub fn drain(&mut self) {
        // We read only once as the channel has a capacity of 1
        if let Some(rx) = &mut self.trigger_rx {
            let _ = rx.try_recv();
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<H: HttpClientTrait + MaybeSend + Sync + 'static> Worker for AgentInfoFetcher<H> {
    async fn initial_trigger(&mut self) {
        // Skip initial wait if cache is not populated
        if AGENT_INFO_CACHE.load().is_none() {
            return;
        }
        self.trigger().await
    }

    async fn trigger(&mut self) {
        // Wait for either a manual trigger or the refresh interval
        match &mut self.trigger_rx {
            Some(trigger_rx) => {
                tokio::select! {
                    // Wait for manual trigger (new state from headers)
                    trigger = trigger_rx.recv() => {
                        if trigger.is_none() {
                            // The channel has been closed
                            self.trigger_rx = None;
                        }
                    }
                    // Regular periodic fetch timer
                    _ = sleep(self.refresh_interval) => {}
                }
            }
            None => {
                // If the trigger channel is closed we only use timed fetch.
                sleep(self.refresh_interval).await;
            }
        }
    }

    async fn on_pause(&mut self) {
        // Release the IoStack waker stored in trigger_rx by waking the channel and drain the
        // message to avoid a spurious fetch on restart. If the channel is not empty then it has
        // already been waked.
        if self.trigger_rx.as_ref().is_some_and(|rx| rx.is_empty()) {
            let _ = self.trigger_tx.try_send(());
            self.drain();
        };
    }

    async fn run(&mut self) {
        self.fetch_and_update().await;
    }
}

impl<H: HttpClientTrait> AgentInfoFetcher<H> {
    /// Fetch agent info and update cache if needed
    async fn fetch_and_update(&self) {
        let current_info = AGENT_INFO_CACHE.load();
        let current_hash = current_info.as_ref().map(|info| info.state_hash.as_str());
        let current_container_tags_hash = current_info
            .as_ref()
            .and_then(|info| info.info.container_tags_hash.as_deref());
        let res = fetch_info_with_state_and_container_tags::<H>(
            &self.info_endpoint,
            current_hash,
            current_container_tags_hash,
        )
        .await;
        match res {
            Ok(FetchInfoStatus::NewState(new_info)) => {
                debug!("New /info state received");
                AGENT_INFO_CACHE.store(Some(Arc::new(*new_info)));
            }
            Ok(FetchInfoStatus::SameState) => {
                debug!("Agent info is up-to-date")
            }
            Err(err) => {
                warn!(?err, "Error while fetching /info");
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
    pub fn check_response(&self, response: &http::Response<Bytes>) {
        let state_str = response
            .headers()
            .get("datadog-agent-state")
            .and_then(|v| v.to_str().ok());
        if let Some(state_str) = state_str {
            let current_state = AGENT_INFO_CACHE.load();
            if current_state.as_ref().map(|s| s.state_hash.as_str()) != Some(state_str) {
                match self.trigger_tx.try_send(()) {
                    Ok(_) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        debug!("Response observer channel full, fetch has already been triggered");
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        debug!("Agent info fetcher channel closed, unable to trigger refresh");
                    }
                }
            }
        }
    }

    /// Manually send a message to the trigger channel.
    pub fn manual_trigger(&self) {
        let _ = self.trigger_tx.try_send(());
    }
}

#[cfg(test)]
mod single_threaded_tests {
    use super::*;
    use crate::agent_info;
    use httpmock::prelude::*;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_shared_runtime::SharedRuntime;

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

    fn calculate_hash(json: &str) -> String {
        format!("{:x}", Sha256::digest(json.as_bytes()))
    }

    const TEST_INFO_HASH: &str = "b7709671827946c15603847bca76c90438579c038ec134eae19c51f1f3e3dfea";

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_fetch_info_without_state() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(TEST_INFO);
            })
            .await;
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

        let info_status = fetch_info_with_state::<NativeCapabilities>(&endpoint, None)
            .await
            .unwrap();
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
                    .body(TEST_INFO);
            })
            .await;
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

        let new_state_info_status =
            fetch_info_with_state::<NativeCapabilities>(&endpoint, Some("state"))
                .await
                .unwrap();
        let same_state_info_status =
            fetch_info_with_state::<NativeCapabilities>(&endpoint, Some(TEST_INFO_HASH))
                .await
                .unwrap();

        mock.assert_calls(2);
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
    async fn test_fetch_info_with_same_state_but_different_container_tags_hash() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("Datadog-Container-Tags-Hash", "new-container-hash")
                    .body(TEST_INFO);
            })
            .await;
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

        let info_status = fetch_info_with_state_and_container_tags::<NativeCapabilities>(
            &endpoint,
            Some(TEST_INFO_HASH),
            Some("old-container-hash"),
        )
        .await
        .unwrap();

        mock.assert();
        assert!(
            matches!(info_status, FetchInfoStatus::NewState(info) if *info == AgentInfo {
                state_hash: TEST_INFO_HASH.to_string(),
                info: AgentInfoStruct {
                    container_tags_hash: Some("new-container-hash".to_string()),
                    ..serde_json::from_str(TEST_INFO).unwrap()
                },
            })
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_fetch_info_can_ignore_container_tags_hash() {
        let server = MockServer::start();
        let mock = server
            .mock_async(|when, then| {
                when.path("/info");
                then.status(200)
                    .header("content-type", "application/json")
                    .header("Datadog-Container-Tags-Hash", "new-container-hash")
                    .body(TEST_INFO);
            })
            .await;
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

        let info_status =
            fetch_info_with_state::<NativeCapabilities>(&endpoint, Some(TEST_INFO_HASH))
                .await
                .unwrap();

        mock.assert();
        assert!(matches!(info_status, FetchInfoStatus::SameState));
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
                    .body(TEST_INFO);
            })
            .await;
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());

        let agent_info = fetch_info::<NativeCapabilities>(&endpoint).await.unwrap();
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
    #[test]
    fn test_agent_info_fetcher_run() {
        AGENT_INFO_CACHE.store(None);
        let server = MockServer::start();
        let mut mock_v1 = server.mock(|when, then| {
            when.path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"version":"1"}"#);
        });
        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());
        let (fetcher, _response_observer) = AgentInfoFetcher::<NativeCapabilities>::new(
            endpoint.clone(),
            Duration::from_millis(100),
        );
        assert!(agent_info::get_agent_info().is_none());
        let shared_runtime = SharedRuntime::new().unwrap();
        shared_runtime.spawn_worker(fetcher, true).unwrap();

        // Wait until the info is fetched
        let start = std::time::Instant::now();
        while agent_info::get_agent_info().is_none() {
            assert!(
                start.elapsed() <= Duration::from_secs(10),
                "Timeout waiting for first /info fetch"
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        let version_1 = agent_info::get_agent_info()
            .as_ref()
            .unwrap()
            .info
            .version
            .clone()
            .unwrap();
        assert_eq!(version_1, "1");

        // Update the info endpoint
        mock_v1.delete();
        let mock_v2 = server.mock(|when, then| {
            when.path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"version":"2"}"#);
        });

        // Wait for second fetch
        let start = std::time::Instant::now();
        while mock_v2.calls() == 0 {
            assert!(
                start.elapsed() <= Duration::from_secs(10),
                "Timeout waiting for second /info fetch"
            );
            std::thread::sleep(Duration::from_millis(100));
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
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_agent_info_trigger_different_state() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"version":"triggered"}"#);
        });

        // Populate the cache with initial state
        AGENT_INFO_CACHE.store(Some(Arc::new(AgentInfo {
            state_hash: "old_state".to_string(),
            info: serde_json::from_str(r#"{"version":"old"}"#).unwrap(),
        })));

        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());
        let (fetcher, response_observer) =
            // Interval is too long to fetch during the test
            AgentInfoFetcher::<NativeCapabilities>::new(endpoint, Duration::from_secs(3600));

        let shared_runtime = SharedRuntime::new().unwrap();
        shared_runtime.spawn_worker(fetcher, true).unwrap();

        // Create a mock HTTP response with the new agent state
        let response = http::Response::builder()
            .status(200)
            .header("datadog-agent-state", "new_state")
            .body(Bytes::new())
            .unwrap();

        // Use the trigger component to check the response
        response_observer.check_response(&response);

        // Wait for the fetch to complete
        const MAX_ATTEMPTS: u32 = 500;
        const SLEEP_DURATION_MS: u64 = 10;

        let mut attempts = 0;
        while mock.calls() == 0 && attempts < MAX_ATTEMPTS {
            attempts += 1;
            std::thread::sleep(Duration::from_millis(SLEEP_DURATION_MS));
        }

        // Should trigger a fetch since the state is different
        mock.assert_calls(1);

        // Wait for the cache to be updated with proper timeout
        let mut attempts = 0;
        let expected_hash = calculate_hash(r#"{"version":"triggered"}"#);

        while attempts < MAX_ATTEMPTS {
            let updated_info = AGENT_INFO_CACHE.load();
            if let Some(info) = updated_info.as_ref() {
                if info.state_hash == expected_hash {
                    break;
                }
            }
            attempts += 1;
            std::thread::sleep(Duration::from_millis(SLEEP_DURATION_MS));
        }

        // Verify the cache was updated
        let updated_info = AGENT_INFO_CACHE.load();
        assert!(updated_info.is_some());
        assert_eq!(updated_info.as_ref().unwrap().state_hash, expected_hash);
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
    #[test]
    fn test_agent_info_trigger_same_state() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"version":"same"}"#);
        });

        let same_json = r#"{"version":"same"}"#;
        let same_hash = calculate_hash(same_json);

        // Populate the cache with the same state
        AGENT_INFO_CACHE.store(Some(Arc::new(AgentInfo {
            state_hash: same_hash.clone(),
            info: serde_json::from_str(same_json).unwrap(),
        })));

        let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());
        let (fetcher, response_observer) =
            AgentInfoFetcher::<NativeCapabilities>::new(endpoint, Duration::from_secs(3600)); // Very long interval

        let shared_runtime = SharedRuntime::new().unwrap();
        shared_runtime.spawn_worker(fetcher, true).unwrap();

        // Create a mock HTTP response with the same agent state
        let response = http::Response::builder()
            .status(200)
            .header("datadog-agent-state", same_hash.as_str())
            .body(Bytes::new())
            .unwrap();

        // Use the trigger component to check the response
        response_observer.check_response(&response);

        // Wait to ensure no fetch occurs
        std::thread::sleep(Duration::from_millis(500));

        // Should not trigger a fetch since the state is the same
        mock.assert_calls(0);
    }
}
