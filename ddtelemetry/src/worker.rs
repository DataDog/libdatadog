// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::DEFAULT_API_VERSION;

use super::{
    data::{self, Application, Dependency, DependencyType, Host, Integration, Log, Telemetry},
    Config,
};
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    str::FromStr,
    sync::mpsc::{sync_channel, Receiver, RecvError, RecvTimeoutError, SyncSender},
    sync::{Arc, Condvar, Mutex},
    time,
};

use anyhow::Result;
use reqwest::{blocking, header};

const TELEMETRY_HEARBEAT_DELAY: time::Duration = time::Duration::from_secs(30);

macro_rules! telemetry_worker_log {
    ($worker:expr , ERROR , $fmt_str:tt, $($arg:tt)*) => {
        if $worker.config.is_telemetry_debug_logging_enabled() {
            eprintln!(concat!("Telemetry worker ERROR: ", $fmt_str), $($arg)*);
        }
    };
    ($worker:expr , DEBUG , $fmt_str:tt, $($arg:tt)*) => {
        if $worker.config.is_telemetry_debug_logging_enabled() {
            println!(concat!("Telemetry worker DEBUG: ", $fmt_str), $($arg)*);
        }
    };
}

#[derive(Debug)]
pub enum TelemetryActions {
    AddDependecy(Dependency),
    SendDependencies,

    AddIntegration(Integration),
    SendIntegrations,

    AddLog((LogIdentifier, Log)),
    SendLogs,

    Start,
    Stop,
    Heartbeat,
}

/// Identifies a logging location uniquely
///
/// The identifier is a single 64 bit integer to save space an memory
/// and to be able to generic on the way different languages handle
///
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct LogIdentifier {
    // Collisions? Never heard of them
    indentifier: u64,
}

#[derive(Debug)]
struct UnfluhsedLogEntry {
    number_skipped: u32,
    log: Log,
}

// Holds the current state of the telemetry worker
struct TelemetryWorkerData {
    started: bool,
    library_config: Vec<(String, String)>,
    unflushed_integrations: Vec<Integration>,
    unflushed_dependencies: Vec<Dependency>,
    unflushed_logs: HashMap<LogIdentifier, UnfluhsedLogEntry>,
    host: Host,
    app: Application,
}

pub struct TelemetryWorker {
    config: Config,
    mailbox: Receiver<TelemetryActions>,
    seq_id: u64,
    runtime_id: String,
    client: blocking::Client,
    deadlines: Scheduler,
    data: TelemetryWorkerData,
}

impl TelemetryWorker {
    fn handle_result(&self, result: Result<()>) {
        if let Err(err) = result {
            telemetry_worker_log!(self, ERROR, "{}", err);
        }
    }

    fn recv_next_action(&self) -> TelemetryActions {
        self.deadlines.recv_next_action(&self.mailbox)
    }

    // Runs a state machine that waits for actions, either from the worker's
    // mailbox, or scheduled actions from the worker's deadline object.
    fn run(mut self) {
        use TelemetryActions::*;
        loop {
            let action = self.recv_next_action();
            telemetry_worker_log!(self, DEBUG, "Handling action {:?}", action);
            match action {
                Start => {
                    let res = self.send_app_started();
                    self.handle_result(res);
                    self.deadlines.schedule_next_heartbeat();
                    self.data.started = true;
                }
                AddDependecy(dep) => {
                    self.data.unflushed_dependencies.push(dep);
                    if self.data.started {
                        self.deadlines.schedule_next_send_dependency();
                    }
                }
                AddIntegration(integration) => {
                    self.data.unflushed_integrations.push(integration);
                    if self.data.started {
                        self.deadlines.schedule_next_send_integration();
                    }
                }
                AddLog((entry, log)) => {
                    self.data
                        .unflushed_logs
                        .entry(entry)
                        .and_modify(|e| e.number_skipped += 1)
                        .or_insert(UnfluhsedLogEntry {
                            number_skipped: 0,
                            log,
                        });
                    if self.data.started {
                        self.deadlines.schedule_next_send_logs();
                    }
                }
                SendDependencies => self.flush_deps(),
                SendIntegrations => self.flush_intgs(),
                SendLogs => self.flush_logs(),
                Heartbeat => {
                    if self.data.started {
                        let res = self.send_heartbeat();
                        self.handle_result(res);
                    }
                    self.deadlines.schedule_next_heartbeat();
                }
                Stop => {
                    if !self.data.started {
                        return;
                    }
                    // TODO: do concurrently when we switch to async implem
                    self.flush_deps();
                    self.flush_intgs();
                    self.flush_logs();
                    let res = self.send_app_stop();
                    self.handle_result(res);
                    return;
                }
            }
        }
    }

    fn flush_deps(&mut self) {
        if !self.data.unflushed_dependencies.is_empty() {
            let res = self.send_dependencies_loaded();
            self.handle_result(res);
            self.deadlines.send_dependency_done();
        }
    }

    fn flush_intgs(&mut self) {
        if !self.data.unflushed_integrations.is_empty() {
            let res = self.send_integrations_change();
            self.handle_result(res);
            self.deadlines.send_integrations_done();
        }
    }

    fn flush_logs(&mut self) {
        if !self.data.unflushed_logs.is_empty() {
            let res = self.send_logs();
            self.handle_result(res);
            self.deadlines.send_logs_done();
        }
    }

    fn send_heartbeat(&mut self) -> Result<()> {
        self.send_payload(data::Payload::AppHearbeat(()))
    }

    fn send_app_stop(&mut self) -> Result<()> {
        self.send_payload(data::Payload::AppClosing(()))
    }

    fn send_app_started(&mut self) -> Result<()> {
        let app_started = data::AppStarted {
            integrations: std::mem::take(&mut self.data.unflushed_integrations),
            dependencies: std::mem::take(&mut self.data.unflushed_dependencies),
            config: std::mem::take(&mut self.data.library_config),
        };
        self.send_payload(data::Payload::AppStarted(app_started))
    }

    fn send_dependencies_loaded(&mut self) -> Result<()> {
        let deps_loaded = data::Payload::AppDependenciesLoaded(data::AppDependenciesLoaded {
            dependencies: std::mem::take(&mut self.data.unflushed_dependencies),
        });
        self.send_payload(deps_loaded)
    }

    fn send_integrations_change(&mut self) -> Result<()> {
        let integrations_change =
            data::Payload::AppIntegrationsChange(data::AppIntegrationsChange {
                integrations: std::mem::take(&mut self.data.unflushed_integrations),
            });
        self.send_payload(integrations_change)
    }

    fn send_logs(&mut self) -> Result<()> {
        let logs = self
            .data
            .unflushed_logs
            .drain()
            .map(|(_, mut e)| {
                use std::fmt::Write;
                if e.number_skipped > 0 {
                    write!(
                        &mut e.log.message,
                        "\nSkipped {} messages",
                        e.number_skipped
                    )
                    .unwrap();
                }
                e.log
            })
            .collect();
        self.send_payload(data::Payload::Logs(logs))
    }

    fn next_seq_id(&mut self) -> u64 {
        self.seq_id += 1;
        self.seq_id
    }

    fn send_payload(&mut self, payload: data::Payload) -> Result<()> {
        let seq_id = self.next_seq_id();
        let tel = Telemetry {
            api_version: DEFAULT_API_VERSION,
            tracer_time: time::SystemTime::now()
                .duration_since(time::SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            runtime_id: &self.runtime_id,
            seq_id,
            host: &self.data.host,
            application: &self.data.app,
            payload,
        };
        telemetry_worker_log!(self, DEBUG, "Sending payload: {:?}", tel);

        self.push_telemetry(&tel)
    }

    fn push_telemetry(&self, payload: &Telemetry) -> Result<()> {
        let mut req = blocking::Request::new(
            http::Method::POST,
            reqwest::Url::from_str(self.config.telemetry_url())?,
        );

        req.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        if let Some(api_key) = self.config.api_key() {
            req.headers_mut()
                .insert("DD-API-KEY", header::HeaderValue::from_str(api_key)?);
        }

        let body = serde_json::to_vec(&payload)?;
        *req.body_mut() = Some(blocking::Body::from(body));

        self.client.execute(req)?.error_for_status()?;
        Ok(())
    }
}

struct InnerTelemetryShutdown {
    is_shutdown: Mutex<bool>,
    condvar: Condvar,
}

impl InnerTelemetryShutdown {
    fn wait_for_shutdown(&self) {
        drop(
            self.condvar
                .wait_while(self.is_shutdown.lock().unwrap(), |is_shutdown| {
                    !*is_shutdown
                })
                .unwrap(),
        )
    }

    fn shutdown_finished(&self) {
        *self.is_shutdown.lock().unwrap() = true;
        self.condvar.notify_all();
    }
}

#[derive(Clone)]
pub struct TelemetryWorkerHandle(SyncSender<TelemetryActions>, Arc<InnerTelemetryShutdown>);

impl TelemetryWorkerHandle {
    pub fn send_start(&self) -> Result<()> {
        Ok(self.0.try_send(TelemetryActions::Start)?)
    }

    pub fn send_stop(&self) -> Result<()> {
        Ok(self.0.try_send(TelemetryActions::Stop)?)
    }

    pub fn add_dependency(&self, name: String, version: Option<String>) -> Result<()> {
        self.0.try_send(TelemetryActions::AddDependecy(Dependency {
            name,
            version,
            hash: None,
            type_: DependencyType::PlatformStandard,
        }))?;
        Ok(())
    }

    pub fn add_integration(
        &self,
        name: String,
        version: Option<String>,
        compatible: Option<bool>,
        enabled: Option<bool>,
        auto_enabled: Option<bool>,
    ) -> Result<()> {
        self.0
            .try_send(TelemetryActions::AddIntegration(Integration {
                name,
                version,
                compatible,
                enabled,
                auto_enabled,
            }))?;
        Ok(())
    }

    pub fn add_log<T: Hash>(
        &self,
        identifier: T,
        message: String,
        level: data::LogLevel,
        stack_trace: Option<String>,
    ) -> Result<()> {
        let mut hasher = DefaultHasher::new();
        identifier.hash(&mut hasher);
        self.0.try_send(TelemetryActions::AddLog((
            LogIdentifier {
                indentifier: hasher.finish(),
            },
            data::Log {
                message,
                level,
                stack_trace,
            },
        )))?;
        Ok(())
    }

    pub fn wait_for_shutdown(&self) {
        self.1.wait_for_shutdown();
    }
}

pub struct TelemetryWorkerBuilder {
    pub host: Host,
    pub application: Application,
    pub runtime_id: Option<String>,
    pub library_config: Vec<(String, String)>,
    pub native_deps: bool,
    pub rust_shared_lib_deps: bool,
}

impl TelemetryWorkerBuilder {
    pub async fn new_fetch_host(
        service_name: String,
        language_name: String,
        language_version: String,
        tracer_version: String,
    ) -> Self {
        Self {
            host: crate::build_host().await,
            application: Application {
                service_name,
                language_name,
                language_version,
                tracer_version,
                ..Default::default()
            },
            runtime_id: None,
            library_config: Vec::new(),
            native_deps: true,
            rust_shared_lib_deps: false,
        }
    }

    pub fn new(
        hostname: String,
        service_name: String,
        language_name: String,
        language_version: String,
        tracer_version: String,
    ) -> Self {
        Self {
            host: Host {
                hostname,
                ..Default::default()
            },
            application: Application {
                service_name,
                language_name,
                language_version,
                tracer_version,
                ..Default::default()
            },
            runtime_id: None,
            library_config: Vec::new(),
            native_deps: true,
            rust_shared_lib_deps: false,
        }
    }

    fn gather_deps(&self) -> Vec<Dependency> {
        Vec::new() // Dummy dependencies
    }

    pub fn run(self) -> TelemetryWorkerHandle {
        let (tx, mailbox) = sync_channel(5000);
        let shutdown = Arc::new(InnerTelemetryShutdown {
            is_shutdown: Mutex::new(false),
            condvar: Condvar::new(),
        });
        let worker_shutdown = shutdown.clone();
        std::thread::spawn(move || {
            let config = Config::read_env_config();
            let unflushed_dependencies = self.gather_deps();
            let worker = TelemetryWorker {
                data: TelemetryWorkerData {
                    started: false,
                    app: self.application,
                    host: self.host,
                    library_config: self.library_config,
                    unflushed_dependencies,
                    unflushed_integrations: Vec::new(),
                    unflushed_logs: HashMap::new(),
                },
                config,
                mailbox,
                seq_id: 0,
                runtime_id: self
                    .runtime_id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                client: reqwest::blocking::Client::new(),
                deadlines: Scheduler::new(),
            };
            worker.run();
            worker_shutdown.shutdown_finished();
        });
        TelemetryWorkerHandle(tx, shutdown)
    }
}

/// Schedules the next action the telemetry worker is supposed to take.
/// `Scheduler::recv_next_action` either waits for the next sheduled action, or
/// returns earlier if we receive a message to process.
///
/// Action are scheduled using the  schedule_next_<action> method.
///
/// Once an action has been executed, the corresponding <action>_done should be called
/// to update the Scheduler state
struct Scheduler {
    heartbeat: time::Instant,
    flush_dependencies: Option<time::Instant>,
    flush_integrations: Option<time::Instant>,
    flush_logs: Option<time::Instant>,
    delays: Delays,
}

// Concrete struct to be able to modify the scheduler delays for testing
struct Delays {
    heartbeat: time::Duration,
    deps_flush: time::Duration,
    intgs_flush: time::Duration,
    logs_flush: time::Duration,
}

impl Default for Delays {
    fn default() -> Self {
        Self {
            heartbeat: time::Duration::from_secs(30),
            deps_flush: time::Duration::from_secs(2),
            intgs_flush: time::Duration::from_secs(2),
            logs_flush: time::Duration::from_secs(60),
        }
    }
}

impl Scheduler {
    fn new() -> Self {
        Self {
            heartbeat: time::Instant::now() + TELEMETRY_HEARBEAT_DELAY,
            flush_dependencies: None,
            flush_integrations: None,
            flush_logs: None,
            delays: Delays::default(),
        }
    }

    fn schedule_next_heartbeat(&mut self) {
        self.heartbeat = time::Instant::now() + self.delays.heartbeat;
    }

    fn schedule_next_send_dependency(&mut self) {
        self.flush_dependencies = Some(time::Instant::now() + self.delays.deps_flush);
    }

    fn schedule_next_send_integration(&mut self) {
        self.flush_integrations = Some(time::Instant::now() + self.delays.intgs_flush);
    }

    fn schedule_next_send_logs(&mut self) {
        // Do not reschedule if a send is already scheduled to prevent stalling
        if self.flush_logs.is_none() {
            self.flush_logs = Some(time::Instant::now() + self.delays.logs_flush);
        }
    }

    fn send_dependency_done(&mut self) {
        self.flush_dependencies = None;
        self.schedule_next_heartbeat();
    }

    fn send_integrations_done(&mut self) {
        self.flush_integrations = None;
        self.schedule_next_heartbeat();
    }

    fn send_logs_done(&mut self) {
        self.flush_logs = None;
        self.schedule_next_heartbeat();
    }

    #[inline(always)]
    fn deadlines(&self) -> impl Iterator<Item = (time::Instant, TelemetryActions)> {
        IntoIterator::into_iter([
            Some((self.heartbeat, TelemetryActions::Heartbeat)),
            self.flush_dependencies
                .map(|d| (d, TelemetryActions::SendDependencies)),
            self.flush_integrations
                .map(|d| (d, TelemetryActions::SendIntegrations)),
            self.flush_logs.map(|d| (d, TelemetryActions::SendLogs)),
        ])
        .flatten()
    }

    fn next_deadline(&self) -> Option<(time::Instant, TelemetryActions)> {
        // Unwrap safe because there always is the heartbeat in the iterator
        self.deadlines().min_by_key(|(d, _)| *d)
    }

    fn recv_next_action(&self, mailbox: &Receiver<TelemetryActions>) -> TelemetryActions {
        if let Some((deadline, deadline_action)) = self.next_deadline() {
            // This circus is necessary because Receiver::recv_deadline has been unstable for 4 years!!
            // https://github.com/rust-lang/rust/issues/46316
            let timeout = match deadline.checked_duration_since(time::Instant::now()) {
                None => return deadline_action,
                Some(timeout) => timeout,
            };
            match mailbox.recv_timeout(timeout) {
                Ok(mailbox_action) => mailbox_action,
                Err(RecvTimeoutError::Timeout) => deadline_action,
                Err(RecvTimeoutError::Disconnected) => TelemetryActions::Stop,
            }
        } else {
            match mailbox.recv() {
                Err(RecvError) => TelemetryActions::Stop,
                Ok(action) => action,
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::mem::discriminant;
    use std::time::{Duration, Instant};

    fn expect_scheduled(
        scheduler: &Scheduler,
        expected_action: TelemetryActions,
        expected_scheduled_after: Duration,
    ) {
        let next_deadline = scheduler.next_deadline().unwrap();
        let scheduled_in = next_deadline.0.duration_since(Instant::now());

        assert_eq!(
            discriminant(&next_deadline.1),
            discriminant(&expected_action)
        );
        assert!(
            expected_scheduled_after - Duration::from_millis(1) < scheduled_in
                && scheduled_in < expected_scheduled_after
        );
    }

    #[test]
    fn test_scheduler_next_heartbeat() {
        let mut scheduler = Scheduler::new();

        let next_deadline = scheduler.next_deadline().unwrap();
        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );

        scheduler.schedule_next_heartbeat();

        let next_deadline2 = scheduler.next_deadline().unwrap();
        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );

        assert!(next_deadline.0 < next_deadline2.0)
    }

    #[test]
    fn test_scheduler_send_dependency() {
        let mut scheduler = Scheduler::new();

        let flush_delay_ms = 222;
        scheduler.delays.deps_flush = Duration::from_millis(flush_delay_ms);

        scheduler.schedule_next_send_dependency();
        expect_scheduled(
            &scheduler,
            TelemetryActions::SendDependencies,
            scheduler.delays.deps_flush,
        );
        scheduler.send_dependency_done();

        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );
    }

    #[test]
    fn test_scheduler_send_integrations() {
        let mut scheduler = Scheduler::new();

        let flush_delay_ms = 333;
        scheduler.delays.intgs_flush = Duration::from_millis(flush_delay_ms);

        scheduler.schedule_next_send_integration();
        expect_scheduled(
            &scheduler,
            TelemetryActions::SendIntegrations,
            scheduler.delays.intgs_flush,
        );

        scheduler.send_integrations_done();

        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );
    }

    #[test]
    fn test_scheduler_send_logs() {
        let mut scheduler = Scheduler::new();

        let flush_delay_ms = 99;
        scheduler.delays.logs_flush = Duration::from_millis(flush_delay_ms);

        scheduler.schedule_next_send_logs();
        expect_scheduled(
            &scheduler,
            TelemetryActions::SendLogs,
            scheduler.delays.logs_flush,
        );

        scheduler.send_logs_done();

        expect_scheduled(
            &scheduler,
            TelemetryActions::Heartbeat,
            scheduler.delays.heartbeat,
        );
    }
}
