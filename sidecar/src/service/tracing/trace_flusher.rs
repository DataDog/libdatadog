// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::TraceSendData;
use crate::agent_remote_config::AgentRemoteConfigWriter;
use datadog_ipc::platform::NamedShmHandle;
use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::SendData;
use ddcommon::Endpoint;
use futures::future::join_all;
use manual_future::ManualFuture;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::iter::zip;
use std::ops::DerefMut;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::select;
use tokio::task::{JoinError, JoinHandle};
use tracing::{debug, error, info};

/// `TraceFlusherStats` holds stats of the trace flusher like the count of allocated shared memory
/// for agent config, agent config writers, last used entries in agent configs, and the size of send
/// data.
#[derive(Serialize, Deserialize)]
pub(crate) struct TraceFlusherStats {
    pub(crate) agent_config_allocated_shm: u32,
    pub(crate) agent_config_writers: u32,
    pub(crate) agent_configs_last_used_entries: u32,
    pub(crate) send_data_size: u32,
}

struct AgentRemoteConfig {
    writer: AgentRemoteConfigWriter<NamedShmHandle>,
    last_write: Instant,
}

#[derive(Default)]
struct AgentRemoteConfigs {
    writers: HashMap<Endpoint, AgentRemoteConfig>,
    last_used: BTreeMap<Instant, Endpoint>,
}
#[derive(Default)]
struct TraceFlusherData {
    traces: TraceSendData,
    flusher: Option<JoinHandle<()>>,
}

/// `TraceFlusher` is a structure that manages the flushing of traces.
/// It contains the traces to be sent, the flusher task, the interval for flushing,
/// the minimum sizes for force flushing and dropping, and the remote configs.
#[derive(Default)]
pub(crate) struct TraceFlusher {
    inner: Mutex<TraceFlusherData>,
    pub(crate) interval: AtomicU64,
    pub(crate) min_force_flush_size: AtomicU32,
    pub(crate) min_force_drop_size: AtomicU32, // put a limit on memory usage
    remote_config: Mutex<AgentRemoteConfigs>,
}

impl TraceFlusher {
    /// Enqueue a `SendData` to the traces and triggers a flush if the size exceeds the minimum
    /// force flush size.
    ///
    /// # Arguments
    ///
    /// * `data` - A `SendData` instance that needs to be added to the traces.
    pub(crate) fn enqueue(self: &Arc<Self>, data: SendData) {
        let mut flush_data = self.inner.lock().unwrap();
        let flush_data = flush_data.deref_mut();

        flush_data.traces.send_data_size += data.size();

        if flush_data.traces.send_data_size
            > self.min_force_drop_size.load(Ordering::Relaxed) as usize
        {
            return;
        }

        flush_data.traces.send_data.push(data);
        if flush_data.flusher.is_none() {
            let (force_flush, completer) = ManualFuture::new();
            flush_data.flusher = Some(self.clone().start_trace_flusher(force_flush));
            flush_data.traces.force_flush = Some(completer);
        }
        if flush_data.traces.send_data_size
            > self.min_force_flush_size.load(Ordering::Relaxed) as usize
        {
            flush_data.traces.flush();
        }
    }

    /// Join the flusher task and flush the remaining traces.
    ///
    /// # Returns
    ///
    /// * A `Result` which is `Ok` if the flusher task successfully joins, or `Err` if the flusher
    ///   task panics.
    /// If the flusher task is not running, it returns `Ok`.
    pub(crate) async fn join(&self) -> anyhow::Result<(), JoinError> {
        let flusher = {
            let mut flush_data = self.inner.lock().unwrap();
            self.interval.store(0, Ordering::SeqCst);
            flush_data.traces.flush();
            flush_data.deref_mut().flusher.take()
        };
        if let Some(flusher) = flusher {
            flusher.await
        } else {
            Ok(())
        }
    }

    /// Get the statistics of the trace flusher.
    ///
    /// # Returns
    ///
    /// * A `TraceFlusherStats` instance that contains the statistics of the trace flusher.
    ///
    /// This method retrieves the statistics of the trace flusher, including the count of allocated
    /// shared memory for agent config, agent config writers, last used entries in agent
    /// configs, and the size of send data.
    pub(crate) fn stats(&self) -> TraceFlusherStats {
        let rc = self.remote_config.lock().unwrap();
        TraceFlusherStats {
            agent_config_allocated_shm: rc.writers.values().map(|r| r.writer.size() as u32).sum(),
            agent_config_writers: rc.writers.len() as u32,
            agent_configs_last_used_entries: rc.last_used.len() as u32,
            send_data_size: self.inner.lock().unwrap().traces.send_data_size as u32,
        }
    }
    fn write_remote_configs(&self, endpoint: Endpoint, contents: Vec<u8>) {
        let configs = &mut *self.remote_config.lock().unwrap();

        let mut entry = configs.writers.entry(endpoint.clone());
        let writer = match entry {
            Entry::Occupied(ref mut entry) => entry.get_mut(),
            Entry::Vacant(entry) => {
                if let Ok(writer) = crate::agent_remote_config::new_writer(&endpoint) {
                    entry.insert(AgentRemoteConfig {
                        writer,
                        last_write: Instant::now(),
                    })
                } else {
                    return;
                }
            }
        };
        writer.writer.write(contents.as_slice());

        let now = Instant::now();
        let last = writer.last_write;
        writer.last_write = now;

        configs.last_used.remove(&last);
        configs.last_used.insert(now, endpoint);

        while let Some((&time, _)) = configs.last_used.iter().next() {
            if time + Duration::new(50, 0) > Instant::now() {
                break;
            }
            configs
                .writers
                .remove(&configs.last_used.remove(&time).unwrap());
        }
    }

    fn start_trace_flusher(self: Arc<Self>, mut force_flush: ManualFuture<()>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                select! {
                    _ = tokio::time::sleep(Duration::from_millis(
                        self.interval.load(Ordering::Relaxed),
                    )) => {},
                    _ = force_flush => {},
                }

                debug!(
                    "Start flushing {} bytes worth of traces",
                    self.inner.lock().unwrap().traces.send_data_size
                );

                let (new_force_flush, completer) = ManualFuture::new();
                force_flush = new_force_flush;

                let trace_buffer = std::mem::replace(
                    &mut self.inner.lock().unwrap().traces,
                    TraceSendData {
                        send_data: vec![],
                        send_data_size: 0,
                        force_flush: Some(completer),
                    },
                )
                .send_data;
                let mut futures: Vec<_> = Vec::new();
                let mut intake_target: Vec<_> = Vec::new();
                for send_data in trace_utils::coalesce_send_data(trace_buffer).into_iter() {
                    intake_target.push(send_data.target.clone());
                    futures.push(send_data.send());
                }
                for (endpoint, response) in zip(intake_target, join_all(futures).await) {
                    match response {
                        Ok(response) => {
                            if endpoint.api_key.is_none() {
                                // not when intake
                                match hyper::body::to_bytes(response.into_body()).await {
                                    Ok(body_bytes) => self.write_remote_configs(
                                        endpoint.clone(),
                                        body_bytes.to_vec(),
                                    ),
                                    Err(e) => error!("Error receiving agent configuration: {e:?}"),
                                }
                            }
                            info!("Successfully flushed traces to {}", endpoint.url);
                        }
                        Err(e) => {
                            error!("Error sending trace: {e:?}");
                            if endpoint.api_key.is_some() {
                                // TODO: APMSP-1020 Retries when sending to intake
                            }
                        }
                    }
                }

                let mut data = self.inner.lock().unwrap();
                let data = data.deref_mut();
                if data.traces.send_data.is_empty() {
                    data.flusher = None;
                    break;
                }
            }
        })
    }
}
