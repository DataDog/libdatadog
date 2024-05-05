// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use serde_with::{serde_as, VecSkipError};

use ddtelemetry::data;
use ddtelemetry::worker::TelemetryWorkerStats;

use crate::log::TemporarilyRetainedMapStats;
use crate::service::{
    telemetry::enqueued_telemetry_stats::EnqueuedTelemetryStats,
    tracing::trace_flusher::TraceFlusherStats, RuntimeMetadata, SerializedTracerHeaderTags,
    SidecarAction, SidecarInterfaceRequest, SidecarInterfaceResponse,
};

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

// TODO-EK: Re-eval access scope before merging
#[serde_as]
#[derive(Deserialize)]
pub struct ComposerPackages {
    #[serde_as(as = "VecSkipError<_>")]
    pub packages: Vec<data::Dependency>,
}

pub mod blocking {
    use std::{
        borrow::Cow,
        io,
        time::{Duration, Instant},
    };

    use datadog_ipc::platform::ShmHandle;
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
