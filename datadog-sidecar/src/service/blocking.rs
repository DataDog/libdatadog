// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    DynamicInstrumentationConfigState, InstanceId, QueueId, SerializedTracerHeaderTags,
    SessionConfig, SidecarAction,
};
use crate::service::sender::SidecarSender;
use crate::service::sidecar_interface::{SidecarInterfaceChannel, SidecarInterfaceRequest};
use datadog_ipc::platform::{FileBackedHandle, ShmHandle};
use datadog_ipc::SeqpacketConn;
use datadog_live_debugger::debugger_defs::DebuggerPayload;
use datadog_live_debugger::sender::DebuggerType;
use libdd_common::tag::Tag;
use libdd_dogstatsd_client::DogStatsDActionOwned;
use serde::Serialize;
use std::sync::Mutex;
use std::{
    io,
    time::{Duration, Instant},
};
use tracing::{info, warn};

/// `SidecarTransport` wraps a [`SidecarSender`] with transparent reconnection support.
///
/// This transport is used for communication between different parts of the sidecar service.
/// It is a blocking transport (all operations block the current thread).
pub struct SidecarTransport {
    pub inner: Mutex<SidecarSender>,
    /// If provided, whenever a connection error is encountered, the connection will be
    /// attempted to be re-established by calling this function.
    pub reconnect_fn: Option<Box<dyn Fn() -> Option<Box<SidecarTransport>>>>,
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

        #[allow(clippy::unwrap_used)]
        if transport.channel.0.is_closed() {
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

    pub fn set_read_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        lock_sender(self)?.set_read_timeout(d)
    }

    pub fn set_write_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        lock_sender(self)?.set_write_timeout(d)
    }

    pub fn ensure_alive(&mut self) {
        let closed = match self.inner.lock() {
            Ok(guard) => guard.channel.0.is_closed(),
            Err(_) => return,
        };
        if closed {
            if let Some(ref reconnect) = self.reconnect_fn {
                warn!("The sidecar transport is closed. Reconnecting... This generally indicates a problem with the sidecar, most likely a crash. Check the logs / core dump locations and possibly report a bug.");
                if let Some(n) = reconnect() {
                    if let Ok(mut guard) = self.inner.lock() {
                        if let Ok(new) = n.inner.into_inner() {
                            *guard = new;
                        }
                    }
                }
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        match self.inner.lock() {
            Ok(t) => t.channel.0.is_closed(),
            // Should happen only during the "reconnection" phase. During this phase the transport
            // is always considered closed.
            Err(_) => true,
        }
    }

    fn with_retry<F, V>(&mut self, f: F) -> io::Result<V>
    where
        F: Fn(&mut SidecarSender) -> io::Result<V>,
    {
        let mut inner = match self.inner.lock() {
            Ok(t) => t,
            Err(e) => return Err(io::Error::other(e.to_string())),
        };
        match f(&mut inner) {
            Ok(ret) => Ok(ret),
            Err(e) => {
                if e.kind() == io::ErrorKind::BrokenPipe
                    || e.kind() == io::ErrorKind::ConnectionReset
                {
                    if let Some(ref reconnect) = self.reconnect_fn {
                        warn!("The sidecar transport is closed. Reconnecting... This generally indicates a problem with the sidecar, most likely a crash. Check the logs / core dump locations and possibly report a bug.");
                        *inner = match reconnect() {
                            None => return Err(e),
                            #[allow(clippy::unwrap_used)]
                            Some(n) => n.inner.into_inner().unwrap(),
                        };
                        f(&mut inner)
                    } else {
                        Err(e)
                    }
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Send garbage data (used in tests to verify error handling).
    pub fn send_garbage(&mut self) -> io::Result<()> {
        match self.inner.lock() {
            Ok(mut c) => c
                .channel
                .0
                .send_blocking(&mut vec![0xDE, 0xAD, 0xBE, 0xEF], &[]),
            Err(e) => Err(io::Error::other(e.to_string())),
        }
    }
}

impl From<SeqpacketConn> for SidecarTransport {
    fn from(conn: SeqpacketConn) -> Self {
        SidecarTransport {
            inner: Mutex::new(SidecarSender::new(SidecarInterfaceChannel::new(conn))),
            reconnect_fn: None,
        }
    }
}

fn lock_sender(
    transport: &mut SidecarTransport,
) -> io::Result<std::sync::MutexGuard<'_, SidecarSender>> {
    transport.ensure_alive();
    transport
        .inner
        .lock()
        .map_err(|e| io::Error::other(e.to_string()))
}

/// Shuts down a runtime.
pub fn shutdown_runtime(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
) -> io::Result<()> {
    lock_sender(transport)?.shutdown_runtime(instance_id.clone());
    Ok(())
}

/// Shuts down a session.
pub fn shutdown_session(transport: &mut SidecarTransport, session_id: String) -> io::Result<()> {
    lock_sender(transport)?.shutdown_session(session_id);
    Ok(())
}

/// Enqueues a list of actions to be performed.
///
/// Uses `with_retry`: if the connection is broken the transport reconnects and the actions
/// are retried once on the new connection, so that telemetry/lifecycle events are not lost
/// when the sidecar crashes and restarts.
pub fn enqueue_actions(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    actions: Vec<SidecarAction>,
) -> io::Result<()> {
    // Pre-serialize once so the Fn closure can borrow the bytes for both the initial
    // attempt and the reconnect retry without needing SidecarAction: Clone.
    let req = SidecarInterfaceRequest::EnqueueActions {
        instance_id: instance_id.clone(),
        queue_id: *queue_id,
        actions,
    };
    let data = datadog_ipc::codec::encode(req.discriminant(), &req);
    transport.with_retry(|s| s.drain_and_send_raw_blocking(&data))
}

/// Sets the configuration for a session.
pub fn set_session_config(
    transport: &mut SidecarTransport,
    session_id: String,
    #[cfg(windows)]
    remote_config_notify_function: crate::service::remote_configs::RemoteConfigNotifyFunction,
    config: &SessionConfig,
    is_fork: bool,
) -> io::Result<()> {
    lock_sender(transport)?.set_session_config(
        session_id,
        #[cfg(windows)]
        remote_config_notify_function,
        config.clone(),
        is_fork,
    );
    Ok(())
}

/// Updates the process tags for an existing session.
pub fn set_session_process_tags(
    transport: &mut SidecarTransport,
    session_id: String,
    process_tags: Vec<Tag>,
) -> io::Result<()> {
    lock_sender(transport)?.set_session_process_tags(session_id, process_tags);
    Ok(())
}

/// Sends a trace as bytes.
pub fn send_trace_v04_bytes(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    data: Vec<u8>,
    headers: SerializedTracerHeaderTags,
) -> io::Result<()> {
    lock_sender(transport)?.send_trace_v04_bytes(instance_id.clone(), data, headers);
    Ok(())
}

/// Sends a trace via shared memory.
pub fn send_trace_v04_shm(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    handle: ShmHandle,
    len: usize,
    headers: SerializedTracerHeaderTags,
) -> io::Result<()> {
    lock_sender(transport)?.send_trace_v04_shm(instance_id.clone(), handle, len, headers);
    Ok(())
}

/// Sends raw data from shared memory to the debugger endpoint.
pub fn send_debugger_data_shm(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: QueueId,
    handle: ShmHandle,
    debugger_type: DebuggerType,
) -> io::Result<()> {
    lock_sender(transport)?.send_debugger_data_shm(
        instance_id.clone(),
        queue_id,
        handle,
        debugger_type,
    );
    Ok(())
}

/// Sends a collection of debugger payloads to the debugger endpoint via shared memory.
pub fn send_debugger_data_shm_vec(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: QueueId,
    payloads: Vec<DebuggerPayload>,
) -> anyhow::Result<()> {
    if payloads.is_empty() {
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

    payloads.serialize(&mut size_serializer)?;

    let mut mapped = ShmHandle::new(size_serializer.into_inner().0)?.map()?;
    let mut serializer = serde_json::Serializer::new(mapped.as_slice_mut());

    payloads.serialize(&mut serializer)?;

    Ok(send_debugger_data_shm(
        transport,
        instance_id,
        queue_id,
        mapped.into(),
        debugger_type,
    )?)
}

/// Submits debugger diagnostics.
pub fn send_debugger_diagnostics(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: QueueId,
    diagnostics_payload: DebuggerPayload,
) -> io::Result<()> {
    lock_sender(transport)?.send_debugger_diagnostics(
        instance_id.clone(),
        queue_id,
        serde_json::to_vec(&diagnostics_payload)?,
    );
    Ok(())
}

/// Acquire an exception hash rate limiter
pub fn acquire_exception_hash_rate_limiter(
    transport: &mut SidecarTransport,
    exception_hash: u64,
    granularity: Duration,
) -> io::Result<()> {
    lock_sender(transport)?.acquire_exception_hash_rate_limiter(exception_hash, granularity);
    Ok(())
}

/// Sets the state of the current remote config operation.
#[allow(clippy::too_many_arguments)]
pub fn set_universal_service_tags(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    service_name: String,
    env_name: String,
    app_version: String,
    global_tags: Vec<Tag>,
    dynamic_instrumentation_state: DynamicInstrumentationConfigState,
) -> io::Result<()> {
    lock_sender(transport)?.set_universal_service_tags(
        instance_id.clone(),
        *queue_id,
        service_name,
        env_name,
        app_version,
        global_tags,
        dynamic_instrumentation_state,
    );
    Ok(())
}

/// Sets request state which do not directly affect the RC connection.
pub fn set_request_config(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    dynamic_instrumentation_state: DynamicInstrumentationConfigState,
) -> io::Result<()> {
    lock_sender(transport)?.set_request_config(
        instance_id.clone(),
        *queue_id,
        dynamic_instrumentation_state,
    );
    Ok(())
}

/// Sends DogStatsD actions.
pub fn send_dogstatsd_actions(
    transport: &mut SidecarTransport,
    instance_id: &InstanceId,
    actions: Vec<DogStatsDActionOwned>,
) -> io::Result<()> {
    lock_sender(transport)?.send_dogstatsd_actions(instance_id.clone(), actions);
    Ok(())
}

/// Sets x-datadog-test-session-token on all requests for the given session.
pub fn set_test_session_token(
    transport: &mut SidecarTransport,
    session_id: String,
    token: String,
) -> io::Result<()> {
    lock_sender(transport)?.set_test_session_token(session_id, token);
    Ok(())
}

/// Dumps the current state of the service.
pub fn dump(transport: &mut SidecarTransport) -> io::Result<String> {
    transport.with_retry(|s| s.dump().map_err(|e| io::Error::other(e.to_string())))
}

/// Retrieves the current statistics of the service.
pub fn stats(transport: &mut SidecarTransport) -> io::Result<String> {
    transport.with_retry(|s| s.stats().map_err(|e| io::Error::other(e.to_string())))
}

/// Flushes the outstanding traces.
pub fn flush_traces(transport: &mut SidecarTransport) -> io::Result<()> {
    transport.with_retry(|s| s.flush_traces())
}

/// Sends a ping to the service.
pub fn ping(transport: &mut SidecarTransport) -> io::Result<Duration> {
    let start = Instant::now();
    transport.with_retry(|s| s.ping())?;
    Ok(start.elapsed())
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use crate::service::blocking::SidecarTransport;
    use datadog_ipc::{SeqpacketConn, SeqpacketListener};

    use tempfile::tempdir;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_reconnect() {
        let tmpdir = tempdir().unwrap();
        let socket_path = tmpdir.path().join("test.sock");

        let listener = SeqpacketListener::bind(&socket_path).expect("Cannot bind");
        let conn = SeqpacketConn::connect(&socket_path).unwrap();
        // Accept so the server holds liveness_read; dropping server_conn triggers POLLHUP.
        let server_conn = listener.try_accept().expect("try_accept");

        let mut transport = SidecarTransport::from(conn);
        assert!(!transport.is_closed());

        // Drop the accepted conn: closes liveness_read → POLLHUP on liveness_write.
        drop(server_conn);
        drop(listener);
        // Force close detection by triggering an I/O operation.
        let _ = transport.send_garbage();
        assert!(transport.is_closed());

        let socket_path2 = socket_path.clone();
        let listener2 = SeqpacketListener::bind(&socket_path2).expect("Cannot rebind");
        transport.reconnect(|| {
            let new_conn = SeqpacketConn::connect(&socket_path2).ok()?;
            Some(Box::new(SidecarTransport::from(new_conn)))
        });
        assert!(!transport.is_closed());
        drop(listener2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_connection_basic() {
        let tmpdir = tempdir().unwrap();
        let socket_path = tmpdir.path().join("test_basic.sock");

        let listener = SeqpacketListener::bind(&socket_path).expect("Cannot bind");
        let conn = SeqpacketConn::connect(&socket_path).unwrap();

        let transport = SidecarTransport::from(conn);
        assert!(!transport.is_closed());
        drop(transport);
        drop(listener);
    }
}
