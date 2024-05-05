// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::agent_remote_config::AgentRemoteConfigWriter;
use crate::log::TemporarilyRetainedMapStats;
use crate::service::{
    telemetry::enqueued_telemetry_stats::EnqueuedTelemetryStats, RuntimeMetadata,
    SerializedTracerHeaderTags, SidecarAction, SidecarInterfaceRequest, SidecarInterfaceResponse,
};
use anyhow::Result;
use datadog_ipc::platform::NamedShmHandle;
use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::SendData;
use ddcommon::Endpoint;
use ddtelemetry::data;
use ddtelemetry::worker::TelemetryWorkerStats;
use futures::future::join_all;
use manual_future::{ManualFuture, ManualFutureCompleter};
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, VecSkipError};
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::iter::zip;
use std::ops::DerefMut;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::select;
use tokio::task::{JoinError, JoinHandle};
use tracing::{debug, error, info};

#[derive(Serialize, Deserialize)]
pub struct SidecarStats {
    pub trace_flusher: TraceFlusherStats,
    pub sessions: u32,
    pub session_counter_size: u32,
    pub runtimes: u32,
    pub apps: u32,
    pub active_apps: u32,
    pub enqueued_apps: u32,
    pub enqueued_telemetry_data: EnqueuedTelemetryStats,
    pub telemetry_metrics_contexts: u32,
    pub telemetry_worker: TelemetryWorkerStats,
    pub telemetry_worker_errors: u32,
    pub log_writer: TemporarilyRetainedMapStats,
    pub log_filter: TemporarilyRetainedMapStats,
}

#[derive(Serialize, Deserialize)]
pub struct TraceFlusherStats {
    pub agent_config_allocated_shm: u32,
    pub agent_config_writers: u32,
    pub agent_configs_last_used_entries: u32,
    pub send_data_size: u32,
}

// TODO-EK: Re-eval access scope before merging
#[serde_as]
#[derive(Deserialize)]
pub struct ComposerPackages {
    #[serde_as(as = "VecSkipError<_>")]
    pub packages: Vec<data::Dependency>,
}

#[derive(Default)]
struct TraceSendData {
    pub send_data: Vec<SendData>,
    pub send_data_size: usize,
    pub force_flush: Option<ManualFutureCompleter<()>>,
}

impl TraceSendData {
    pub fn flush(&mut self) {
        if let Some(force_flush) = self.force_flush.take() {
            debug!(
                "Emitted flush for traces with {} bytes in send_data buffer",
                self.send_data_size
            );
            tokio::spawn(async move {
                force_flush.complete(()).await;
            });
        }
    }
}

#[derive(Default)]
struct TraceFlusherData {
    pub traces: TraceSendData,
    pub flusher: Option<JoinHandle<()>>,
}

struct AgentRemoteConfig {
    pub writer: AgentRemoteConfigWriter<NamedShmHandle>,
    pub last_write: Instant,
}

#[derive(Default)]
struct AgentRemoteConfigs {
    pub writers: HashMap<Endpoint, AgentRemoteConfig>,
    pub last_used: BTreeMap<Instant, Endpoint>,
}

#[derive(Default)]
pub struct TraceFlusher {
    inner: Mutex<TraceFlusherData>,
    pub interval: AtomicU64,
    pub min_force_flush_size: AtomicU32,
    pub min_force_drop_size: AtomicU32, // put a limit on memory usage
    remote_config: Mutex<AgentRemoteConfigs>,
}

impl TraceFlusher {
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
                                // TODO: Retries when sending to intake
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

    pub fn enqueue(self: &Arc<Self>, data: SendData) {
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

    pub async fn join(&self) -> Result<(), JoinError> {
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

    pub fn stats(&self) -> TraceFlusherStats {
        let rc = self.remote_config.lock().unwrap();
        TraceFlusherStats {
            agent_config_allocated_shm: rc.writers.values().map(|r| r.writer.size() as u32).sum(),
            agent_config_writers: rc.writers.len() as u32,
            agent_configs_last_used_entries: rc.last_used.len() as u32,
            send_data_size: self.inner.lock().unwrap().traces.send_data_size as u32,
        }
    }
}

pub mod blocking {
    use datadog_ipc::platform::ShmHandle;
    use std::{
        borrow::Cow,
        io,
        time::{Duration, Instant},
    };

    use datadog_ipc::transport::blocking::BlockingTransport;

    use crate::interface::{SerializedTracerHeaderTags, SidecarAction};
    use crate::service::{InstanceId, QueueId, SessionConfig};

    use super::{RuntimeMetadata, SidecarInterfaceRequest, SidecarInterfaceResponse};

    pub type SidecarTransport =
        BlockingTransport<SidecarInterfaceResponse, SidecarInterfaceRequest>;

    pub fn shutdown_runtime(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::ShutdownRuntime {
            instance_id: instance_id.clone(),
        })
    }

    pub fn shutdown_session(
        transport: &mut SidecarTransport,
        session_id: String,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::ShutdownSession { session_id })
    }

    pub fn enqueue_actions(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
        queue_id: &QueueId,
        actions: Vec<SidecarAction>,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::EnqueueActions {
            instance_id: instance_id.clone(),
            queue_id: *queue_id,
            actions,
        })
    }

    pub fn register_service_and_flush_queued_actions(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
        queue_id: &QueueId,
        runtime_metadata: &RuntimeMetadata,
        service_name: Cow<str>,
        env_name: Cow<str>,
    ) -> io::Result<()> {
        transport.send(
            SidecarInterfaceRequest::RegisterServiceAndFlushQueuedActions {
                instance_id: instance_id.clone(),
                queue_id: *queue_id,
                meta: runtime_metadata.clone(),
                service_name: service_name.into_owned(),
                env_name: env_name.into_owned(),
            },
        )
    }

    pub fn set_session_config(
        transport: &mut SidecarTransport,
        session_id: String,
        config: &SessionConfig,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::SetSessionConfig {
            session_id,
            config: config.clone(),
        })
    }

    pub fn send_trace_v04_bytes(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::SendTraceV04Bytes {
            instance_id: instance_id.clone(),
            data,
            headers,
        })
    }

    pub fn send_trace_v04_shm(
        transport: &mut SidecarTransport,
        instance_id: &InstanceId,
        handle: ShmHandle,
        headers: SerializedTracerHeaderTags,
    ) -> io::Result<()> {
        transport.send(SidecarInterfaceRequest::SendTraceV04Shm {
            instance_id: instance_id.clone(),
            handle,
            headers,
        })
    }

    pub fn dump(transport: &mut SidecarTransport) -> io::Result<String> {
        let res = transport.call(SidecarInterfaceRequest::Dump {})?;
        if let SidecarInterfaceResponse::Dump(dump) = res {
            Ok(dump)
        } else {
            Ok("".to_string())
        }
    }

    pub fn stats(transport: &mut SidecarTransport) -> io::Result<String> {
        let res = transport.call(SidecarInterfaceRequest::Stats {})?;
        if let SidecarInterfaceResponse::Stats(stats) = res {
            Ok(stats)
        } else {
            Ok("".to_string())
        }
    }

    pub fn ping(transport: &mut SidecarTransport) -> io::Result<Duration> {
        let start = Instant::now();
        transport.call(SidecarInterfaceRequest::Ping {})?;

        Ok(Instant::now()
            .checked_duration_since(start)
            .unwrap_or_default())
    }
}
