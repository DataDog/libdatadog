// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    InstanceId, QueueId, RuntimeMetadata, SerializedTracerHeaderTags, SessionConfig, SidecarAction,
    SidecarInterfaceRequest, SidecarInterfaceResponse,
};
use crate::dogstatsd::DogStatsDAction;
use datadog_ipc::platform::{Channel, ShmHandle};
use datadog_ipc::transport::blocking::BlockingTransport;
use std::sync::Mutex;
use std::{
    borrow::Cow,
    io,
    time::{Duration, Instant},
};
use tracing::info;

/// `SidecarTransport` is a wrapper around a BlockingTransport struct from the `datadog_ipc` crate
/// that handles transparent reconnection.
/// It is used for sending `SidecarInterfaceRequest` and receiving `SidecarInterfaceResponse`.
///
/// This transport is used for communication between different parts of the sidecar service.
/// It is a blocking transport, meaning that it will block the current thread until the operation is
/// complete.
pub struct SidecarTransport {
    pub inner: Mutex<BlockingTransport<SidecarInterfaceResponse, SidecarInterfaceRequest>>,
}

impl SidecarTransport {
    pub fn reconnect<F>(&mut self, factory: F)
    where
        F: FnOnce() -> Option<Box<SidecarTransport>>,
    {
        let mut transport = match self.inner.lock() {
            Ok(t) => t,
            Err(_) => return,
        };
        if transport.is_closed() {
            info!("The sidecar transport is closed. Reconnecting...");
            let new = match factory() {
                None => return,
                Some(n) => n.inner.into_inner(),
            };
            if new.is_err() {
                return;
            }
            *transport = new.unwrap();
        }
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self.inner.lock() {
            Ok(mut t) => t.set_read_timeout(timeout),
            Err(e) => Err(io::Error::new(io::ErrorKind::Other, e.to_string())),
        }
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self.inner.lock() {
            Ok(mut t) => t.set_write_timeout(timeout),
            Err(e) => Err(io::Error::new(io::ErrorKind::Other, e.to_string())),
        }
    }

    pub fn is_closed(&self) -> bool {
        match self.inner.lock() {
            Ok(t) => t.is_closed(),
            Err(_) => true, // Well... what can we do?
        }
    }

    pub fn send(&mut self, item: SidecarInterfaceRequest) -> io::Result<()> {
        match self.inner.lock() {
            Ok(mut t) => t.send(item),
            Err(e) => Err(io::Error::new(io::ErrorKind::Other, e.to_string())),
        }
    }

    pub fn call(&mut self, item: SidecarInterfaceRequest) -> io::Result<SidecarInterfaceResponse> {
        match self.inner.lock() {
            Ok(mut t) => t.call(item),
            Err(e) => Err(io::Error::new(io::ErrorKind::Other, e.to_string())),
        }
    }
}

impl From<Channel> for SidecarTransport {
    fn from(c: Channel) -> Self {
        SidecarTransport {
            inner: Mutex::new(c.into()),
        }
    }
}

/// Shuts down a runtime.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn shutdown_runtime(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
) -> io::Result<()> {
    transport.send(SidecarInterfaceRequest::ShutdownRuntime {
        instance_id: instance_id.clone(),
    })
}

/// Shuts down a session.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `session_id` - The ID of the session.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn shutdown_session(transport: &mut SidecarTransport, session_id: String) -> io::Result<()> {
    transport.send(SidecarInterfaceRequest::ShutdownSession { session_id })
}

/// Enqueues a list of actions to be performed.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
/// * `queue_id` - The unique identifier for the action in the queue.
/// * `actions` - The action type being enqueued.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
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

/// Registers a service and flushes any queued actions.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
/// * `queue_id` - The unique identifier for the action in the queue.
/// * `runtime_metadata` - The metadata of the runtime.
/// * `service_name` - The name of the service.
/// * `env_name` - The name of the environment.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
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

/// Sets the configuration for a session.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `session_id` - The ID of the session.
/// * `config` - The configuration to be set.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
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

/// Sends a trace as bytes.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
/// * `data` - The trace data serialized as bytes.
/// * `headers` - The serialized headers from the tracer.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
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

/// Sends a trace via shared memory.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
/// * `handle` - The handle to the shared memory.
/// * `headers` - The serialized headers from the tracer.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
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

/// Sends DogStatsD actions.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
/// * `actions` - The DogStatsD actions to send.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn send_dogstatsd_actions(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    actions: Vec<DogStatsDAction>,
) -> io::Result<()> {
    transport.send(SidecarInterfaceRequest::SendDogstatsdActions {
        instance_id: instance_id.clone(),
        actions,
    })
}

/// Dumps the current state of the service.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
///
/// # Returns
///
/// An `io::Result<String>` representing the current state of the service.
pub fn dump(transport: &mut SidecarTransport) -> io::Result<String> {
    let res = transport.call(SidecarInterfaceRequest::Dump {})?;
    if let SidecarInterfaceResponse::Dump(dump) = res {
        Ok(dump)
    } else {
        Ok(String::default())
    }
}

/// Retrieves the current statistics of the service.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
///
/// # Returns
///
/// An `io::Result<String>` representing the current statistics of the service.
pub fn stats(transport: &mut SidecarTransport) -> io::Result<String> {
    let res = transport.call(SidecarInterfaceRequest::Stats {})?;
    if let SidecarInterfaceResponse::Stats(stats) = res {
        Ok(stats)
    } else {
        Ok(String::default())
    }
}

/// Sends a ping to the service.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
///
/// # Returns
///
/// An `io::Result<Duration>` representing the round-trip time of the ping.
pub fn ping(transport: &mut SidecarTransport) -> io::Result<Duration> {
    let start = Instant::now();
    transport.call(SidecarInterfaceRequest::Ping {})?;

    Ok(Instant::now()
        .checked_duration_since(start)
        .unwrap_or_default())
}
