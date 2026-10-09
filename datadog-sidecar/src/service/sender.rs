// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Higher-level sender with connection state mirroring and telemetry load-shedding.
//!
//! Wraps [`SidecarInterfaceChannel`] with:
//! - A **connection state mirror**: the sidecar keeps the session configuration and the current
//!   application per connection. The last value of each such message is kept here, unchanged values
//!   are not resent, and after a reconnect all of them are replayed to the new connection. Pending
//!   state messages are drained, in order, before any other message, so the sidecar always handles
//!   a message with the state current at the time it was sent.
//! - **Telemetry load-shedding**: when `outstanding > max_outstanding / 2`, 90% of `EnqueueActions`
//!   calls are dropped (telemetry is low priority).
//!
//! `SidecarSender` takes `&mut self`; the caller is responsible for exclusive access.

use crate::service::{
    ApplicationConfig, InstanceId, SessionConfig,
    sidecar_interface::{
        DynamicInstrumentationConfigState, SidecarFlushOptions, SidecarInterfaceChannel,
        SidecarInterfaceClientRequest, SidecarInterfaceRequest,
    },
};
use libdd_common::tag::Tag;
use libdd_telemetry::metrics::MetricContext;
use std::collections::HashMap;
use std::{io, time::Duration};
use tracing::trace;

/// The last value of a connection state message and whether it still needs to be sent.
struct StateSlot<T> {
    value: Option<T>,
    pending: bool,
}

impl<T> Default for StateSlot<T> {
    fn default() -> Self {
        StateSlot {
            value: None,
            pending: false,
        }
    }
}

impl<T> StateSlot<T> {
    fn replace(&mut self, value: T) {
        self.value = Some(value);
        self.pending = true;
    }

    fn pending(&self) -> Option<&T> {
        self.value.as_ref().filter(|_| self.pending)
    }

    /// Offers the request for the pending value to `submit`. Returns whether nothing is pending
    /// anymore.
    fn send_with(
        &mut self,
        submit: &mut impl FnMut(&SidecarInterfaceRequest) -> bool,
        request: impl FnOnce(&T) -> SidecarInterfaceRequest,
    ) -> bool {
        let Some(value) = self.pending() else {
            return true;
        };
        if !submit(&request(value)) {
            return false;
        }
        self.pending = false;
        true
    }
}

impl<T: PartialEq> StateSlot<T> {
    /// Returns whether the value changed.
    fn set(&mut self, value: T) -> bool {
        if self.value.as_ref() == Some(&value) {
            return false;
        }
        self.replace(value);
        true
    }
}

struct ConnectionConfig {
    instance_id: InstanceId,
    #[cfg(windows)]
    remote_config_notify_target: Option<crate::service::remote_configs::RemoteConfigNotifyTarget>,
    config: SessionConfig,
}

/// Mirror of the state the sidecar associates with this connection.
#[derive(Default)]
struct ConnectionState {
    connection_config: StateSlot<ConnectionConfig>,
    process_tags: StateSlot<Vec<Tag>>,
    user_service_defined: StateSlot<bool>,
    test_session_token: StateSlot<String>,
    application: StateSlot<Option<ApplicationConfig>>,
    /// The remote config generation the application was last set with, replayed with it.
    application_generation: u64,
    /// The generation to send with the pending application message, if it differs.
    pending_application_generation: Option<u64>,
}

impl ConnectionState {
    /// Offers the pending messages to `submit`, in the order they must be sent in: the
    /// configuration first, the application last. Stops at the first message `submit` refuses,
    /// which stays pending with all following ones. Returns whether everything was sent.
    fn send_pending(&mut self, mut submit: impl FnMut(&SidecarInterfaceRequest) -> bool) -> bool {
        if !self.connection_config.send_with(&mut submit, |connection| {
            SidecarInterfaceRequest::SetConnectionConfig {
                instance_id: connection.instance_id.clone(),
                #[cfg(windows)]
                remote_config_notify_target: connection.remote_config_notify_target.clone(),
                config: connection.config.clone(),
            }
        }) {
            return false;
        }
        if !self.process_tags.send_with(&mut submit, |process_tags| {
            SidecarInterfaceRequest::SetProcessTags {
                process_tags: process_tags.clone(),
            }
        }) {
            return false;
        }
        if !self
            .user_service_defined
            .send_with(&mut submit, |is_defined| {
                SidecarInterfaceRequest::SetUserServiceDefined {
                    is_defined: *is_defined,
                }
            })
        {
            return false;
        }
        if !self.test_session_token.send_with(&mut submit, |token| {
            SidecarInterfaceRequest::SetTestSessionToken {
                token: token.clone(),
            }
        }) {
            return false;
        }
        let generation = self
            .pending_application_generation
            .unwrap_or(self.application_generation);
        if !self.application.send_with(&mut submit, |application| {
            SidecarInterfaceRequest::SetApplication {
                application: application.clone(),
                remote_config_generation: generation,
            }
        }) {
            return false;
        }
        self.pending_application_generation = None;
        true
    }

    /// Marks the whole state as to be sent, for a new connection or a new session on it.
    fn resend_all(&mut self) {
        self.connection_config.pending = self.connection_config.value.is_some();
        self.process_tags.pending = self.process_tags.value.is_some();
        self.user_service_defined.pending = self.user_service_defined.value.is_some();
        self.test_session_token.pending = self.test_session_token.value.is_some();
        // A new connection has no application to clear.
        self.application.pending = matches!(self.application.value, Some(Some(_)));
        self.pending_application_generation = None;
    }
}

/// Higher-level IPC sender with connection state mirroring and telemetry load-shedding.
pub struct SidecarSender {
    pub channel: SidecarInterfaceChannel,
    state: ConnectionState,
    /// Maximum allowed outstanding (sent-but-not-acked) messages before state drain is skipped
    /// and fire-and-forget sends are blocked.
    pub max_outstanding: u64,
    /// Cycles 0–9; used to implement 90% telemetry drop under backpressure.
    enqueue_actions_counter: u8,
    /// All metric registrations ever sent on this transport (keyed by name).
    /// Persisted across reconnects; replayed on new connections before any metric points.
    pub metric_registrations: HashMap<String, MetricContext>,
}

impl SidecarSender {
    pub fn new(channel: SidecarInterfaceChannel) -> Self {
        Self {
            channel,
            state: ConnectionState::default(),
            max_outstanding: 100,
            enqueue_actions_counter: 0,
            metric_registrations: HashMap::new(),
        }
    }

    /// Takes over the connection state of the sender this one replaces after a reconnect. The
    /// state is sent to the new connection before anything else.
    pub fn adopt_state(&mut self, previous: SidecarSender) {
        self.state = previous.state;
        self.state.resend_all();
        // The new connection may reach another sidecar, unrelated to what the client has read.
        self.state.application_generation = 0;
        self.max_outstanding = previous.max_outstanding;
        // Replay all registered metrics
        for metric in previous.metric_registrations.into_values() {
            self.register_telemetry_metric(metric);
        }
    }

    /// Non-blocking drain of pending state messages.  Returns `true` if all messages were sent.
    fn try_drain_outbox(&mut self) -> bool {
        // Drain pending acks when approaching the throttle threshold so the socket
        // receive buffer doesn't fill up and block the sidecar from sending more acks.
        if self.channel.0.outstanding() >= self.max_outstanding / 2 {
            self.channel.0.drain_acks();
        }
        let (channel, max_outstanding) = (&mut self.channel, self.max_outstanding);
        self.state.send_pending(|request| {
            channel.0.outstanding() < max_outstanding && channel.try_send_request(request)
        })
    }

    /// Blocking drain of pending state messages (used before blocking calls).
    fn drain_outbox_blocking(&mut self) {
        let channel = &mut self.channel;
        self.state.send_pending(|request| {
            channel.send_request_blocking(request).ok();
            true
        });
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

    pub fn set_connection_config(
        &mut self,
        instance_id: InstanceId,
        #[cfg(windows)] remote_config_notify_target: Option<
            crate::service::remote_configs::RemoteConfigNotifyTarget,
        >,
        config: SessionConfig,
    ) {
        self.state.connection_config.replace(ConnectionConfig {
            instance_id,
            #[cfg(windows)]
            remote_config_notify_target,
            config,
        });
        // The sidecar starts over with a new session: all state layered on it is sent again.
        self.state.resend_all();
        self.try_drain_outbox();
    }

    /// Moves the connection to another instance, as in a fork child. The application is
    /// registered again, with the remote config generation last read by the client.
    pub fn set_instance_id(&mut self, instance_id: InstanceId, remote_config_generation: u64) {
        if let Some(connection) = self.state.connection_config.value.as_mut() {
            connection.instance_id = instance_id;
        }
        // Like a new configuration, the new instance starts over with a new session.
        self.state.resend_all();
        self.state.application_generation = remote_config_generation;
        self.try_drain_outbox();
    }

    pub fn set_process_tags(&mut self, process_tags: Vec<Tag>) {
        if self.state.process_tags.set(process_tags) {
            self.try_drain_outbox();
        }
    }

    pub fn set_user_service_defined(&mut self, is_defined: bool) {
        if self.state.user_service_defined.set(is_defined) {
            self.try_drain_outbox();
        }
    }

    pub fn set_test_session_token(&mut self, token: String) {
        if self.state.test_session_token.set(token) {
            self.try_drain_outbox();
        }
    }

    /// Sets the application of the current request, `None` at request end.
    ///
    /// The remote config generation is not compared: an unchanged application does not need
    /// to re-register for remote config.
    pub fn set_application(
        &mut self,
        application: Option<ApplicationConfig>,
        remote_config_generation: u64,
    ) {
        if self.state.application.set(application) {
            self.state.application_generation = remote_config_generation;
            self.state.pending_application_generation = None;
            self.try_drain_outbox();
        }
    }

    /// Updates the dynamic instrumentation state of the current application, if any.
    pub fn set_dynamic_instrumentation_state(&mut self, state: DynamicInstrumentationConfigState) {
        let Some(Some(application)) = &self.state.application.value else {
            return;
        };
        if application.dynamic_instrumentation_state == state {
            return;
        }
        let mut application = application.clone();
        application.dynamic_instrumentation_state = state;
        // Not a reason to check for remote config updates. An application whose registration is
        // still to be sent keeps its generation though, it is merely sent with the new state.
        if !self.state.application.pending {
            self.state.pending_application_generation = Some(u64::MAX);
        }
        self.state.application.replace(Some(application));
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

    /// Sends a request after the pending state, unless backpressure drops it. Returns whether it
    /// was sent: if not, a closed connection tells that it broke instead.
    pub fn send(&mut self, request: &SidecarInterfaceRequest) -> bool {
        self.try_drain_outbox() && self.channel.try_send_request(request)
    }

    /// Like [`Self::send`], for telemetry actions: when `outstanding > max_outstanding / 2`, 90%
    /// of them are dropped to shed load.
    pub fn send_actions(&mut self, request: &SidecarInterfaceRequest) -> bool {
        self.may_enqueue_actions() && self.channel.try_send_request(request)
    }

    fn may_enqueue_actions(&mut self) -> bool {
        if !self.try_drain_outbox() {
            return false;
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
                return false;
            }
            // The 10% that passes through falls to the try_send.
        }
        true
    }

    pub fn set_read_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.channel.0.set_read_timeout(d)
    }

    pub fn set_write_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.channel.0.set_write_timeout(d)
    }

    pub fn ensure_appsec_started(
        &mut self,
        request: &SidecarInterfaceRequest,
    ) -> Result<bool, libdd_ipc::codec::DecodeError> {
        self.channel.call_request_blocking(request)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::service::DynamicInstrumentationConfigState;
    use libdd_ipc::SeqpacketConn;

    fn sender() -> (SidecarSender, SeqpacketConn) {
        let (conn, peer) = SeqpacketConn::socketpair().unwrap();
        (SidecarSender::new(SidecarInterfaceChannel::new(conn)), peer)
    }

    /// Decodes all requests sent to `peer` so far.
    fn received(peer: &SeqpacketConn) -> Vec<SidecarInterfaceRequest> {
        let mut buf = vec![0; libdd_ipc::max_message_size()];
        let mut requests = vec![];
        loop {
            match peer.try_recv_raw(&mut buf) {
                Ok((len, _)) => requests.push(libdd_ipc::codec::decode(&buf[..len]).unwrap()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return requests,
                Err(e) => panic!("receiving from the sender failed: {e}"),
            }
        }
    }

    fn application(service_name: &str) -> ApplicationConfig {
        ApplicationConfig {
            service_name: service_name.to_owned(),
            env_name: "env".to_owned(),
            app_version: "1.0".to_owned(),
            global_tags: vec![],
            dynamic_instrumentation_state: DynamicInstrumentationConfigState::NotSet,
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn unchanged_application_is_sent_once() {
        let (mut sender, peer) = sender();
        sender.set_application(Some(application("svc")), 1);
        sender.set_application(Some(application("svc")), 1);

        match received(&peer).as_slice() {
            [
                SidecarInterfaceRequest::SetApplication {
                    application: sent,
                    remote_config_generation,
                },
            ] => {
                assert_eq!(sent.as_ref(), Some(&application("svc")));
                assert_eq!(*remote_config_generation, 1);
            }
            other => panic!("expected a single SetApplication, got {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn adopted_state_is_replayed_in_order_before_the_next_message() {
        let instance_id = InstanceId::new("session", "runtime");
        let process_tags = vec![Tag::new("entrypoint.name", "php").unwrap()];
        let (mut previous, previous_peer) = sender();
        previous.set_connection_config(instance_id.clone(), SessionConfig::for_test());
        previous.set_process_tags(process_tags.clone());
        previous.set_application(Some(application("svc")), 7);
        assert_eq!(received(&previous_peer).len(), 3);

        let (mut sender, peer) = sender();
        sender.adopt_state(previous);
        // The state is replayed lazily, ahead of the next message.
        assert!(received(&peer).is_empty());
        sender.send(&SidecarInterfaceRequest::SendDogstatsdActions { actions: vec![] });

        match received(&peer).as_slice() {
            [
                SidecarInterfaceRequest::SetConnectionConfig {
                    instance_id: replayed_instance_id,
                    config,
                },
                SidecarInterfaceRequest::SetProcessTags {
                    process_tags: replayed_process_tags,
                },
                SidecarInterfaceRequest::SetApplication {
                    application: replayed_application,
                    remote_config_generation,
                },
                SidecarInterfaceRequest::SendDogstatsdActions { actions },
            ] => {
                assert_eq!(replayed_instance_id, &instance_id);
                assert_eq!(config.language, "php");
                assert_eq!(replayed_process_tags, &process_tags);
                assert_eq!(replayed_application.as_ref(), Some(&application("svc")));
                // The new connection may reach another sidecar, which notifies about anything.
                assert_eq!(*remote_config_generation, 0);
                assert!(actions.is_empty());
            }
            other => panic!("unexpected requests after adopting the state: {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn new_instance_replays_the_state_with_the_given_generation() {
        let process_tags = vec![Tag::new("entrypoint.name", "php").unwrap()];
        let (mut previous, previous_peer) = sender();
        previous.set_connection_config(
            InstanceId::new("session", "parent"),
            SessionConfig::for_test(),
        );
        previous.set_process_tags(process_tags.clone());
        previous.set_application(Some(application("svc")), 7);
        assert_eq!(received(&previous_peer).len(), 3);

        let (mut sender, peer) = sender();
        sender.adopt_state(previous);
        let child = InstanceId::new("session", "child");
        sender.set_instance_id(child.clone(), 9);

        match received(&peer).as_slice() {
            [
                SidecarInterfaceRequest::SetConnectionConfig { instance_id, .. },
                SidecarInterfaceRequest::SetProcessTags {
                    process_tags: replayed_process_tags,
                },
                SidecarInterfaceRequest::SetApplication {
                    application: replayed_application,
                    remote_config_generation,
                },
            ] => {
                assert_eq!(instance_id, &child);
                assert_eq!(replayed_process_tags, &process_tags);
                assert_eq!(replayed_application.as_ref(), Some(&application("svc")));
                assert_eq!(*remote_config_generation, 9);
            }
            other => panic!("unexpected requests for the new instance: {other:?}"),
        }
    }

    /// The dynamic instrumentation state and remote config generation of the application message
    /// among `requests`.
    fn sent_application(
        requests: &[SidecarInterfaceRequest],
    ) -> (DynamicInstrumentationConfigState, u64) {
        match requests
            .iter()
            .find(|r| matches!(r, SidecarInterfaceRequest::SetApplication { .. }))
        {
            Some(SidecarInterfaceRequest::SetApplication {
                application: Some(application),
                remote_config_generation,
            }) => (
                application.dynamic_instrumentation_state,
                *remote_config_generation,
            ),
            _ => panic!("expected a SetApplication, got {requests:?}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn instrumentation_update_of_a_registered_application_requests_no_notification() {
        let (mut sender, peer) = sender();
        sender.set_application(Some(application("svc")), 3);
        assert_eq!(received(&peer).len(), 1);

        sender.set_dynamic_instrumentation_state(DynamicInstrumentationConfigState::Enabled);
        assert_eq!(
            sent_application(&received(&peer)),
            (DynamicInstrumentationConfigState::Enabled, u64::MAX)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn instrumentation_update_keeps_the_generation_of_an_unsent_application() {
        let (mut sender, peer) = sender();
        // Backpressure: nothing is sent.
        sender.max_outstanding = 0;
        sender.set_application(Some(application("svc")), 3);
        sender.set_dynamic_instrumentation_state(DynamicInstrumentationConfigState::Enabled);
        assert!(received(&peer).is_empty());

        sender.max_outstanding = 100;
        sender.send(&SidecarInterfaceRequest::SendDogstatsdActions { actions: vec![] });
        assert_eq!(
            sent_application(&received(&peer)),
            (DynamicInstrumentationConfigState::Enabled, 3)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn instrumentation_update_keeps_the_generation_of_a_replayed_application() {
        let (mut previous, previous_peer) = sender();
        previous.set_application(Some(application("svc")), 3);
        assert_eq!(received(&previous_peer).len(), 1);

        let (mut sender, peer) = sender();
        sender.adopt_state(previous);
        sender.set_dynamic_instrumentation_state(DynamicInstrumentationConfigState::Enabled);
        sender.send(&SidecarInterfaceRequest::SendDogstatsdActions { actions: vec![] });
        assert_eq!(
            sent_application(&received(&peer)),
            (DynamicInstrumentationConfigState::Enabled, 0)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn replaced_configuration_resends_the_state_layered_on_it() {
        let (mut sender, peer) = sender();
        let instance_id = InstanceId::new("session", "runtime");
        sender.set_connection_config(instance_id.clone(), SessionConfig::for_test());
        sender.set_user_service_defined(true);
        sender.set_test_session_token("token".to_owned());
        sender.set_application(Some(application("svc")), 3);
        assert_eq!(received(&peer).len(), 4);

        sender.set_connection_config(instance_id, SessionConfig::for_test());
        // Unchanged since the previous session, which the sidecar replaced.
        sender.set_user_service_defined(true);

        match received(&peer).as_slice() {
            [
                SidecarInterfaceRequest::SetConnectionConfig { .. },
                SidecarInterfaceRequest::SetUserServiceDefined { is_defined: true },
                SidecarInterfaceRequest::SetTestSessionToken { token },
                SidecarInterfaceRequest::SetApplication {
                    application: replayed,
                    remote_config_generation: 3,
                },
            ] => {
                assert_eq!(token, "token");
                assert_eq!(replayed.as_ref(), Some(&application("svc")));
            }
            other => panic!("unexpected requests for the new session: {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn cleared_application_is_not_replayed() {
        let (mut previous, _previous_peer) = sender();
        previous.set_connection_config(
            InstanceId::new("session", "runtime"),
            SessionConfig::for_test(),
        );
        previous.set_application(Some(application("svc")), 0);
        previous.set_application(None, 0);

        let (mut sender, peer) = sender();
        sender.adopt_state(previous);
        sender.send(&SidecarInterfaceRequest::SendDogstatsdActions { actions: vec![] });

        match received(&peer).as_slice() {
            [
                SidecarInterfaceRequest::SetConnectionConfig { .. },
                SidecarInterfaceRequest::SendDogstatsdActions { .. },
            ] => {}
            other => panic!("unexpected requests after adopting the state: {other:?}"),
        }
    }
}
