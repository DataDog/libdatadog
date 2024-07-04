// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    InstanceId, QueueId, RuntimeMetadata, SerializedTracerHeaderTags, SessionConfig, SidecarAction,
    SidecarInterfaceRequest, SidecarInterfaceResponse,
};
use datadog_ipc::platform::{Channel, FileBackedHandle, ShmHandle};
use datadog_ipc::transport::blocking::BlockingTransport;
use ddcommon::tag::Tag;
use dogstatsd_client::DogStatsDActionOwned;
use serde::Serialize;
use std::sync::Mutex;
use std::{
    borrow::Cow,
    io,
    time::{Duration, Instant},
};
use tracing::info;
use datadog_live_debugger::debugger_defs::DebuggerData;
use datadog_live_debugger::sender::DebuggerType;

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
            // Should happen only during the "reconnection" phase. During this phase the transport
            // is always closed.
            Err(_) => true,
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
/// * `remote_config_notify_function` (windows): a function pointer to be invoked
/// * `pid` (unix): the pid of the remote process
/// * `session_id` - The ID of the session.
/// * `config` - The configuration to be set.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn set_session_config(
    transport: &mut SidecarTransport,
    #[cfg(unix)] pid: libc::pid_t,
    #[cfg(windows)] remote_config_notify_function: *mut libc::c_void,
    session_id: String,
    config: &SessionConfig,
) -> io::Result<()> {
    #[cfg(unix)]
    let remote_config_notify_target = pid;
    #[cfg(windows)]
    let remote_config_notify_target =
        crate::service::remote_configs::RemoteConfigNotifyFunction(remote_config_notify_function);
    transport.send(SidecarInterfaceRequest::SetSessionConfig {
        session_id,
        remote_config_notify_target,
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
/// * `len` - The size of the shared memory data.
/// * `headers` - The serialized headers from the tracer.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn send_trace_v04_shm(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    handle: ShmHandle,
    len: usize,
    headers: SerializedTracerHeaderTags,
) -> io::Result<()> {
    transport.send(SidecarInterfaceRequest::SendTraceV04Shm {
        instance_id: instance_id.clone(),
        handle,
        len,
        headers,
    })
}

/// Sends raw data from shared memory to the debugger endpoint.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
/// * `queue_id` - The unique identifier for the trace context.
/// * `handle` - The handle to the shared memory.
/// * `debugger_type` - Whether it's log or diagnostic data.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn send_debugger_data_shm(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: QueueId,
    handle: ShmHandle,
    debugger_type: DebuggerType,
) -> io::Result<()> {
    transport.send(SidecarInterfaceRequest::SendDebuggerDataShm {
        instance_id: instance_id.clone(),
        queue_id,
        handle,
        debugger_type,
    })
}

/// Sends a collection of debugger payloads to the debugger endpoint.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
/// * `queue_id` - The unique identifier for the trace context.
/// * `payloads` - The payloads to be sent
///
/// # Returns
///
/// An `anyhow::Result<()>` indicating the result of the operation.
pub fn send_debugger_data_shm_vec(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: QueueId,
    payloads: Vec<datadog_live_debugger::debugger_defs::DebuggerPayload>,
) -> anyhow::Result<()> {
    if payloads.len() == 0 {
        return Ok(());
    }
    let debugger_type = DebuggerType::of_payload(&payloads[0]);
    
    struct SizeCount(usize);

    impl io::Write for SizeCount {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut size_serializer = serde_json::Serializer::new(SizeCount(0));
    payloads.serialize(&mut size_serializer).unwrap();

    let mut mapped = ShmHandle::new(size_serializer.into_inner().0)?.map()?;
    let mut serializer = serde_json::Serializer::new(mapped.as_slice_mut());
    payloads.serialize(&mut serializer).unwrap();

    Ok(send_debugger_data_shm(
        transport,
        instance_id,
        queue_id,
        mapped.into(),
        debugger_type,
    )?)
}

/// Sets the state of the current remote config operation.
/// The queue id is shared with telemetry and the associated data will be freed upon a
/// `Lifecycle::Stop` event.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
/// * `instance_id` - The ID of the instance.
/// * `queue_id` - The unique identifier for the action in the queue.
/// * `service_name` - The name of the service.
/// * `env_name` - The name of the environment.
/// * `app_version` - The metadata of the runtime.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn set_remote_config_data(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    service_name: String,
    env_name: String,
    app_version: String,
    global_tags: Vec<Tag>,
) -> io::Result<()> {
    transport.send(SidecarInterfaceRequest::SetRemoteConfigData {
        instance_id: instance_id.clone(),
        queue_id: *queue_id,
        service_name,
        env_name,
        app_version,
        global_tags,
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
    actions: Vec<DogStatsDActionOwned>,
) -> io::Result<()> {
    transport.send(SidecarInterfaceRequest::SendDogstatsdActions {
        instance_id: instance_id.clone(),
        actions,
    })
}

/// Sets x-datadog-test-session-token on all requests for the given session.
///
/// # Arguments
///
/// * `session_id` - The ID of the session.
/// * `token` - The session token.
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn set_test_session_token(
    transport: &mut SidecarTransport,
    session_id: String,
    token: String,
) -> io::Result<()> {
    transport.send(SidecarInterfaceRequest::SetTestSessionToken { session_id, token })
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

/// Flushes the outstanding traces.
///
/// # Arguments
///
/// * `transport` - The transport used for communication.
///
/// # Returns
///
/// An `io::Result<()>` indicating the result of the operation.
pub fn flush_traces(transport: &mut SidecarTransport) -> io::Result<()> {
    transport.call(SidecarInterfaceRequest::FlushTraces {})?;
    Ok(())
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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use crate::service::blocking::SidecarTransport;
    use datadog_ipc::platform::Channel;
    use std::net::Shutdown;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::Duration;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_reconnect() {
        let bind_addr = "/tmp/test_reconnect.sock";
        let _ = std::fs::remove_file(bind_addr);

        let listener = UnixListener::bind(bind_addr).expect("Cannot bind");
        let sock = UnixStream::connect_addr(&listener.local_addr().unwrap()).unwrap();

        let mut transport = SidecarTransport::from(Channel::from(sock.try_clone().unwrap()));
        assert!(!transport.is_closed());

        sock.shutdown(Shutdown::Both)
            .expect("shutdown function failed");
        assert!(transport.is_closed());

        transport.reconnect(|| {
            let new_sock = UnixStream::connect_addr(&listener.local_addr().unwrap()).unwrap();
            Some(Box::new(SidecarTransport::from(Channel::from(new_sock))))
        });
        assert!(!transport.is_closed());

        let _ = std::fs::remove_file(bind_addr);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_set_timeout() {
        let bind_addr = "/tmp/test_set_timeout.sock";
        let _ = std::fs::remove_file(bind_addr);

        let listener = UnixListener::bind(bind_addr).expect("Cannot bind");
        let sock = UnixStream::connect_addr(&listener.local_addr().unwrap()).unwrap();

        let mut transport = SidecarTransport::from(Channel::from(sock.try_clone().unwrap()));
        assert_eq!(
            Duration::default(),
            sock.read_timeout().unwrap().unwrap_or_default()
        );
        assert_eq!(
            Duration::default(),
            sock.write_timeout().unwrap().unwrap_or_default()
        );

        transport
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set_read_timeout function failed");
        transport
            .set_write_timeout(Some(Duration::from_millis(300)))
            .expect("set_write_timeout function failed");

        assert_eq!(
            Duration::from_millis(200),
            sock.read_timeout().unwrap().unwrap_or_default()
        );
        assert_eq!(
            Duration::from_millis(300),
            sock.write_timeout().unwrap().unwrap_or_default()
        );

        let _ = std::fs::remove_file(bind_addr);
    }
}
