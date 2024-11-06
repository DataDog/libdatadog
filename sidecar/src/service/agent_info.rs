// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This file contains code for fetching and sharing the info from the Datadog Agent.
//! It will keep one fetcher per Endpoint. The SidecarServer is expected to keep the AgentInfoGuard
//! alive for the lifetime of the session.
//! The fetcher will remain alive for a short while after all guards have been dropped.
//! It writes the raw agent response to shared memory at a fixed per-endpoint location, to be
//! consumed be tracers.

use crate::one_way_shared_memory::{open_named_shm, OneWayShmReader, OneWayShmWriter};
use crate::primary_sidecar_identifier;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use data_pipeline::agent_info::schema::AgentInfoStruct;
use data_pipeline::agent_info::{fetch_info_with_state, FetchInfoStatus};
use datadog_ipc::platform::NamedShmHandle;
use ddcommon::Endpoint;
use futures::future::Shared;
use futures::FutureExt;
use http::uri::PathAndQuery;
use manual_future::ManualFuture;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{error, warn};
use zwohash::{HashMap, ZwoHasher};

#[derive(Default, Clone)]
pub struct AgentInfos(Arc<Mutex<HashMap<Endpoint, AgentInfoFetcher>>>);

impl AgentInfos {
    /// Ensures a fetcher for the endpoints agent info and keeps it alive for at least as long as
    /// the returned guard exists.
    pub fn query_for(&self, endpoint: Endpoint) -> AgentInfoGuard {
        let mut infos_guard = self.0.lock().unwrap();
        if let Some(info) = infos_guard.get_mut(&endpoint) {
            info.rc += 1;
        } else {
            infos_guard.insert(
                endpoint.clone(),
                AgentInfoFetcher::new(self.clone(), endpoint.clone()),
            );
        }

        AgentInfoGuard {
            infos: self.clone(),
            endpoint,
        }
    }
}

pub struct AgentInfoGuard {
    infos: AgentInfos,
    endpoint: Endpoint,
}

impl AgentInfoGuard {
    pub fn get(&self) -> Shared<ManualFuture<AgentInfoStruct>> {
        let infos_guard = self.infos.0.lock().unwrap();
        let infos = infos_guard.get(&self.endpoint).unwrap();
        infos.infos.clone()
    }
}

impl Drop for AgentInfoGuard {
    fn drop(&mut self) {
        let mut infos_guard = self.infos.0.lock().unwrap();
        let info = infos_guard.get_mut(&self.endpoint).unwrap();
        info.last_update = Instant::now();
        info.rc -= 1;
    }
}

pub struct AgentInfoFetcher {
    /// Once the last_update is too old, we'll stop the fetcher.
    last_update: Instant,
    /// Will be kept alive forever if rc > 0.
    rc: u32,
    /// The initial fetch is an unresolved future (to be able to await on it), subsequent fetches
    /// are simply directly replacing this with a resolved future.
    infos: Shared<ManualFuture<AgentInfoStruct>>,
}

impl AgentInfoFetcher {
    fn new(agent_infos: AgentInfos, endpoint: Endpoint) -> AgentInfoFetcher {
        let (future, completer) = ManualFuture::new();
        tokio::spawn(async move {
            let mut state: Option<String> = None;
            let mut writer = None;
            let mut completer = Some(completer);
            let mut fetch_endpoint = endpoint.clone();
            let mut parts = fetch_endpoint.url.into_parts();
            parts.path_and_query = Some(PathAndQuery::from_static("/info"));
            fetch_endpoint.url = hyper::Uri::from_parts(parts).unwrap();
            loop {
                let fetched = fetch_info_with_state(&fetch_endpoint, state.as_deref()).await;
                let mut complete_fut = None;
                {
                    let mut infos_guard = agent_infos.0.lock().unwrap();
                    let infos = infos_guard.get_mut(&endpoint).unwrap();
                    if infos.rc == 0 && infos.last_update.elapsed().as_secs() > 60 {
                        break;
                    }
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
                                writer.write(&serde_json::to_vec(&status.info).unwrap())
                            }
                            if let Some(completer) = completer {
                                complete_fut = Some(completer.complete(status.info));
                            } else {
                                infos.infos = ManualFuture::new_completed(status.info).shared();
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
                }
                if let Some(complete_fut) = complete_fut.take() {
                    complete_fut.await;
                }
                sleep(Duration::from_secs(60)).await;
            }
            agent_infos.0.lock().unwrap().remove(&endpoint);
        });

        AgentInfoFetcher {
            last_update: Instant::now(),
            rc: 1,
            infos: future.shared(),
        }
    }
}

fn info_path(endpoint: &Endpoint) -> CString {
    let mut hasher = ZwoHasher::default();
    endpoint.hash(&mut hasher);
    let mut path = format!(
        "/ddinf{}-{}",
        primary_sidecar_identifier(),
        BASE64_URL_SAFE_NO_PAD.encode(hasher.finish().to_ne_bytes()),
    );
    // datadog agent info, on macos we're restricted to 31 chars
    path.truncate(31); // should not be larger than 31 chars, but be sure.
    CString::new(path).unwrap()
}

pub struct AgentInfoReader {
    reader: OneWayShmReader<NamedShmHandle, CString>,
    info: Option<AgentInfoStruct>,
}

impl AgentInfoReader {
    pub fn new(endpoint: &Endpoint) -> AgentInfoReader {
        let path = info_path(endpoint);
        AgentInfoReader {
            reader: OneWayShmReader::new(open_named_shm(&path).ok(), path),
            info: None,
        }
    }

    pub fn read(&mut self) -> (bool, &Option<AgentInfoStruct>) {
        let (updated, data) = self.reader.read();
        if updated {
            match serde_json::from_slice(data) {
                Ok(info) => self.info = Some(info),
                Err(e) => error!("Failed deserializing the agent info: {e:?}"),
            }
        }
        (updated, &self.info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

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
