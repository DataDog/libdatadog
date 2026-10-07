// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Higher-level sender with priority outbox and telemetry load-shedding.
//!
//! Wraps [`SidecarInterfaceChannel`] with:
//! - A **priority outbox** for state-change messages: coalesced and drained before fire-and-forget
//!   sends.
//! - **Telemetry load-shedding**: when `outstanding > max_outstanding / 2`, 90% of `EnqueueActions`
//!   calls are dropped (telemetry is low priority).
//!
//! `SidecarSender` takes `&mut self`; the caller is responsible for exclusive access.

use crate::service::ffe_submission::FfeSubmissionStatus;
use crate::service::{
    InstanceId, QueueId, SerializedTracerHeaderTags, SessionConfig, SidecarAction,
    sidecar_interface::{
        DynamicInstrumentationConfigState, SidecarFlushOptions, SidecarInterfaceChannel,
        SidecarInterfaceClientRequest, SidecarInterfaceRequest,
    },
};
use libdd_common::tag::Tag;
use libdd_dogstatsd_client::DogStatsDActionOwned;
use libdd_ipc::platform::ShmHandle;
use libdd_live_debugger::sender::DebuggerType;
use libdd_telemetry::metrics::MetricContext;
use libdd_trace_utils::trace_utils::TracerGenericTags;
use std::collections::HashMap;
use std::{io, time::Duration};
use tracing::trace;

#[cfg(all(test, unix))]
mod ffe_submission_tests;

/// Priority outbox for state-change (coalesced) messages.
///
/// Each slot holds the most recent pending message of its kind.
/// Slots are drained in field order (priority order) before fire-and-forget sends.
#[derive(Default)]
struct SidecarOutbox {
    set_session_config: Option<SidecarInterfaceRequest>,
    set_session_process_tags: Option<SidecarInterfaceRequest>,
    set_session_default_service_name: Option<SidecarInterfaceRequest>,
    set_session_user_service_defined: Option<SidecarInterfaceRequest>,
    set_universal_service_tags: Option<SidecarInterfaceRequest>,
    set_request_config: Option<SidecarInterfaceRequest>,
    clear_queue_id: Option<SidecarInterfaceRequest>,
    shutdown_runtime: Option<SidecarInterfaceRequest>,
    shutdown_session: Option<SidecarInterfaceRequest>,
}

impl SidecarOutbox {
    fn slots_mut(&mut self) -> [&mut Option<SidecarInterfaceRequest>; 9] {
        [
            &mut self.set_session_config,
            &mut self.set_session_process_tags,
            &mut self.set_session_default_service_name,
            &mut self.set_session_user_service_defined,
            &mut self.set_universal_service_tags,
            &mut self.set_request_config,
            &mut self.clear_queue_id,
            &mut self.shutdown_runtime,
            &mut self.shutdown_session,
        ]
    }
}

fn cancel_if_instance(slot: &mut Option<SidecarInterfaceRequest>, instance_id: &InstanceId) {
    let should_cancel = match slot {
        Some(SidecarInterfaceRequest::SetUniversalServiceTags {
            instance_id: id, ..
        }) => id == instance_id,
        Some(SidecarInterfaceRequest::SetRequestConfig {
            instance_id: id, ..
        }) => id == instance_id,
        _ => false,
    };
    if should_cancel {
        *slot = None;
    }
}

fn cancel_if_queue(
    slot: &mut Option<SidecarInterfaceRequest>,
    instance_id: &InstanceId,
    queue_id: &QueueId,
) {
    let should_cancel = match slot {
        Some(SidecarInterfaceRequest::SetUniversalServiceTags {
            instance_id: id,
            queue_id: q,
            ..
        }) => id == instance_id && q == queue_id,
        Some(SidecarInterfaceRequest::SetRequestConfig {
            instance_id: id,
            queue_id: q,
            ..
        }) => id == instance_id && q == queue_id,
        _ => false,
    };
    if should_cancel {
        *slot = None;
    }
}

fn coalesce(outbox: &mut SidecarOutbox, incoming: SidecarInterfaceRequest) {
    if let SidecarInterfaceRequest::ShutdownRuntime { ref instance_id } = incoming {
        cancel_if_instance(&mut outbox.set_request_config, instance_id);
        cancel_if_instance(&mut outbox.set_universal_service_tags, instance_id);
    }
    if matches!(incoming, SidecarInterfaceRequest::ShutdownSession {}) {
        outbox.set_session_config = None;
    }
    if let SidecarInterfaceRequest::ClearQueueId {
        ref instance_id,
        ref queue_id,
    } = incoming
    {
        cancel_if_queue(&mut outbox.set_request_config, instance_id, queue_id);
        cancel_if_queue(
            &mut outbox.set_universal_service_tags,
            instance_id,
            queue_id,
        );
    }

    match incoming {
        SidecarInterfaceRequest::SetSessionConfig { .. } => {
            outbox.set_session_config = Some(incoming);
        }
        SidecarInterfaceRequest::SetSessionProcessTags { .. } => {
            outbox.set_session_process_tags = Some(incoming);
        }
        SidecarInterfaceRequest::SetSessionDefaultServiceName { .. } => {
            outbox.set_session_default_service_name = Some(incoming);
        }
        SidecarInterfaceRequest::SetSessionUserServiceDefined { .. } => {
            outbox.set_session_user_service_defined = Some(incoming);
        }
        SidecarInterfaceRequest::SetUniversalServiceTags { .. } => {
            outbox.set_universal_service_tags = Some(incoming);
        }
        SidecarInterfaceRequest::SetRequestConfig { .. } => {
            outbox.set_request_config = Some(incoming);
        }
        SidecarInterfaceRequest::ClearQueueId { .. } => {
            outbox.clear_queue_id = Some(incoming);
        }
        SidecarInterfaceRequest::ShutdownRuntime { .. } => {
            outbox.shutdown_runtime = Some(incoming);
        }
        SidecarInterfaceRequest::ShutdownSession { .. } => {
            outbox.shutdown_session = Some(incoming);
        }
        _ => {
            unreachable!("Not in outbox");
        }
    }
}

/// Higher-level IPC sender with outbox coalescing and telemetry load-shedding.
pub struct SidecarSender {
    pub channel: SidecarInterfaceChannel,
    outbox: SidecarOutbox,
    /// Maximum allowed outstanding (sent-but-not-acked) messages before outbox drain is skipped
    /// and fire-and-forget sends are blocked.
    pub max_outstanding: u64,
    /// Cycles 0–9; used to implement 90% telemetry drop under backpressure.
    enqueue_actions_counter: u8,
    /// All metric registrations ever sent on this transport (keyed by name).
    /// Persisted across reconnects; replayed on new connections before any metric points.
    pub metric_registrations: HashMap<String, MetricContext>,
}

impl SidecarSender {
    /// Advisory FFE admission check. Drains available ACKs but never waits,
    /// reconnects, advances shedding, or reserves a slot.
    pub fn check_ffe_submission(&mut self) -> FfeSubmissionStatus {
        if self.channel.0.is_closed() {
            return FfeSubmissionStatus::Unavailable;
        }
        if self.channel.0.outstanding() >= self.max_outstanding / 2 {
            self.channel.0.drain_acks();
        }
        if self.channel.0.is_closed() {
            FfeSubmissionStatus::Unavailable
        } else if self.channel.0.outstanding() >= self.max_outstanding {
            FfeSubmissionStatus::QueueFull
        } else {
            FfeSubmissionStatus::Ready
        }
    }

    /// Submit one FFE observation without waiting or reconnecting. `build` runs
    /// only after admission; it must bound and normalize borrowed input before
    /// ownership. No request is retained when submission is rejected.
    /// A size rejection gets one retry without targeting key or event context.
    /// Both full and degraded observations return `Accepted` on success.
    /// Retains the existing transport policy: macOS `ENOBUFS` can close the
    /// connection, unlike `WouldBlock`. Further observations are skipped until
    /// an ordinary transport/lifecycle operation reconnects outside evaluation.
    pub fn try_submit_ffe<F>(&mut self, build: F) -> FfeSubmissionStatus
    where
        F: FnOnce() -> Result<SidecarInterfaceRequest, FfeSubmissionStatus>,
    {
        let status = self.check_ffe_submission();
        if status != FfeSubmissionStatus::Ready {
            return status;
        }
        if !self.try_drain_outbox() {
            return if self.channel.0.is_closed() {
                FfeSubmissionStatus::Unavailable
            } else {
                FfeSubmissionStatus::PriorityPending
            };
        }
        let status = self.check_ffe_submission();
        if status != FfeSubmissionStatus::Ready {
            return status;
        }
        if self.channel.0.outstanding() > self.max_outstanding / 2 {
            self.enqueue_actions_counter = self.enqueue_actions_counter.wrapping_add(1) % 10;
            if self.enqueue_actions_counter != 0 {
                return FfeSubmissionStatus::LoadShed;
            }
        }
        let mut request = match build() {
            Ok(request) => request,
            Err(status) => return status,
        };
        if !matches!(&request, SidecarInterfaceRequest::EnqueueActions { actions, .. }
            if matches!(actions.as_slice(), [SidecarAction::FfeFlagEvaluationBatch(batch)]
                if batch.flag_evaluations.len() == 1))
        {
            return FfeSubmissionStatus::InvalidInput;
        }
        let status = self.try_send_ffe_request(&request);
        if status != FfeSubmissionStatus::PayloadTooLarge {
            return status;
        }
        if let SidecarInterfaceRequest::EnqueueActions { actions, .. } = &mut request
            && let [SidecarAction::FfeFlagEvaluationBatch(batch)] = actions.as_mut_slice()
            && let [event] = batch.flag_evaluations.as_mut_slice()
            && !event.is_degraded
            && (event.targeting_key.is_some() || event.context.is_some())
        {
            // Match the final EVP size fallback. Move straight to the reduced
            // send: no second admission, load-shed decision, or retained copy.
            event.targeting_key = None;
            event.context = None;
            event.is_degraded = true;
            return self.try_send_ffe_request(&request);
        }
        status
    }

    fn try_send_ffe_request(&mut self, request: &SidecarInterfaceRequest) -> FfeSubmissionStatus {
        let size = match bincode::serialized_size(&request) {
            Ok(size) => size,
            Err(_) => return FfeSubmissionStatus::EncodingError,
        };
        let Ok(size) = usize::try_from(size) else {
            return FfeSubmissionStatus::PayloadTooLarge;
        };
        if size > libdd_ipc::max_message_size() {
            return FfeSubmissionStatus::PayloadTooLarge;
        }
        let Some(capacity) = size.checked_add(libdd_ipc::platform::HANDLE_SUFFIX_SIZE) else {
            return FfeSubmissionStatus::PayloadTooLarge;
        };
        let mut data = Vec::with_capacity(capacity);
        if bincode::serialize_into(&mut data, &request).is_err() {
            return FfeSubmissionStatus::EncodingError;
        }
        match self.channel.0.try_send_with_rejection(data, &[]) {
            Ok(()) => FfeSubmissionStatus::Accepted,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => FfeSubmissionStatus::WouldBlock,
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::EMSGSIZE) => {
                FfeSubmissionStatus::PayloadTooLarge
            }
            Err(_) => FfeSubmissionStatus::Unavailable,
        }
    }

    pub fn new(channel: SidecarInterfaceChannel) -> Self {
        Self {
            channel,
            outbox: SidecarOutbox::default(),
            max_outstanding: 100,
            enqueue_actions_counter: 0,
            metric_registrations: HashMap::new(),
        }
    }

    /// Non-blocking drain of the outbox.  Returns `true` if all messages were sent.
    fn try_drain_outbox(&mut self) -> bool {
        // Drain pending acks when approaching the throttle threshold so the socket
        // receive buffer doesn't fill up and block the sidecar from sending more acks.
        if self.channel.0.outstanding() >= self.max_outstanding / 2 {
            self.channel.0.drain_acks();
        }
        for slot in self.outbox.slots_mut() {
            if let Some(msg) = slot {
                if self.channel.0.outstanding() >= self.max_outstanding {
                    return false;
                }
                if !self.channel.try_send_request(msg) {
                    return false;
                }
                *slot = None;
            }
        }
        true
    }

    /// Blocking drain of the outbox (used before blocking calls).
    fn drain_outbox_blocking(&mut self) {
        for slot in self.outbox.slots_mut() {
            if let Some(msg) = slot.take() {
                self.channel.send_request_blocking(&msg).ok();
            }
        }
    }

    /// Drain outbox blocking, then send pre-serialized bytes blocking (no fds).
    ///
    /// Returns `Err(BrokenPipe)` (or another I/O error) when the connection is broken,
    /// allowing callers to detect failure and trigger reconnect via `SidecarTransport::with_retry`.
    /// Only suitable for requests that transfer no file descriptors (e.g. `enqueue_actions`).
    pub fn drain_and_send_raw_blocking(&mut self, data: &[u8]) -> io::Result<()> {
        self.drain_outbox_blocking();
        self.channel.0.send_blocking(data.to_vec(), &[])
    }

    pub fn set_session_config(
        &mut self,
        session_id: String,
        #[cfg(windows)] remote_config_notify_target: Option<
            crate::service::remote_configs::RemoteConfigNotifyTarget,
        >,
        config: SessionConfig,
        is_fork: bool,
    ) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::SetSessionConfig {
                session_id,
                #[cfg(windows)]
                remote_config_notify_target,
                config,
                is_fork,
            },
        );
        self.try_drain_outbox();
    }

    pub fn set_session_process_tags(&mut self, process_tags: Vec<Tag>) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::SetSessionProcessTags { process_tags },
        );
        self.try_drain_outbox();
    }

    pub fn set_session_default_service_name(&mut self, name: Option<String>) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::SetSessionDefaultServiceName { name },
        );
        self.try_drain_outbox();
    }

    pub fn set_session_user_service_defined(&mut self, is_defined: bool) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::SetSessionUserServiceDefined { is_defined },
        );
        self.try_drain_outbox();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_universal_service_tags(
        &mut self,
        instance_id: InstanceId,
        queue_id: QueueId,
        service_name: String,
        env_name: String,
        app_version: String,
        global_tags: Vec<Tag>,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
        remote_config_generation: u64,
    ) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::SetUniversalServiceTags {
                instance_id,
                queue_id,
                service_name,
                env_name,
                app_version,
                global_tags,
                dynamic_instrumentation_state,
                remote_config_generation,
            },
        );
        self.try_drain_outbox();
    }

    pub fn set_request_config(
        &mut self,
        instance_id: InstanceId,
        queue_id: QueueId,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
    ) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::SetRequestConfig {
                instance_id,
                queue_id,
                dynamic_instrumentation_state,
            },
        );
        self.try_drain_outbox();
    }

    /// Registers a telemetry metric context on this connection.
    ///
    /// Deduplicates by name: if already registered on this connection, the call is a no-op.
    /// Sends the registration blocking (bypasses load-shedding).  The registration is stored
    /// and replayed automatically after any reconnect, before the next `enqueue_actions` call.
    pub fn register_telemetry_metric(&mut self, metric: MetricContext) {
        if self.metric_registrations.contains_key(&metric.name) {
            return;
        }
        self.metric_registrations
            .insert(metric.name.clone(), metric.clone());
        let req = SidecarInterfaceRequest::RegisterTelemetryMetric { metric };
        self.channel.send_request_blocking(&req).ok();
    }

    pub fn clear_queue_id(&mut self, instance_id: InstanceId, queue_id: QueueId) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::ClearQueueId {
                instance_id,
                queue_id,
            },
        );
        self.try_drain_outbox();
    }

    pub fn shutdown_runtime(&mut self, instance_id: InstanceId) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::ShutdownRuntime { instance_id },
        );
        self.try_drain_outbox();
    }

    pub fn shutdown_session(&mut self) {
        coalesce(
            &mut self.outbox,
            SidecarInterfaceRequest::ShutdownSession {},
        );
        self.try_drain_outbox();
    }

    /// Enqueue telemetry actions.
    ///
    /// When `outstanding > max_outstanding / 2`, 90% of calls are dropped to shed load.
    pub fn enqueue_actions(
        &mut self,
        instance_id: InstanceId,
        queue_id: QueueId,
        actions: Vec<SidecarAction>,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        // Load-shed: drop 90% when buffer is more than half full.
        let outstanding = self.channel.0.outstanding();
        if outstanding > self.max_outstanding / 2 {
            self.enqueue_actions_counter = self.enqueue_actions_counter.wrapping_add(1) % 10;
            if self.enqueue_actions_counter != 0 {
                trace!(
                    "enqueue_actions dropped: load-shedding (buffer more than half full) - outstanding: {}/{}",
                    outstanding, self.max_outstanding,
                );
                return;
            }
            // The 10% that passes through falls to the try_send below.
        }
        self.channel
            .try_send_enqueue_actions(instance_id, queue_id, actions);
    }

    pub fn send_trace_v04_shm(
        &mut self,
        instance_id: InstanceId,
        handle: ShmHandle,
        len: usize,
        headers: SerializedTracerHeaderTags,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel
            .try_send_send_trace_v04_shm(instance_id, handle, len, headers);
    }

    pub fn send_trace_v04_bytes(
        &mut self,
        instance_id: InstanceId,
        data: Vec<u8>,
        headers: SerializedTracerHeaderTags,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel
            .try_send_send_trace_v04_bytes(instance_id, data, headers);
    }

    pub fn send_trace_v1_shm(
        &mut self,
        instance_id: InstanceId,
        handle: ShmHandle,
        len: usize,
        generic: TracerGenericTags,
        lang_interpreter: String,
        lang_vendor: String,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel.try_send_send_trace_v1_shm(
            instance_id,
            handle,
            len,
            generic,
            lang_interpreter,
            lang_vendor,
        );
    }

    pub fn send_trace_v1_bytes(
        &mut self,
        instance_id: InstanceId,
        data: Vec<u8>,
        generic: TracerGenericTags,
        lang_interpreter: String,
        lang_vendor: String,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel.try_send_send_trace_v1_bytes(
            instance_id,
            data,
            generic,
            lang_interpreter,
            lang_vendor,
        );
    }

    pub fn send_debugger_data_shm(
        &mut self,
        instance_id: InstanceId,
        queue_id: QueueId,
        handle: ShmHandle,
        debugger_type: DebuggerType,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel
            .try_send_send_debugger_data_shm(instance_id, queue_id, handle, debugger_type);
    }

    pub fn send_debugger_diagnostics(
        &mut self,
        instance_id: InstanceId,
        queue_id: QueueId,
        diagnostics_payload: Vec<u8>,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel
            .try_send_send_debugger_diagnostics(instance_id, queue_id, diagnostics_payload);
    }

    pub fn acquire_exception_hash_rate_limiter(
        &mut self,
        exception_hash: u64,
        granularity: Duration,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel
            .try_send_acquire_exception_hash_rate_limiter(exception_hash, granularity);
    }

    pub fn send_dogstatsd_actions(
        &mut self,
        instance_id: InstanceId,
        actions: Vec<DogStatsDActionOwned>,
    ) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel
            .try_send_send_dogstatsd_actions(instance_id, actions);
    }

    pub fn set_test_session_token(&mut self, token: String) {
        if !self.try_drain_outbox() {
            return;
        }
        self.channel.try_send_set_test_session_token(token);
    }

    pub fn add_span_to_concentrator(
        &mut self,
        env: String,
        version: String,
        span: libdd_ipc::shm_stats::OwnedShmSpanInput,
    ) -> bool {
        if !self.try_drain_outbox() {
            return false;
        }
        self.channel
            .try_send_add_span_to_concentrator(env, version, span)
    }

    pub fn set_read_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.channel.0.set_read_timeout(d)
    }

    pub fn set_write_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.channel.0.set_write_timeout(d)
    }

    pub fn ensure_appsec_started(
        &mut self,
        log_file_path: Vec<u8>,
        log_level: String,
    ) -> Result<bool, libdd_ipc::codec::DecodeError> {
        self.channel
            .call_ensure_appsec_started(log_file_path, log_level)
    }

    pub fn send_appsec_message(
        &mut self,
        request: &SidecarInterfaceClientRequest<'_>,
    ) -> Result<(Vec<u8>, bool), libdd_ipc::codec::DecodeError> {
        self.drain_outbox_blocking();
        self.channel.call_client_request_blocking(request)
    }

    pub fn flush(&mut self, options: SidecarFlushOptions) -> io::Result<()> {
        self.drain_outbox_blocking();
        self.channel.call_flush(options)
    }

    pub fn ping(&mut self) -> io::Result<()> {
        self.drain_outbox_blocking();
        self.channel.call_ping()
    }

    pub fn dump(&mut self) -> Result<String, libdd_ipc::codec::DecodeError> {
        self.drain_outbox_blocking();
        self.channel.call_dump()
    }

    pub fn stats(&mut self) -> Result<String, libdd_ipc::codec::DecodeError> {
        self.drain_outbox_blocking();
        self.channel.call_stats()
    }
}
