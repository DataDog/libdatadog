// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This file contains code for fetching and sharing the info from the Datadog Agent.
//! It will keep one fetcher per Endpoint. Each sidecar connection keeps an AgentInfoGuard alive
//! for as long as it is configured with that endpoint.
//! The fetcher will remain alive for a short while after all guards have been dropped.
//! It writes the raw agent response to shared memory at a fixed per-endpoint location, to be
//! consumed be tracers.

use arc_swap::ArcSwap;
use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use futures::FutureExt;
use futures::future::Shared;
use http::uri::PathAndQuery;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::{Endpoint, MutexExt};
use libdd_data_pipeline::agent_info::schema::AgentInfoStruct;
use libdd_data_pipeline::agent_info::{FetchInfoStatus, fetch_info_with_state};
use libdd_ipc::one_way_shared_memory::{OneWayShmReader, OneWayShmWriter, open_named_shm};
use libdd_ipc::platform::NamedShmHandle;
use libdd_live_debugger::sender::agent_info_supports_debugger_v2_endpoint;
use manual_future::ManualFuture;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{error, warn};
use zwohash::{HashMap, ZwoHasher};

/// The map lock is only taken when a guard is created and when an idle fetcher retires; guards
/// and readers of the fetched info never touch it, so per-connection guards stay uncontended.
#[derive(Default, Clone)]
pub struct AgentInfos(Arc<Mutex<HashMap<Endpoint, Arc<AgentInfoFetcher>>>>);

impl AgentInfos {
    /// Ensures a fetcher for the endpoints agent info and keeps it alive for at least as long as
    /// the returned guard exists.
    pub fn query_for(&self, endpoint: Endpoint) -> AgentInfoGuard {
        let mut infos_guard = self.0.lock_or_panic();
        let fetcher = match infos_guard.get(&endpoint) {
            Some(fetcher) => {
                fetcher.rc.fetch_add(1, Ordering::AcqRel);
                fetcher.clone()
            }
            None => {
                let fetcher = AgentInfoFetcher::start(self.clone(), endpoint.clone());
                infos_guard.insert(endpoint, fetcher.clone());
                fetcher
            }
        };

        AgentInfoGuard { fetcher }
    }
}

pub struct AgentInfoGuard {
    fetcher: Arc<AgentInfoFetcher>,
}

impl AgentInfoGuard {
    pub fn get(&self) -> Shared<ManualFuture<AgentInfoStruct>> {
        (**self.fetcher.infos.load()).clone()
    }

    /// Whether the last fetched agent info lacks the debugger v2 intake. False until the first
    /// info has been fetched.
    pub fn lacks_debugger_v2_endpoint(&self) -> bool {
        self.fetcher.lacks_debugger_v2.load(Ordering::Relaxed)
    }
}

impl Drop for AgentInfoGuard {
    fn drop(&mut self) {
        self.fetcher.touch();
        self.fetcher.rc.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct AgentInfoFetcher {
    /// Will be kept alive forever if rc > 0.
    rc: AtomicU32,
    created_at: Instant,
    /// Milliseconds since `created_at` of the last guard drop. Once it is too old (and rc is 0),
    /// we'll stop the fetcher.
    last_update_ms: AtomicU64,
    /// The initial fetch is an unresolved future (to be able to await on it), subsequent fetches
    /// are simply directly replacing this with a resolved future.
    infos: ArcSwap<Shared<ManualFuture<AgentInfoStruct>>>,
    lacks_debugger_v2: AtomicBool,
}

impl AgentInfoFetcher {
    fn touch(&self) {
        let elapsed = self.created_at.elapsed().as_millis();
        self.last_update_ms.store(
            u64::try_from(elapsed).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn idle_for(&self) -> Duration {
        self.created_at
            .elapsed()
            .saturating_sub(Duration::from_millis(
                self.last_update_ms.load(Ordering::Relaxed),
            ))
    }

    fn start(agent_infos: AgentInfos, endpoint: Endpoint) -> Arc<AgentInfoFetcher> {
        let (future, completer) = ManualFuture::new();
        let fetcher = Arc::new(AgentInfoFetcher {
            rc: AtomicU32::new(1),
            created_at: Instant::now(),
            last_update_ms: AtomicU64::new(0),
            infos: ArcSwap::from_pointee(future.shared()),
            lacks_debugger_v2: AtomicBool::new(false),
        });
        let this = fetcher.clone();
        #[allow(clippy::unwrap_used)]
        tokio::spawn(async move {
            let mut state: Option<String> = None;
            let mut writer = None;
            let mut completer = Some(completer);
            let mut fetch_endpoint = endpoint.clone();
            let mut parts = fetch_endpoint.url.into_parts();
            parts.path_and_query = Some(PathAndQuery::from_static("/info"));
            fetch_endpoint.url = http::Uri::from_parts(parts).unwrap();
            loop {
                let fetched =
                    fetch_info_with_state::<NativeCapabilities>(&fetch_endpoint, state.as_deref())
                        .await;
                {
                    // Checked under the map lock, so that query_for cannot revive a fetcher
                    // which is being removed.
                    let mut infos_guard = agent_infos.0.lock_or_panic();
                    if this.rc.load(Ordering::Acquire) == 0
                        && this.idle_for() > Duration::from_secs(60)
                    {
                        infos_guard.remove(&endpoint);
                        break;
                    }
                }
                let mut complete_fut = None;
                match fetched {
                    Ok(FetchInfoStatus::SameState) => {}
                    Ok(FetchInfoStatus::NewState(status)) => {
                        state = Some(status.state_hash);
                        if writer.is_none() {
                            writer = match OneWayShmWriter::<NamedShmHandle>::new(info_path(
                                &endpoint,
                            )) {
                                Ok(writer) => Some(writer),
                                Err(e) => {
                                    error!("Failed acquiring an agent info writer: {e:?}");
                                    None
                                }
                            };
                        }
                        if let Some(ref writer) = writer {
                            // A payload that does not fit is logged and dropped by the
                            // writer; the previously published info stays readable.
                            _ = writer.write(&serde_json::to_vec(&status.info).unwrap());
                        }
                        this.lacks_debugger_v2.store(
                            !agent_info_supports_debugger_v2_endpoint(&status.info),
                            Ordering::Relaxed,
                        );
                        if let Some(completer) = completer {
                            complete_fut = Some(completer.complete(status.info));
                        } else {
                            this.infos
                                .store(Arc::new(ManualFuture::new_completed(status.info).shared()));
                        }
                        completer = None;
                    }
                    Err(e) => {
                        // We'll just return the old values as long as the endpoint is
                        // unreachable.
                        warn!(
                            "The agent info for {} could not be fetched: {}",
                            fetch_endpoint.url, e
                        );
                    }
                }
                if let Some(complete_fut) = complete_fut.take() {
                    complete_fut.await;
                }
                sleep(Duration::from_secs(60)).await;
            }
        });

        fetcher
    }
}

fn info_path(endpoint: &Endpoint) -> CString {
    let mut hasher = ZwoHasher::default();
    endpoint.hash(&mut hasher);
    let mut path = format!(
        "/ddinf{}-{}",
        crate::shm_namespace(),
        BASE64_URL_SAFE_NO_PAD.encode(hasher.finish().to_ne_bytes()),
    );
    if cfg!(unix) {
        path.truncate(31);
    }

    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

pub struct AgentInfoReader {
    reader: OneWayShmReader<NamedShmHandle, Endpoint>,
    info: Option<AgentInfoStruct>,
}

impl AgentInfoReader {
    pub fn new(endpoint: &Endpoint) -> AgentInfoReader {
        let path = info_path(endpoint);
        AgentInfoReader {
            reader: OneWayShmReader::new_with_opener(
                open_named_shm(&path).ok(),
                endpoint.clone(),
                |endpoint| open_named_shm(&info_path(endpoint)).ok(),
            ),
            info: None,
        }
    }

    pub fn reconnect(&self) {
        self.reader.reconnect(&info_path(&self.reader.extra));
    }

    pub fn read(&mut self) -> (bool, &Option<AgentInfoStruct>) {
        let (mut updated, data) = self.reader.read();
        if updated {
            // This may transiently happen during AgentInfo initialization
            if data.is_empty() {
                updated = false
            } else {
                match serde_json::from_slice(data) {
                    Ok(info) => self.info = Some(info),
                    Err(e) => error!("Failed deserializing the agent info: {e:?}"),
                }
            }
        }
        (updated, &self.info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn readers_follow_a_new_thread_master() {
        const CHILD: &str = "DD_TEST_AGENT_INFO_MASTER_CHANGE";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "service::agent_info::tests::readers_follow_a_new_thread_master",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }

        let endpoint = Endpoint::from_slice("http://thread-master.invalid:8126");
        crate::use_thread_sidecar_shm_namespace(Some(std::process::id()));
        let old_info = OneWayShmWriter::<NamedShmHandle>::new(info_path(&endpoint)).unwrap();
        let old_config = crate::agent_remote_config::new_writer(&endpoint).unwrap();
        assert!(old_info.write(TEST_INFO.as_bytes()));
        assert!(old_config.write(b"old config"));
        let mut info = AgentInfoReader::new(&endpoint);
        let mut config = crate::agent_remote_config::new_reader(&endpoint);
        assert!(info.read().0);
        assert_eq!(config.read(), (true, b"old config".as_slice()));
        info.reconnect();
        config.reconnect();
        assert!(old_info.write(TEST_INFO.as_bytes()));
        assert!(old_config.write(b"old config"));

        // Stay outside the signed PID range to avoid another process's SHM.
        let other_master = std::process::id() | (1 << 31);
        crate::use_thread_sidecar_shm_namespace(Some(other_master));
        info.reconnect();
        config.reconnect();
        let new_info = OneWayShmWriter::<NamedShmHandle>::new(info_path(&endpoint)).unwrap();
        let new_config = crate::agent_remote_config::new_writer(&endpoint).unwrap();
        assert!(new_info.write(TEST_INFO.replace("testenv", "newenv").as_bytes()));
        assert!(new_config.write(b"new config"));
        assert!(info.read().0, "agent info must follow the new master");
        assert_eq!(
            info.info
                .as_ref()
                .unwrap()
                .config
                .as_ref()
                .unwrap()
                .default_env
                .as_deref(),
            Some("newenv")
        );
        assert_eq!(config.read(), (true, b"new config".as_slice()));

        assert!(old_info.write(TEST_INFO.as_bytes()));
        assert!(old_config.write(b"old config"));
        info.reconnect();
        config.reconnect();
        assert!(new_info.write(TEST_INFO.as_bytes()));
        assert!(new_config.write(b"same master"));
        assert_eq!(config.read(), (true, b"same master".as_slice()));

        crate::use_thread_sidecar_shm_namespace(Some(std::process::id()));
        config.reconnect();
        let replacement = crate::agent_remote_config::new_writer(&endpoint).unwrap();
        assert!(replacement.write(b"returned"));
        assert_eq!(config.read(), (true, b"returned".as_slice()));
    }

    #[test]
    fn shm_paths_distinguish_endpoints() {
        assert_ne!(
            info_path(&Endpoint::from_slice("http://agent-a:8126")),
            info_path(&Endpoint::from_slice("http://agent-b:8126")),
        );
    }

    const TEST_INFO: &str = r#"{
        "config": {
            "default_env": "testenv"
        }
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
        let endpoint = Endpoint::from_url(server.url("/").parse().unwrap());
        let agent_infos = AgentInfos::default();

        let mut reader = AgentInfoReader::new(&endpoint);
        assert_eq!(reader.read(), (false, &None));

        let info = agent_infos.query_for(endpoint).get().await;
        mock.assert();
        assert_eq!(
            info.config.unwrap().default_env,
            Some("testenv".to_string())
        );

        let (updated, info) = reader.read();
        assert!(updated);
        assert_eq!(
            info.as_ref().unwrap().config.as_ref().unwrap().default_env,
            Some("testenv".to_string())
        );
    }
}
