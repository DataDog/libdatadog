// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use http::HeaderValue;
use libdd_shared_runtime::{ForkSafeRuntime, SharedRuntime, Worker, WorkerHandle};
use rc_x509_client::{
    AbortOnDrop, AppInfo, ConnectionId, EndpointHostname, ShutdownCtl, ShutdownSignal,
    codec::{ClientToServer, DecodingError, ServerToClient},
    connection::{ConnectionEvent, ConnectionUpdate},
    dispatch::new_dispatcher_interconnect,
    entrypoint::{LibraryEntrypoint, Main},
    host_runtime::{Connection, ConnectionErr},
};
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::handler::{ReceivedPayload, run_dispatch_loop};

const IO_QUEUE_CAPACITY: usize = 100;
const DIAL_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTHY_RESET_THRESHOLD: Duration = Duration::from_secs(30);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const BACKOFF_MULTIPLIER: f64 = 2.0;

#[derive(Debug, Error)]
pub enum AiGuardX509Error {
    #[error("invalid site '{site}': {reason}")]
    InvalidSite { site: String, reason: String },
    #[error("invalid API key header value: {0}")]
    InvalidApiKey(String),
    #[error("TLS configuration failed: {0}")]
    Tls(String),
    #[error("failed to spawn worker on runtime: {0}")]
    Runtime(String),
}

#[derive(Debug, Clone)]
pub struct AiGuardX509Config {
    pub site: String,
    pub api_key: String,
    pub app_name: String,
    pub app_version: String,
}

impl AiGuardX509Config {
    pub fn endpoint_hostname(&self) -> Result<EndpointHostname, AiGuardX509Error> {
        let trimmed = self
            .site
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("wss://")
            .trim_start_matches("ws://")
            .trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(AiGuardX509Error::InvalidSite {
                site: self.site.clone(),
                reason: "empty site".to_string(),
            });
        }
        let host = if trimmed.starts_with("config.") {
            trimmed.to_string()
        } else {
            format!("config.{trimmed}")
        };
        EndpointHostname::try_from(host).map_err(|e| AiGuardX509Error::InvalidSite {
            site: self.site.clone(),
            reason: e.to_string(),
        })
    }

    pub fn websocket_url(&self) -> Result<String, AiGuardX509Error> {
        let host = self.endpoint_hostname()?;
        Ok(format!("wss://{host}/api/v2/ws"))
    }
}

#[derive(Debug)]
struct Backoff {
    initial: Duration,
    max: Duration,
    multiplier: f64,
    attempt: u32,
}

impl Backoff {
    fn new() -> Self {
        Self {
            initial: INITIAL_BACKOFF,
            max: MAX_BACKOFF,
            multiplier: BACKOFF_MULTIPLIER,
            attempt: 0,
        }
    }

    fn next_delay(&mut self) -> Duration {
        let raw = self.initial.as_secs_f64() * self.multiplier.powi(self.attempt as i32);
        let capped = raw.min(self.max.as_secs_f64());
        self.attempt = self.attempt.saturating_add(1);
        let jittered = capped / 2.0 + fastrand::f64() * (capped / 2.0);
        Duration::from_secs_f64(jittered)
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[derive(Debug)]
struct WsConnection {
    tx: mpsc::Sender<ClientToServer>,
    rx: Option<mpsc::Receiver<Result<ServerToClient, DecodingError>>>,
}

impl WsConnection {
    fn new(
        tx: mpsc::Sender<ClientToServer>,
        rx: mpsc::Receiver<Result<ServerToClient, DecodingError>>,
    ) -> Self {
        Self { tx, rx: Some(rx) }
    }
}

impl Connection for WsConnection {
    type Incoming = ReceiverStream<Result<ServerToClient, DecodingError>>;

    async fn send(&mut self, payload: ClientToServer) -> Result<(), ConnectionErr> {
        match self.tx.try_send(payload) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ConnectionErr::Closed),
            Err(mpsc::error::TrySendError::Full(_)) => Err(ConnectionErr::QueueFull),
        }
    }

    fn take_recv_stream(&mut self) -> Option<Self::Incoming> {
        self.rx.take().map(ReceiverStream::new)
    }
}

fn build_tls_config() -> Result<Arc<rustls::ClientConfig>, AiGuardX509Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| AiGuardX509Error::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

struct SessionGuard {
    rc_stop: Option<ShutdownCtl>,
    rc_task: Option<AbortOnDrop<()>>,
    fwd_task: Option<AbortOnDrop<()>>,
    connection_id_tx: watch::Sender<Option<ConnectionId>>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(stop) = self.rc_stop.take() {
            stop.shutdown_now();
        }
        self.fwd_task.take();
        self.rc_task.take();
        self.connection_id_tx.send_replace(None);
    }
}

async fn run_session_loop(
    ws_url: String,
    endpoint_hostname: String,
    api_key: HeaderValue,
    app_info: AppInfo,
    tls_config: Arc<rustls::ClientConfig>,
    payload_tx: watch::Sender<Option<ReceivedPayload>>,
    connection_id_tx: watch::Sender<Option<ConnectionId>>,
    stop_token: CancellationToken,
) {
    let (rc_signal, rc_stop) = ShutdownSignal::new();
    let (event_tx, event_rx) = mpsc::channel::<ConnectionUpdate<WsConnection>>(8);

    let (main, mut rc_conn_id_rx) = Main::new(app_info).with_connection_id_watcher();
    let rc_task = AbortOnDrop::from(tokio::spawn(
        main.entrypoint(rc_signal, ReceiverStream::new(event_rx)),
    ));

    let fwd_tx = connection_id_tx.clone();
    let fwd_task = AbortOnDrop::from(tokio::spawn(async move {
        loop {
            let current = rc_conn_id_rx.borrow_and_update().clone();
            if let Some(ref cid) = current {
                info!(connection_id = %cid, "RC x509 handshake complete");
            }
            fwd_tx.send_replace(current);
            if rc_conn_id_rx.changed().await.is_err() {
                break;
            }
        }
    }));

    // Cancelling this future (e.g. on ForkSafeRuntime::before_fork Pause) drops
    // `_guard`, immediately signalling ShutdownCtl and aborting background tasks.
    let mut guard = SessionGuard {
        rc_stop: Some(rc_stop),
        rc_task: Some(rc_task),
        fwd_task: Some(fwd_task),
        connection_id_tx: connection_id_tx.clone(),
    };

    let mut backoff = Backoff::new();

    loop {
        if stop_token.is_cancelled() {
            break;
        }

        let started_at = Instant::now();
        let outcome = run_single_connection(
            &ws_url,
            &endpoint_hostname,
            &api_key,
            &tls_config,
            &event_tx,
            &payload_tx,
            &stop_token,
        )
        .await;

        if stop_token.is_cancelled() {
            break;
        }

        if started_at.elapsed() >= HEALTHY_RESET_THRESHOLD {
            backoff.reset();
        }

        let delay = backoff.next_delay();
        warn!(
            reason = %outcome,
            retry_in_ms = delay.as_millis() as u64,
            ws_url = %ws_url,
            "RC x509 connection ended; scheduling reconnect"
        );

        tokio::select! {
            _ = stop_token.cancelled() => break,
            _ = tokio::time::sleep(delay) => {}
        }
    }

    drop(event_tx);
    if let Some(stop) = guard.rc_stop.take() {
        stop.shutdown_now();
    }
    if let Some(task) = guard.rc_task.take() {
        let _ = tokio::time::timeout(Duration::from_secs(3), task.into_inner()).await;
    }
    guard.fwd_task.take();
    guard.connection_id_tx.send_replace(None);
}

async fn run_single_connection(
    ws_url: &str,
    endpoint_hostname: &str,
    api_key: &HeaderValue,
    tls_config: &Arc<rustls::ClientConfig>,
    event_tx: &mpsc::Sender<ConnectionUpdate<WsConnection>>,
    payload_tx: &watch::Sender<Option<ReceivedPayload>>,
    stop_token: &CancellationToken,
) -> String {
    let mut req = match ws_url.into_client_request() {
        Ok(r) => r,
        Err(e) => return format!("invalid websocket request: {e}"),
    };
    req.headers_mut().insert("DD-API-KEY", api_key.clone());

    let ws_stream = tokio::select! {
        _ = stop_token.cancelled() => return "cancelled during dial".to_string(),
        res = tokio::time::timeout(
            DIAL_TIMEOUT,
            connect_async_tls_with_config(req, None, false, Some(Connector::Rustls(Arc::clone(tls_config)))),
        ) => match res {
            Ok(Ok((stream, resp))) => {
                debug!(status = %resp.status(), ws_url = %ws_url, "RC x509 WebSocket connected");
                stream
            }
            Ok(Err(e)) => return format!("websocket dial error: {e}"),
            Err(_) => return format!("websocket dial timed out after {}s", DIAL_TIMEOUT.as_secs()),
        }
    };

    if event_tx
        .send(ConnectionUpdate::new(ConnectionEvent::Init))
        .await
        .is_err()
    {
        return "client event channel closed on Init".to_string();
    }

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<ClientToServer>(IO_QUEUE_CAPACITY);
    let (incoming_tx, incoming_rx) =
        mpsc::channel::<Result<ServerToClient, DecodingError>>(IO_QUEUE_CAPACITY);
    let io = WsConnection::new(outgoing_tx, incoming_rx);

    let (dispatch_publisher, dispatch_stream, dispatch_responder) = new_dispatcher_interconnect();
    let dispatch_task = AbortOnDrop::from(tokio::spawn(run_dispatch_loop(
        dispatch_stream,
        dispatch_responder,
        payload_tx.clone(),
    )));

    let parsed_host = match EndpointHostname::try_from(endpoint_hostname.to_string()) {
        Ok(h) => h,
        Err(e) => return format!("invalid endpoint hostname: {e}"),
    };

    if event_tx
        .send(ConnectionUpdate::new(ConnectionEvent::Connected(
            io,
            dispatch_publisher,
            parsed_host,
        )))
        .await
        .is_err()
    {
        return "client event channel closed on Connected".to_string();
    }

    let (mut ws_sink, mut ws_read) = ws_stream.split();

    let reason = loop {
        tokio::select! {
            _ = stop_token.cancelled() => {
                let _ = ws_sink.close().await;
                break "shutdown requested".to_string();
            }
            outgoing = outgoing_rx.recv() => {
                let Some(msg) = outgoing else {
                    break "rc-x509-client closed outgoing channel".to_string();
                };
                let encoded: Bytes = msg.into();
                if let Err(e) = ws_sink.send(Message::Binary(encoded)).await {
                    break format!("websocket write error: {e}");
                }
            }
            incoming = ws_read.next() => {
                match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        if data.is_empty() {
                            break "received empty binary frame".to_string();
                        }
                        let decoded = ServerToClient::try_from(data.as_ref());
                        if incoming_tx.send(decoded).await.is_err() {
                            break "rc-x509-client closed incoming channel".to_string();
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(e) = ws_sink.send(Message::Pong(payload)).await {
                            break format!("websocket pong write error: {e}");
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(frame))) => {
                        break format!("websocket closed by server: {frame:?}");
                    }
                    Some(Ok(other)) => {
                        break format!("unexpected non-binary websocket frame: {other:?}");
                    }
                    Some(Err(e)) => {
                        break format!("websocket read error: {e}");
                    }
                    None => {
                        break "websocket stream ended".to_string();
                    }
                }
            }
        }
    };

    drop(incoming_tx);
    drop(dispatch_task);
    let _ = event_tx
        .send(ConnectionUpdate::new(ConnectionEvent::Disconnected))
        .await;
    let _ = event_tx
        .send(ConnectionUpdate::new(ConnectionEvent::Release))
        .await;

    reason
}

#[derive(Debug)]
struct X509SessionWorker {
    ws_url: String,
    endpoint_hostname: String,
    api_key: HeaderValue,
    app_info: AppInfo,
    tls_config: Arc<rustls::ClientConfig>,
    payload_tx: watch::Sender<Option<ReceivedPayload>>,
    connection_id_tx: watch::Sender<Option<ConnectionId>>,
    stop_token: CancellationToken,
}

#[async_trait]
impl Worker for X509SessionWorker {
    async fn trigger(&mut self) {
        if !self.stop_token.is_cancelled() {
            run_session_loop(
                self.ws_url.clone(),
                self.endpoint_hostname.clone(),
                self.api_key.clone(),
                self.app_info.clone(),
                Arc::clone(&self.tls_config),
                self.payload_tx.clone(),
                self.connection_id_tx.clone(),
                self.stop_token.clone(),
            )
            .await;
        }
        std::future::pending::<()>().await;
    }

    async fn run(&mut self) {}

    async fn shutdown(&mut self) {
        self.stop_token.cancel();
    }
}

#[derive(Debug)]
enum RunnerHandle {
    ForkSafeWorker(#[allow(dead_code)] WorkerHandle),
    StandaloneTask(#[allow(dead_code)] AbortOnDrop<()>),
}

#[derive(Debug)]
pub struct AiGuardX509 {
    stop_token: CancellationToken,
    payload_rx: watch::Receiver<Option<ReceivedPayload>>,
    connection_id_rx: watch::Receiver<Option<ConnectionId>>,
    _runner: RunnerHandle,
}

impl AiGuardX509 {
    pub fn start_on_runtime(
        runtime: &ForkSafeRuntime,
        config: AiGuardX509Config,
    ) -> Result<Self, AiGuardX509Error> {
        let (worker, stop_token, payload_rx, connection_id_rx) = build_worker(config)?;
        // restart_on_fork = false: before_fork pauses cleanly via cancellation;
        // parent resumes automatically; forked children spawn a fresh client via product.restart().
        let handle = runtime
            .spawn_worker(worker, false)
            .map_err(|e| AiGuardX509Error::Runtime(e.to_string()))?;

        Ok(Self {
            stop_token,
            payload_rx,
            connection_id_rx,
            _runner: RunnerHandle::ForkSafeWorker(handle),
        })
    }

    pub fn start(config: AiGuardX509Config) -> Result<Self, AiGuardX509Error> {
        let (worker, stop_token, payload_rx, connection_id_rx) = build_worker(config)?;
        let task = AbortOnDrop::from(tokio::spawn(run_session_loop(
            worker.ws_url,
            worker.endpoint_hostname,
            worker.api_key,
            worker.app_info,
            worker.tls_config,
            worker.payload_tx,
            worker.connection_id_tx,
            worker.stop_token,
        )));

        Ok(Self {
            stop_token,
            payload_rx,
            connection_id_rx,
            _runner: RunnerHandle::StandaloneTask(task),
        })
    }

    pub fn payload(&self) -> Option<ReceivedPayload> {
        self.payload_rx.borrow().clone()
    }

    pub fn subscribe_payload(&self) -> watch::Receiver<Option<ReceivedPayload>> {
        self.payload_rx.clone()
    }

    pub fn connection_id(&self) -> Option<String> {
        self.connection_id_rx
            .borrow()
            .as_ref()
            .map(ToString::to_string)
    }

    pub fn subscribe_connection_id(&self) -> watch::Receiver<Option<ConnectionId>> {
        self.connection_id_rx.clone()
    }

    pub fn shutdown(&self) {
        self.stop_token.cancel();
    }
}

impl Drop for AiGuardX509 {
    fn drop(&mut self) {
        self.stop_token.cancel();
    }
}

fn build_worker(
    config: AiGuardX509Config,
) -> Result<
    (
        X509SessionWorker,
        CancellationToken,
        watch::Receiver<Option<ReceivedPayload>>,
        watch::Receiver<Option<ConnectionId>>,
    ),
    AiGuardX509Error,
> {
    let endpoint_hostname = config.endpoint_hostname()?.to_string();
    let ws_url = config.websocket_url()?;
    let api_key = HeaderValue::from_str(&config.api_key)
        .map_err(|e| AiGuardX509Error::InvalidApiKey(e.to_string()))?;
    let tls_config = build_tls_config()?;
    let app_info = AppInfo::new(config.app_name, config.app_version);

    let (payload_tx, payload_rx) = watch::channel(None);
    let (connection_id_tx, connection_id_rx) = watch::channel(None);
    let stop_token = CancellationToken::new();

    let worker = X509SessionWorker {
        ws_url,
        endpoint_hostname,
        api_key,
        app_info,
        tls_config,
        payload_tx,
        connection_id_tx,
        stop_token: stop_token.clone(),
    };

    Ok((worker, stop_token, payload_rx, connection_id_rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_agentless_config_host_and_ws_url() {
        let cfg = AiGuardX509Config {
            site: "datad0g.com".to_string(),
            api_key: "test-key".to_string(),
            app_name: "dd-trace-py".to_string(),
            app_version: "0.1.0".to_string(),
        };
        assert_eq!(cfg.endpoint_hostname().unwrap().to_string(), "config.datad0g.com");
        assert_eq!(
            cfg.websocket_url().unwrap(),
            "wss://config.datad0g.com/api/v2/ws"
        );
    }

    #[test]
    fn backoff_stays_within_bounds_and_resets() {
        let mut b = Backoff::new();
        for _ in 0..10 {
            let d = b.next_delay();
            assert!(d >= Duration::from_millis(500));
            assert!(d <= MAX_BACKOFF);
        }
        b.reset();
        let d = b.next_delay();
        assert!(d >= Duration::from_millis(500) && d <= INITIAL_BACKOFF);
    }

    #[test]
    fn before_fork_does_not_deadlock() {
        let rt = ForkSafeRuntime::new().expect("fork safe runtime");
        let client = AiGuardX509::start_on_runtime(
            &rt,
            AiGuardX509Config {
                site: "127.0.0.1.invalid".to_string(),
                api_key: "dummy".to_string(),
                app_name: "poc-test".to_string(),
                app_version: "0.0.1".to_string(),
            },
        )
        .expect("start_on_runtime");

        std::thread::sleep(Duration::from_millis(50));
        rt.before_fork();
        rt.after_fork_parent().expect("after_fork_parent");
        client.shutdown();
        rt.shutdown(Some(Duration::from_secs(2)))
            .expect("runtime shutdown");
    }

    #[tokio::test]
    #[ignore = "requires live staging DD_API_KEY via dd-auth"]
    async fn live_staging_handshake_smoke() {
        let api_key = std::env::var("DD_API_KEY").expect("DD_API_KEY must be set");
        let site = std::env::var("DD_SITE").unwrap_or_else(|_| "datad0g.com".to_string());
        let client = AiGuardX509::start(AiGuardX509Config {
            site,
            api_key,
            app_name: "dd-trace-py-poc".to_string(),
            app_version: "0.1.0".to_string(),
        })
        .expect("start");

        let mut cid_rx = client.subscribe_connection_id();
        let cid = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(id) = cid_rx.borrow_and_update().clone() {
                    return id;
                }
                cid_rx.changed().await.expect("cid channel open");
            }
        })
        .await
        .expect("timed out waiting for staging ConnectionId");

        println!("STAGING_CONNECTION_ID={cid}");
        client.shutdown();
    }
}
