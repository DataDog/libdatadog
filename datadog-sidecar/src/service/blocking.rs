// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    ApplicationConfig, DynamicInstrumentationConfigState, InstanceId, SerializedTracerHeaderTags,
    SessionConfig, SidecarAction, SidecarFlushOptions,
};
use crate::service::ffe_submission::FfeSubmissionStatus;
use crate::service::sender::SidecarSender;
use crate::service::sidecar_interface::{
    SidecarInterfaceChannel, SidecarInterfaceClientRequest, SidecarInterfaceRequest,
};
use libdd_common::tag::Tag;
use libdd_dogstatsd_client::DogStatsDActionOwned;
use libdd_ipc::SeqpacketConn;
use libdd_ipc::codec::DecodeError;
use libdd_ipc::platform::{FileBackedHandle, ShmHandle};
use libdd_live_debugger::debugger_defs::DebuggerPayload;
use libdd_live_debugger::sender::DebuggerType;
use libdd_telemetry::metrics::MetricContext;
use libdd_trace_utils::trace_utils::TracerGenericTags;
use serde::Serialize;
use std::cell::Cell;
use std::{
    io,
    time::{Duration, Instant},
};
use tracing::warn;

/// `SidecarTransport` wraps a [`SidecarSender`] with transparent reconnection support.
///
/// This transport is used for communication between different parts of the sidecar service.
/// It is owned by a single client thread: the sidecar binds state to the connection,
/// so it must not be shared between threads which process requests concurrently.
/// Ordinary operations may block and reconnect. FFE submission bypasses reconnection
/// and never waits for transport capacity or delivery; platform synchronization may wait.
pub struct SidecarTransport {
    pub inner: SidecarSender,
    /// If provided, whenever a connection error is encountered, the connection will be
    /// attempted to be re-established by calling this function. The connection state is carried
    /// over to the new connection.
    pub reconnect_fn: Option<Box<dyn Fn() -> Option<Box<SidecarTransport>>>>,
    /// Backpressure limits, applied to every new connection as well.
    backpressure: Option<(usize, u64)>,
}

impl SidecarTransport {
    /// Returns the PID of the remote peer (the sidecar/daemon process).
    ///
    /// Uses the platform's peer credential mechanism (SO_PEERCRED on Linux,
    /// LOCAL_PEERPID on macOS) on the underlying IPC socket.
    pub fn peer_pid(&self) -> io::Result<u32> {
        let creds = self.inner.channel.0.conn.peer_credentials()?;
        Ok(creds.pid)
    }

    #[cfg(unix)]
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.inner.channel.0.conn.as_raw_fd()
    }

    /// See [`libdd_ipc::SeqpacketConn::close_inherited_fds`].
    ///
    /// # Safety
    ///
    /// The transport must never be used or dropped afterwards, but leaked.
    #[cfg(unix)]
    pub unsafe fn close_inherited_fds(&self) {
        unsafe { self.inner.channel.0.conn.close_inherited_fds() }
    }

    pub fn reconnect<F>(&mut self, factory: F) -> bool
    where
        F: FnOnce() -> Option<Box<SidecarTransport>>,
    {
        self.is_closed() && self.do_reconnect(factory)
    }

    fn do_reconnect<F>(&mut self, factory: F) -> bool
    where
        F: FnOnce() -> Option<Box<SidecarTransport>>,
    {
        if !self.inner.channel.0.is_closed() {
            return true;
        }

        // Avoid recursive reconnect, just to make sure we don't loop.
        thread_local! {
            static RECONNECT_IN_PROGRESS: Cell<bool> = const { Cell::new(false) };
        }
        if RECONNECT_IN_PROGRESS.with(Cell::get) {
            warn!(
                "Reconnect already in progress on this thread; not attempting a nested reconnect."
            );
            return false;
        }
        RECONNECT_IN_PROGRESS.with(|in_progress| in_progress.set(true));
        warn!(
            "The sidecar transport is closed. Reconnecting... This generally indicates a problem with the sidecar, most likely a crash. Check the logs / core dump locations and possibly report a bug."
        );
        let new = factory();
        RECONNECT_IN_PROGRESS.with(|in_progress| in_progress.set(false));
        let Some(new) = new else {
            return false;
        };
        self.replace_connection(new);
        true
    }

    /// Continues on the connection of `new`, replaying the state of the previous connection.
    fn replace_connection(&mut self, new: Box<SidecarTransport>) {
        let previous = std::mem::replace(&mut self.inner, new.inner);
        if let Some((max_bytes, max_queue)) = self.backpressure {
            _ = self.apply_backpressure(max_bytes, max_queue);
        }
        self.inner.adopt_state(previous);
    }

    /// Continues on the connection of `new` as another instance, as a fork child does: its
    /// inherited connection belongs to the parent, so nothing is sent on that one anymore.
    pub fn replace_connection_as(
        &mut self,
        new: Box<SidecarTransport>,
        instance_id: InstanceId,
        remote_config_generation: u64,
    ) {
        self.replace_connection(new);
        self.inner
            .set_instance_id(instance_id, remote_config_generation);
    }

    pub fn set_read_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(d)
    }

    pub fn set_write_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(d)
    }

    pub fn set_backpressure(&mut self, max_bytes: usize, max_queue: u64) -> io::Result<()> {
        self.backpressure = Some((max_bytes, max_queue));
        self.apply_backpressure(max_bytes, max_queue)
    }

    fn apply_backpressure(&mut self, max_bytes: usize, max_queue: u64) -> io::Result<()> {
        self.inner.max_outstanding = max_queue.max(21);
        #[cfg(unix)]
        self.inner.channel.0.set_sndbuf_size(max_bytes)?;
        #[cfg(not(unix))]
        let _ = max_bytes; // handled on pipe creation
        Ok(())
    }

    pub fn ensure_alive(&mut self) {
        if let Some(reconnect) = self.reconnect_fn.take() {
            self.do_reconnect(&reconnect);
            self.reconnect_fn = Some(reconnect);
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.channel.0.is_closed()
    }

    /// Runs `f` on the connection, reconnecting first if it is known to be broken. A broken
    /// connection may only be noticed by `f` itself: then `f` runs once more on a new
    /// connection, which carries the state of this one over. So `f` sends requests it was given
    /// by reference, instead of consuming their payload.
    fn with_retry<V>(&mut self, f: impl Fn(&mut SidecarSender) -> V) -> V {
        let result = f(self.sender());
        if !self.is_closed() {
            return result;
        }
        self.ensure_alive();
        if self.is_closed() {
            return result;
        }
        f(&mut self.inner)
    }

    /// Send garbage data (used in tests to verify error handling).
    pub fn send_garbage(&mut self) -> io::Result<()> {
        self.inner
            .channel
            .0
            .send_blocking(vec![0xDE, 0xAD, 0xBE, 0xEF], &[])
    }

    pub fn sender(&mut self) -> &mut SidecarSender {
        // Drain accumulated acks first so that EOF is detected (closing the connection)
        // before ensure_alive checks is_closed() and decides whether to reconnect.
        self.inner.channel.0.drain_acks();
        self.ensure_alive();
        &mut self.inner
    }
}

impl From<SeqpacketConn> for SidecarTransport {
    fn from(conn: SeqpacketConn) -> Self {
        SidecarTransport {
            inner: SidecarSender::new(SidecarInterfaceChannel::new(conn)),
            reconnect_fn: None,
            backpressure: None,
        }
    }
}

/// Best-effort FFE submission through the shared sender without reconnecting.
/// Uses the exclusively owned transport; platform synchronization may briefly wait.
/// The caller has already built one observation. A size rejection gets one
/// immediate retry without identity/context, not another load-shed decision.
pub fn try_submit_ffe(
    transport: &mut SidecarTransport,
    request: &mut SidecarInterfaceRequest,
) -> FfeSubmissionStatus {
    let sender = &mut transport.inner;
    if sender.channel.0.is_closed() {
        return FfeSubmissionStatus::Unavailable;
    }
    let status = sender
        .send_actions(request)
        .map_or_else(FfeSubmissionStatus::from, |_| FfeSubmissionStatus::Accepted);
    if status != FfeSubmissionStatus::PayloadTooLarge {
        return status;
    }
    let SidecarInterfaceRequest::EnqueueActions { actions, .. } = request else {
        return status;
    };
    let [SidecarAction::FfeFlagEvaluationBatch(batch)] = actions.as_mut_slice() else {
        return status;
    };
    let [event] = batch.flag_evaluations.as_mut_slice() else {
        return status;
    };
    if event.is_degraded || (event.targeting_key.is_none() && event.context.is_none()) {
        return status;
    }
    event.targeting_key = None;
    event.context = None;
    event.is_degraded = true;
    sender
        .channel
        .try_send_request(request)
        .map_or_else(FfeSubmissionStatus::from, |_| FfeSubmissionStatus::Accepted)
}

/// Converts a [`DecodeError`] to an [`io::Error`], preserving the original
/// [`io::ErrorKind`] when the decode failure was itself an I/O error.
///
/// This ensures `with_retry` properly reacts to errors and doesn't ignore some actual errors.
fn decode_error_to_io(e: DecodeError) -> io::Error {
    match e {
        DecodeError::Io(io_err) => io_err,
        other => io::Error::other(other.to_string()),
    }
}

/// Enqueues a list of actions for the application of the current request.
pub fn enqueue_actions(
    transport: &mut SidecarTransport,
    actions: Vec<SidecarAction>,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::EnqueueActions { actions };
    let _ = transport.with_retry(|s| s.send_actions(&request));
    Ok(())
}

/// Enqueues a list of actions for an explicitly given service and env.
pub fn enqueue_actions_for_service(
    transport: &mut SidecarTransport,
    service_name: String,
    env_name: String,
    actions: Vec<SidecarAction>,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::EnqueueActionsForService {
        service_name,
        env_name,
        actions,
    };
    let _ = transport.with_retry(|s| s.send_actions(&request));
    Ok(())
}

/// Registers a telemetry metric context on this connection.
///
/// Connection-bound: deduplicated per connection, never dropped, replayed after reconnect.
pub fn register_telemetry_metric(
    transport: &mut SidecarTransport,
    metric: MetricContext,
) -> io::Result<()> {
    transport.sender().register_telemetry_metric(metric);
    Ok(())
}

/// Sets the identity and configuration of the connection. Kept and replayed after reconnects.
pub fn set_connection_config(
    transport: &mut SidecarTransport,
    instance_id: InstanceId,
    #[cfg(windows)] remote_config_notify_target: Option<
        crate::service::remote_configs::RemoteConfigNotifyTarget,
    >,
    config: SessionConfig,
) -> io::Result<()> {
    transport.sender().set_connection_config(
        instance_id,
        #[cfg(windows)]
        remote_config_notify_target,
        config,
    );
    Ok(())
}

/// Sets the explicit EVP transport for one intake target on this connection.
///
/// Call after setting the connection configuration. Updates replace only this
/// target; identical configuration preserves its route and deduplication state.
/// The transport retains the latest configuration per target across reconnects.
pub fn set_session_evp_transport(
    transport: &mut SidecarTransport,
    config: crate::service::EvpTransportConfigWithIdentity,
) -> anyhow::Result<()> {
    transport.sender().set_session_evp_transport(config);
    Ok(())
}

/// Updates the process tags of the connection.
pub fn set_process_tags(
    transport: &mut SidecarTransport,
    process_tags: Vec<Tag>,
) -> io::Result<()> {
    transport.sender().set_process_tags(process_tags);
    Ok(())
}

pub fn set_user_service_defined(
    transport: &mut SidecarTransport,
    is_defined: bool,
) -> io::Result<()> {
    transport.sender().set_user_service_defined(is_defined);
    Ok(())
}

/// Sets the application of the current request, or clears it (`None`) at request end.
pub fn set_application(
    transport: &mut SidecarTransport,
    application: Option<ApplicationConfig>,
    remote_config_generation: u64,
) -> io::Result<()> {
    transport
        .sender()
        .set_application(application, remote_config_generation);
    Ok(())
}

/// Updates the dynamic instrumentation state of the current application.
pub fn set_dynamic_instrumentation_state(
    transport: &mut SidecarTransport,
    state: DynamicInstrumentationConfigState,
) -> io::Result<()> {
    transport.sender().set_dynamic_instrumentation_state(state);
    Ok(())
}

/// Sends a trace as bytes.
pub fn send_trace_v04_bytes(
    transport: &mut SidecarTransport,
    data: Vec<u8>,
    headers: SerializedTracerHeaderTags,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::SendTraceV04Bytes { data, headers };
    transport.with_retry(|s| s.send(&request));
    Ok(())
}

/// Sends a trace via shared memory.
pub fn send_trace_v04_shm(
    transport: &mut SidecarTransport,
    handle: ShmHandle,
    len: usize,
    headers: SerializedTracerHeaderTags,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::SendTraceV04Shm {
        handle,
        len,
        headers,
    };
    transport.with_retry(|s| s.send(&request));
    Ok(())
}

/// Sends a V1-encoded trace as bytes. The sidecar decodes the V1 payload, can inspect it, and
/// re-encodes it as V1 msgpack on the way to the agent's `/v1.0/traces` endpoint.
pub fn send_trace_v1_bytes(
    transport: &mut SidecarTransport,
    data: Vec<u8>,
    generic: TracerGenericTags,
    lang_interpreter: String,
    lang_vendor: String,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::SendTraceV1Bytes {
        data,
        generic,
        lang_interpreter,
        lang_vendor,
    };
    transport.with_retry(|s| s.send(&request));
    Ok(())
}

/// Sends a V1-encoded trace via shared memory. The sidecar decodes the V1 payload, can inspect
/// it, and re-encodes it as V1 msgpack on the way to the agent's `/v1.0/traces` endpoint.
pub fn send_trace_v1_shm(
    transport: &mut SidecarTransport,
    handle: ShmHandle,
    len: usize,
    generic: TracerGenericTags,
    lang_interpreter: String,
    lang_vendor: String,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::SendTraceV1Shm {
        handle,
        len,
        generic,
        lang_interpreter,
        lang_vendor,
    };
    transport.with_retry(|s| s.send(&request));
    Ok(())
}

/// Sends raw data from shared memory to the debugger endpoint.
pub fn send_debugger_data_shm(
    transport: &mut SidecarTransport,
    handle: ShmHandle,
    debugger_type: DebuggerType,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::SendDebuggerDataShm {
        handle,
        debugger_type,
    };
    transport.with_retry(|s| s.send(&request));
    Ok(())
}

/// Sends a collection of debugger payloads to the debugger endpoint via shared memory.
pub fn send_debugger_data_shm_vec(
    transport: &mut SidecarTransport,
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
        mapped.into(),
        debugger_type,
    )?)
}

/// Submits debugger diagnostics.
pub fn send_debugger_diagnostics(
    transport: &mut SidecarTransport,
    diagnostics_payload: DebuggerPayload,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::SendDebuggerDiagnostics {
        diagnostics_payload: serde_json::to_vec(&diagnostics_payload)?,
    };
    transport.with_retry(|s| s.send(&request));
    Ok(())
}

/// Acquire an exception hash rate limiter
pub fn acquire_exception_hash_rate_limiter(
    transport: &mut SidecarTransport,
    exception_hash: u64,
    granularity: Duration,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::AcquireExceptionHashRateLimiter {
        exception_hash,
        granularity,
    };
    transport.with_retry(|s| s.send(&request));
    Ok(())
}

/// Sends DogStatsD actions.
pub fn send_dogstatsd_actions(
    transport: &mut SidecarTransport,
    actions: Vec<DogStatsDActionOwned>,
) -> io::Result<()> {
    let request = SidecarInterfaceRequest::SendDogstatsdActions { actions };
    transport.with_retry(|s| s.send(&request));
    Ok(())
}

/// Sets x-datadog-test-session-token on all requests of this connection.
pub fn set_test_session_token(transport: &mut SidecarTransport, token: String) -> io::Result<()> {
    transport.sender().set_test_session_token(token);
    Ok(())
}

/// IPC fallback. Returns whether the span was sent, so callers can retry a failed bootstrap.
pub fn add_span_to_concentrator(
    transport: &mut SidecarTransport,
    env: String,
    version: String,
    span: libdd_ipc::shm_stats::OwnedShmSpanInput,
) -> io::Result<bool> {
    let request = SidecarInterfaceRequest::AddSpanToConcentrator { env, version, span };
    Ok(transport.with_retry(|s| s.send(&request)))
}

/// IPC fallback for DSM. Returns whether the checkpoint was sent, so callers can retry a failed
/// bootstrap.
pub fn add_dsm_checkpoint(
    transport: &mut SidecarTransport,
    env: String,
    version: String,
    service: String,
    primary_tag: String,
    checkpoint: libdd_ipc::shm_dsm_checkpoint::OwnedDsmCheckpoint,
) -> io::Result<bool> {
    let request = SidecarInterfaceRequest::AddDsmCheckpoint {
        env,
        version,
        service,
        primary_tag,
        checkpoint,
    };
    Ok(transport.with_retry(|s| s.send(&request)))
}

/// IPC fallback for DSM backlogs. Returns whether the offset was sent, as
/// [`add_dsm_checkpoint`] does.
pub fn add_dsm_backlog(
    transport: &mut SidecarTransport,
    env: String,
    version: String,
    service: String,
    primary_tag: String,
    backlog: libdd_ipc::shm_dsm_backlog::OwnedDsmBacklog,
) -> io::Result<bool> {
    let request = SidecarInterfaceRequest::AddDsmBacklog {
        env,
        version,
        service,
        primary_tag,
        backlog,
    };
    Ok(transport.with_retry(|s| s.send(&request)))
}

/// Starts the AppSec backend in the sidecar and waits for initialization to
/// complete before returning.
pub fn ensure_appsec_started(
    transport: &mut SidecarTransport,
    log_file_path: Vec<u8>,
    log_level: String,
) -> io::Result<bool> {
    let request = SidecarInterfaceRequest::EnsureAppsecStarted {
        log_file_path,
        log_level,
    };
    transport.with_retry(|s| {
        s.ensure_appsec_started(&request)
            .map_err(decode_error_to_io)
    })
}

/// Dumps the current state of the service.
pub fn dump(transport: &mut SidecarTransport) -> io::Result<String> {
    transport.with_retry(|s| s.dump().map_err(decode_error_to_io))
}

/// Retrieves the current statistics of the service.
pub fn stats(transport: &mut SidecarTransport) -> io::Result<String> {
    transport.with_retry(|s| s.stats().map_err(decode_error_to_io))
}

/// Forwards an AppSec message to the sidecar for dispatching to the registered helper.
///
/// Returns the response bytes from the helper and a disconnect flag.
pub fn send_appsec_message(
    transport: &mut SidecarTransport,
    data: &[u8],
    client_init: bool,
) -> io::Result<(Vec<u8>, bool)> {
    let request = SidecarInterfaceClientRequest::SendAppsecMessage { data, client_init };
    transport.with_retry(|s| s.send_appsec_message(&request).map_err(decode_error_to_io))
}

/// Forwards an AppSec message without reconnecting the sidecar on failure.
///
/// Returns the response bytes from the helper and a disconnect flag.
pub fn send_appsec_message_without_reconnect(
    transport: &mut SidecarTransport,
    data: &[u8],
    client_init: bool,
) -> io::Result<(Vec<u8>, bool)> {
    let request = SidecarInterfaceClientRequest::SendAppsecMessage { data, client_init };
    transport
        .inner
        .send_appsec_message(&request)
        .map_err(decode_error_to_io)
}

/// Flushes traces/stats and/or telemetry, as specified by options.
pub fn flush(transport: &mut SidecarTransport, options: SidecarFlushOptions) -> io::Result<()> {
    transport.with_retry(|s| s.flush(options))
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
    use super::try_submit_ffe;
    use crate::service::SidecarAction;
    use crate::service::blocking::{
        SidecarTransport, add_dsm_backlog, add_dsm_checkpoint, add_span_to_concentrator,
        enqueue_actions, send_dogstatsd_actions,
    };
    use crate::service::ffe_submission::FfeSubmissionStatus;
    use crate::service::sidecar_interface::{
        DynamicInstrumentationConfigState, SidecarInterfaceRequest,
    };
    use crate::service::{
        ApplicationConfig, EvpProducerIdentity, EvpTransportConfig, EvpTransportConfigWithIdentity,
        InstanceId,
    };
    use libdd_common::Endpoint;
    use libdd_ipc::{SeqpacketConn, SeqpacketListener};
    use libdd_telemetry::data;
    use libdd_telemetry::worker::TelemetryActions;
    use std::cell::RefCell;
    use std::time::Duration;

    use tempfile::tempdir;

    fn evp_config(target: &str, version: &str) -> EvpTransportConfigWithIdentity {
        EvpTransportConfigWithIdentity::new(
            EvpTransportConfig::prefer_local_then_direct(
                Endpoint::default(),
                Some(Endpoint {
                    url: format!("https://{target}.datadoghq.com").parse().unwrap(),
                    api_key: Some("test-api-key".into()),
                    ..Endpoint::default()
                }),
                target,
            ),
            EvpProducerIdentity::new("dd-trace-rb", version).unwrap(),
        )
        .unwrap()
    }

    fn receive_packet(peer: &SeqpacketConn) -> Vec<u8> {
        let mut buf = [0; 8192];
        let (len, _) = peer.try_recv_raw(&mut buf).unwrap();
        buf[..len].to_vec()
    }

    fn assert_evp_configs(peer: &SeqpacketConn, expected: &[EvpTransportConfigWithIdentity]) {
        match libdd_ipc::codec::decode::<SidecarInterfaceRequest>(&receive_packet(peer)).unwrap() {
            SidecarInterfaceRequest::AddSessionEvpTransports { configs } => {
                assert_eq!(configs, expected)
            }
            request => panic!("expected EVP configuration, got {request:?}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn coalesced_evp_updates_follow_connection_setup_before_data() {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        let sender = &mut transport.inner;
        sender.max_outstanding = 0;
        sender.set_connection_config(
            InstanceId::new("test-session", "runtime"),
            super::SessionConfig::for_test(),
        );
        let event_platform = evp_config("event-platform-intake", "2.0.0");
        let errors = evp_config("errors-intake", "1.0.0");
        sender.set_session_evp_transport(evp_config("event-platform-intake", "1.0.0"));
        sender.set_session_evp_transport(errors.clone());
        sender.set_session_evp_transport(event_platform.clone());

        sender.max_outstanding = 100;
        sender.set_process_tags(vec![]);
        sender.drain_and_send_raw_blocking(b"DATA").unwrap();
        assert!(matches!(
            libdd_ipc::codec::decode::<SidecarInterfaceRequest>(&receive_packet(&peer)).unwrap(),
            SidecarInterfaceRequest::SetConnectionConfig { instance_id, .. }
                if instance_id == InstanceId::new("test-session", "runtime")
        ));
        assert_evp_configs(&peer, &[event_platform, errors]);
        assert!(matches!(
            libdd_ipc::codec::decode::<SidecarInterfaceRequest>(&receive_packet(&peer)).unwrap(),
            SidecarInterfaceRequest::SetProcessTags { .. }
        ));
        assert_eq!(receive_packet(&peer), b"DATA");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn failed_blocking_drain_retains_evp_configuration() {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        let sender = &mut transport.inner;
        sender.max_outstanding = 0;
        let config = evp_config("event-platform-intake", "1.0.0");
        let other = evp_config("errors-intake", "1.0.0");
        sender.set_session_evp_transport(config.clone());
        sender.set_session_evp_transport(other.clone());
        drop(peer);
        assert!(sender.drain_and_send_raw_blocking(b"DATA").is_err());

        // Keep the sender to isolate failed-drain retention from reconnect replay.
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        sender.channel = super::SidecarInterfaceChannel::new(conn);
        sender.drain_and_send_raw_blocking(b"DATA").unwrap();
        assert_evp_configs(&peer, &[config, other]);
        assert_eq!(receive_packet(&peer), b"DATA");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn reconnect_replays_latest_evp_configuration_before_data() {
        for pending in [false, true] {
            let (conn, peer) = SeqpacketConn::socketpair().unwrap();
            let mut transport = SidecarTransport::from(conn);
            let config = evp_config("event-platform-intake", "2.0.0");
            let other = evp_config("errors-intake", "1.0.0");
            let sender = &mut transport.inner;
            sender.max_outstanding = if pending { 0 } else { 100 };
            sender.set_session_evp_transport(evp_config("event-platform-intake", "1.0.0"));
            sender.set_session_evp_transport(other.clone());
            sender.set_session_evp_transport(config.clone());
            if !pending {
                assert_eq!(received(&peer).len(), 3);
            }
            drop(peer);
            assert!(
                transport
                    .inner
                    .drain_and_send_raw_blocking(b"DATA")
                    .is_err()
            );
            // A failed factory must not discard state needed by the next attempt.
            assert!(!transport.do_reconnect(|| None));
            let (conn, peer) = SeqpacketConn::socketpair().unwrap();
            assert!(transport.do_reconnect(|| Some(Box::new(SidecarTransport::from(conn)))));
            transport
                .inner
                .drain_and_send_raw_blocking(b"DATA")
                .unwrap();
            assert_evp_configs(&peer, &[config, other]);
            assert_eq!(receive_packet(&peer), b"DATA");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn reconnect_keeps_new_factory_evp_configuration() {
        let (conn, old_peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        transport.inner.max_outstanding = 0;
        transport
            .inner
            .set_session_evp_transport(evp_config("event-platform-intake", "old"));
        transport
            .inner
            .set_session_evp_transport(evp_config("errors-intake", "old"));
        drop(old_peer);
        assert!(
            transport
                .inner
                .drain_and_send_raw_blocking(b"DATA")
                .is_err()
        );
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let fresh = evp_config("event-platform-intake", "new");
        assert!(transport.do_reconnect(|| {
            let mut new = SidecarTransport::from(conn);
            new.inner.max_outstanding = 0;
            new.inner.set_session_evp_transport(fresh.clone());
            Some(Box::new(new))
        }));
        transport
            .inner
            .drain_and_send_raw_blocking(b"DATA")
            .unwrap();
        assert_evp_configs(&peer, &[fresh, evp_config("errors-intake", "old")]);
        assert_eq!(receive_packet(&peer), b"DATA");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_new_transport_does_not_inherit_ended_connection_evp_settings() {
        let (conn, _peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        transport.inner.max_outstanding = 0;
        transport
            .inner
            .set_session_evp_transport(evp_config("event-platform-intake", "old"));
        drop(transport);

        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        transport
            .inner
            .drain_and_send_raw_blocking(b"DATA")
            .unwrap();
        assert_eq!(receive_packet(&peer), b"DATA");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn evp_configuration_is_replayed_after_reconfiguration_and_fork() {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        let sender = &mut transport.inner;
        sender.set_connection_config(
            InstanceId::new("session", "parent"),
            super::SessionConfig::for_test(),
        );
        let config = evp_config("event-platform-intake", "1.0.0");
        sender.set_session_evp_transport(config.clone());
        assert_eq!(received(&peer).len(), 2);
        sender.set_session_evp_transport(config.clone());
        assert!(received(&peer).is_empty());

        for fork in [false, true] {
            let instance = InstanceId::new("session", if fork { "child" } else { "parent" });
            if fork {
                sender.set_instance_id(instance.clone(), 0);
            } else {
                sender.set_connection_config(instance.clone(), super::SessionConfig::for_test());
            }
            sender.drain_and_send_raw_blocking(b"DATA").unwrap();
            assert!(matches!(
                libdd_ipc::codec::decode::<SidecarInterfaceRequest>(&receive_packet(&peer)).unwrap(),
                SidecarInterfaceRequest::SetConnectionConfig { instance_id, .. }
                    if instance_id == instance
            ));
            assert_evp_configs(&peer, std::slice::from_ref(&config));
            assert_eq!(receive_packet(&peer), b"DATA");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn stats_fallback_reports_whether_the_span_was_sent() {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        transport
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let send = |transport: &mut SidecarTransport| {
            add_span_to_concentrator(
                transport,
                "env".into(),
                "v1".into(),
                libdd_ipc::shm_stats::OwnedShmSpanInput {
                    fixed: Default::default(),
                    peer_tags: vec![("db.hostname".into(), "db".into())],
                    duration_ns: 42,
                    is_error: false,
                    is_top_level: true,
                },
            )
            .unwrap()
        };
        assert!(send(&mut transport));
        let mut buf = [0; 1024];
        let (len, _) = peer.try_recv_raw(&mut buf).unwrap();
        match libdd_ipc::codec::decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap() {
            SidecarInterfaceRequest::AddSpanToConcentrator { env, version, span } => {
                assert_eq!(env, "env");
                assert_eq!(version, "v1");
                assert_eq!(span.duration_ns, 42);
                assert_eq!(span.peer_tags, [("db.hostname".into(), "db".into())]);
            }
            _ => panic!("expected a span"),
        }

        drop(peer);
        assert!(!send(&mut transport));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn dsm_fallback_reports_whether_the_point_was_sent() {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        transport
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let send = |transport: &mut SidecarTransport| {
            add_dsm_checkpoint(
                transport,
                "env".into(),
                "v1".into(),
                "svc".into(),
                "region:eu".into(),
                libdd_ipc::shm_dsm_checkpoint::OwnedDsmCheckpoint {
                    hash: 2,
                    parent_hash: 3,
                    edge_tags: vec!["topic:orders".into()],
                    pathway_latency_ns: 4,
                    edge_latency_ns: 5,
                    payload_size_bytes: 6,
                },
            )
            .unwrap()
        };
        assert!(send(&mut transport));
        let mut buf = [0; 1024];
        let (len, _) = peer.try_recv_raw(&mut buf).unwrap();
        match libdd_ipc::codec::decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap() {
            SidecarInterfaceRequest::AddDsmCheckpoint {
                env,
                version,
                service,
                primary_tag,
                checkpoint,
            } => {
                assert_eq!(env, "env");
                assert_eq!(version, "v1");
                assert_eq!(service, "svc");
                assert_eq!(primary_tag, "region:eu");
                assert_eq!(checkpoint.hash, 2);
                assert_eq!(checkpoint.edge_tags, ["topic:orders"]);
            }
            _ => panic!("expected a DSM checkpoint"),
        }

        drop(peer);
        assert!(!send(&mut transport));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn dsm_backlog_fallback_reports_whether_the_offset_was_sent() {
        use libdd_ipc::shm_dsm_backlog::BacklogTag;
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        transport
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let send = |transport: &mut SidecarTransport| {
            add_dsm_backlog(
                transport,
                "env".into(),
                "v1".into(),
                "svc".into(),
                "".into(),
                libdd_ipc::shm_dsm_backlog::OwnedDsmBacklog {
                    tags: vec![
                        (BacklogTag::Topic, "orders".into()),
                        (BacklogTag::Type, "kafka_produce".into()),
                    ],
                    offset: 42,
                },
            )
            .unwrap()
        };
        assert!(send(&mut transport));
        let mut buf = [0; 1024];
        let (len, _) = peer.try_recv_raw(&mut buf).unwrap();
        match libdd_ipc::codec::decode::<SidecarInterfaceRequest>(&buf[..len]).unwrap() {
            SidecarInterfaceRequest::AddDsmBacklog {
                env,
                service,
                backlog,
                ..
            } => {
                assert_eq!(env, "env");
                assert_eq!(service, "svc");
                assert_eq!(
                    backlog.tags,
                    [
                        (BacklogTag::Topic, "orders".to_owned()),
                        (BacklogTag::Type, "kafka_produce".to_owned()),
                    ]
                );
                assert_eq!(backlog.offset, 42);
            }
            _ => panic!("expected a DSM backlog"),
        }

        drop(peer);
        assert!(!send(&mut transport));
    }

    /// Decodes all requests sent to `peer` so far.
    fn received(peer: &SeqpacketConn) -> Vec<SidecarInterfaceRequest> {
        let mut buf = vec![0; libdd_ipc::max_message_size()];
        let mut requests = vec![];
        while let Ok((len, fds)) = peer.try_recv_raw(&mut buf) {
            let request: SidecarInterfaceRequest = libdd_ipc::codec::decode(&buf[..len]).unwrap();
            requests.push(request.with_handles(fds).unwrap());
        }
        requests
    }

    fn application() -> ApplicationConfig {
        ApplicationConfig {
            service_name: "svc".to_owned(),
            env_name: "env".to_owned(),
            app_version: "1.0".to_owned(),
            global_tags: vec![],
            dynamic_instrumentation_state: DynamicInstrumentationConfigState::NotSet,
        }
    }

    /// A transport whose connection broke while nothing awaits an acknowledgement, so only the
    /// next send notices it. It reconnects to the returned peer, if `reconnect`.
    fn broken_transport(reconnect: bool) -> (SidecarTransport, SeqpacketConn) {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        transport.inner.set_application(Some(application()), 3);
        assert_eq!(received(&peer).len(), 1);
        // Acknowledged like the sidecar does, so draining the acks finds nothing to read.
        peer.try_send_raw(vec![0], &[]).unwrap();
        transport.inner.channel.0.drain_acks();
        assert_eq!(transport.inner.channel.0.outstanding(), 0);
        drop(peer);

        let (new_conn, new_peer) = SeqpacketConn::socketpair().unwrap();
        if reconnect {
            let new_conn = RefCell::new(Some(new_conn));
            transport.reconnect_fn = Some(Box::new(move || {
                new_conn
                    .borrow_mut()
                    .take()
                    .map(|conn| Box::new(SidecarTransport::from(conn)))
            }));
        }
        (transport, new_peer)
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn ffe_closed_transport_does_not_reconnect() {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let mut transport = SidecarTransport::from(conn);
        transport.reconnect_fn = Some(Box::new(|| panic!("must not reconnect")));
        drop(peer);
        assert!(transport.send_garbage().is_err());
        assert!(transport.is_closed());
        let mut request = SidecarInterfaceRequest::EnqueueActions { actions: vec![] };
        assert_eq!(
            try_submit_ffe(&mut transport, &mut request),
            FfeSubmissionStatus::Unavailable
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn ffe_does_not_reconnect_when_the_send_finds_a_broken_connection() {
        let (mut transport, new_peer) = broken_transport(true);
        let mut request = SidecarInterfaceRequest::EnqueueActions { actions: vec![] };
        assert_eq!(
            try_submit_ffe(&mut transport, &mut request),
            FfeSubmissionStatus::Unavailable
        );
        assert!(transport.is_closed());
        assert!(received(&new_peer).is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_request_finding_the_connection_broken_is_sent_on_a_new_one() {
        let (mut transport, new_peer) = broken_transport(true);
        send_dogstatsd_actions(&mut transport, vec![]).unwrap();

        match received(&new_peer).as_slice() {
            [
                SidecarInterfaceRequest::SetApplication {
                    application: replayed,
                    remote_config_generation: 0,
                },
                SidecarInterfaceRequest::SendDogstatsdActions { actions },
            ] => {
                assert_eq!(replayed.as_ref(), Some(&application()));
                assert!(actions.is_empty());
            }
            other => panic!("unexpected requests on the new connection: {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn without_reconnect_a_request_finding_the_connection_broken_is_dropped() {
        let (mut transport, new_peer) = broken_transport(false);
        send_dogstatsd_actions(&mut transport, vec![]).unwrap();

        assert!(transport.is_closed());
        assert!(received(&new_peer).is_empty());
    }

    /// A transport whose socket grants a small send buffer, like one capped by
    /// `net.core.wmem_max`, without changing the process-wide configured packet limit.
    fn small_buffer_transport() -> (SidecarTransport, SeqpacketConn) {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        let bytes: libc::c_int = 16 * 1024;
        // SAFETY: valid socket and pointer/length for a live integer.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    conn.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&bytes as *const libc::c_int).cast(),
                    std::mem::size_of_val(&bytes).try_into().unwrap(),
                )
            },
            0
        );
        let transport = SidecarTransport::from(conn);
        assert!(transport.inner.channel.0.max_send_size() < libdd_ipc::max_message_size());
        (transport, peer)
    }

    fn endpoint(path: String) -> SidecarAction {
        SidecarAction::Telemetry(TelemetryActions::AddEndpoint(data::Endpoint {
            method: Some(data::Method::Get),
            path: Some(path),
            operation_name: "laravel.request".to_owned(),
            resource_name: "GET /route".to_owned(),
            response_code: None,
            request_body_type: None,
            response_body_type: None,
        }))
    }

    fn endpoint_path(action: &SidecarAction) -> &str {
        match action {
            SidecarAction::Telemetry(TelemetryActions::AddEndpoint(data::Endpoint {
                path: Some(path),
                ..
            })) => path,
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn actions_exceeding_the_granted_message_size_are_sent_in_shared_memory() {
        let (mut transport, peer) = small_buffer_transport();
        let paths: Vec<String> = (0..2000).map(|i| format!("/route/{i}")).collect();
        enqueue_actions(
            &mut transport,
            paths.iter().cloned().map(endpoint).collect(),
        )
        .unwrap();

        match received(&peer).as_slice() {
            [SidecarInterfaceRequest::EnqueueActions { actions }] => {
                assert_eq!(actions.iter().map(endpoint_path).collect::<Vec<_>>(), paths);
            }
            other => panic!("expected a single request, got {}", other.len()),
        }
        assert!(!transport.is_closed());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[serial_test::serial(log_counter)]
    fn actions_exceeding_the_configured_message_size_are_dropped() {
        let (mut transport, peer) = small_buffer_transport();
        let huge = "x".repeat(libdd_ipc::max_message_size());
        enqueue_actions(
            &mut transport,
            vec![endpoint("/a".to_owned()), endpoint(huge)],
        )
        .unwrap();
        enqueue_actions(&mut transport, vec![endpoint("/b".to_owned())]).unwrap();

        match received(&peer).as_slice() {
            [SidecarInterfaceRequest::EnqueueActions { actions }] => {
                assert_eq!(
                    actions.iter().map(endpoint_path).collect::<Vec<_>>(),
                    ["/b"]
                );
            }
            other => panic!("unexpected requests: {other:?}"),
        }
        assert!(!transport.is_closed());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[serial_test::serial(log_counter)]
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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_peer_pid_returns_current_process() {
        let tmpdir = tempdir().unwrap();
        let socket_path = tmpdir.path().join("test_peer_pid.sock");

        let listener = SeqpacketListener::bind(&socket_path).expect("Cannot bind");
        let conn = SeqpacketConn::connect(&socket_path).unwrap();
        let _server_conn = listener.try_accept().expect("try_accept");

        let transport = SidecarTransport::from(conn);
        let pid = transport.peer_pid().expect("peer_pid should succeed");
        assert_eq!(
            pid,
            std::process::id(),
            "peer_pid should be our own PID for a loopback connection"
        );
    }
}
