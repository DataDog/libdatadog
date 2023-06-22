// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use spawn_worker::{entrypoint, getpid, Stdio};

use std::fs::File;
use std::os::unix::net::UnixListener as StdUnixListener;

use std::time::{self, Instant};
use std::{
    io::{self},
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::Duration,
};
use futures::future;
use manual_future::ManualFuture;
use tokio::select;

use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::interface::blocking::TelemetryTransport;
use crate::interface::TelemetryServer;
use datadog_ipc::platform::Channel as IpcChannel;
use ddtelemetry::data::metrics::{MetricNamespace, MetricType};
use ddtelemetry::metrics::ContextKey;
use ddtelemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerHandle};

use crate::setup::{self, Liaison};

use crate::config::{self, Config};

struct MetricData<'a> {
    worker: &'a TelemetryWorkerHandle,
    server: &'a TelemetryServer,
    submitted_payloads: ContextKey,
    active_sessions: ContextKey,
}
impl<'a> MetricData<'a> {
    async fn send(&self, key: ContextKey, value: f64) {
        let _ = self.worker.send_msg(TelemetryActions::AddPoint((value, key, vec![]))).await;
    }

    async fn collect_and_send(&self) {
        future::join_all(vec![
            self.send(self.submitted_payloads, self.server.submitted_payloads.swap(0, Ordering::SeqCst) as f64),
            self.send(self.active_sessions, self.server.active_session_count() as f64),
        ]).await;
    }
}

fn self_telemetry(server: TelemetryServer, mut shutdown_receiver: Receiver<()>) -> JoinHandle<()> {
    let (future, completer) = ManualFuture::new();
    server.self_telemetry_config.lock().unwrap().replace(completer);
    tokio::spawn(async move {
        if let Ok((worker, join_handle)) = TelemetryWorkerBuilder::new_fetch_host("datadog-ipc-helper".to_string(), "php".to_string(), "".to_string(), "".to_string()).spawn_with_config(future.await).await {
            let metrics = MetricData {
                worker: &worker,
                server: &server,
                submitted_payloads: worker.register_metric_context("sidecar.submitted_payloads".to_string(), vec![], MetricType::Count, true, MetricNamespace::Trace),
                active_sessions: worker.register_metric_context("sidecar.active_sessions".to_string(), vec![], MetricType::Gauge, true, MetricNamespace::Trace),
            };

            let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start)).await;
            loop {
                select! {
                    _ = tokio::time::sleep(Duration::from_secs(3)) => { // TODO: increase to 60 sec
                        metrics.collect_and_send().await;
                        let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushMetricAggr)).await;
                        let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData)).await;
                    },
                    _ = shutdown_receiver.recv() => {
                        metrics.collect_and_send().await;
                        let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop)).await;
                        let _ = join_handle.await;
                        return
                    },
                }
            }
        } else {
            shutdown_receiver.recv().await;
        }
    })
}

async fn main_loop(listener: UnixListener) -> tokio::io::Result<()> {
    let counter = Arc::new(AtomicI32::new(0));
    let token = CancellationToken::new();
    let cloned_counter = Arc::clone(&counter);
    let cloned_token = token.clone();

    tokio::spawn(async move {
        let mut last_seen_connection_time = time::Instant::now();
        let max_idle_linger_time = config::Config::get().idle_linger_time;

        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;

            if cloned_counter.load(Ordering::Acquire) > 0 {
                last_seen_connection_time = time::Instant::now();
            }

            if last_seen_connection_time.elapsed() > max_idle_linger_time {
                cloned_token.cancel();
                tracing::info!("No active connections - shutting down");
                break;
            }
        }
    });

    let cloned_token = token.clone();
    tokio::spawn(async move {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!("Error setting up signal handler {}", err);
        }
        tracing::info!("Received Ctrl-C Signal, shutting down");
        cloned_token.cancel();
    });

    let server = TelemetryServer::default();
    let (shutdown_complete_tx, shutdown_complete_rx) = mpsc::channel::<()>(1);
    let telemetry_handle = self_telemetry(server.clone(), shutdown_complete_rx);

    loop {
        let (socket, _) = select! {
            res = listener.accept() => {
                res?
            },
            _ = token.cancelled() => {
                break
            },
        };

        tracing::info!("connection accepted");
        counter.fetch_add(1, Ordering::AcqRel);

        let cloned_counter = Arc::clone(&counter);
        let server = server.clone();
        let shutdown_complete_tx = shutdown_complete_tx.clone();
        tokio::spawn(async move {
            server.accept_connection(socket).await;
            cloned_counter.fetch_add(-1, Ordering::AcqRel);
            tracing::info!("connection closed");

            // Once all tx/senders are dropped the receiver will complete
            drop(shutdown_complete_tx);
        });
    }
    // Shutdown final sender so the receiver can complete
    drop(shutdown_complete_tx);
    let _ = telemetry_handle.await;
    Ok(())
}

fn enter_listener_loop(listener: StdUnixListener) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _g = runtime.enter();

    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;

    runtime.block_on(main_loop(listener)).map_err(|e| e.into())
}

#[no_mangle]
pub extern "C" fn ddog_daemon_entry_point() {
    if let Err(err) = nix::unistd::setsid() {
        tracing::error!("Error calling setsid(): {err}")
    }

    #[cfg(target_os = "linux")]
    let _ = prctl::set_name("dd-ipc-helper");

    #[cfg(feature = "tracing")]
    enable_tracing().ok();
    let now = Instant::now();

    if let Some(fd) = spawn_worker::recv_passed_fd() {
        let listener: StdUnixListener = fd.into();
        tracing::info!("Starting sidecar, pid: {}", getpid());
        if let Err(err) = enter_listener_loop(listener) {
            tracing::error!("Error: {err}")
        }
    }

    tracing::info!(
        "shutting down sidecar, pid: {}, total runtime: {:.3}s",
        getpid(),
        now.elapsed().as_secs_f64()
    )
}

fn daemonize(listener: StdUnixListener, cfg: Config) -> io::Result<()> {
    // TODO: allow passing presaved environment
    let mut spawn_cfg = unsafe { spawn_worker::SpawnWorker::new() };
    spawn_cfg
        .pass_fd(listener)
        .stdin(Stdio::Null)
        .daemonize(true)
        .process_name("datadog-ipc-helper")
        .shared_lib_dependencies(cfg.library_dependencies.clone())
        .target(entrypoint!(ddog_daemon_entry_point));
    match cfg.log_method {
        config::LogMethod::File(path) => {
            let file = File::options()
                .write(true)
                .append(true)
                .truncate(false)
                .create(true)
                .open(path)?;
            spawn_cfg.stdout(Stdio::Fd(file.try_clone()?.into()));
            spawn_cfg.stderr(Stdio::Fd(file.into()));
        }
        config::LogMethod::Disabled => {
            spawn_cfg.stdout(Stdio::Null);
            spawn_cfg.stdout(Stdio::Null);
        }
        _ => {}
    }

    let child = spawn_cfg
        .spawn()
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;

    child
        .wait()
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;

    Ok(())
}

pub fn start_or_connect_to_sidecar(cfg: config::Config) -> io::Result<TelemetryTransport> {
    let liaison = match cfg.ipc_mode {
        config::IpcMode::Shared => setup::DefaultLiason::ipc_shared(),
        config::IpcMode::InstancePerProcess => setup::DefaultLiason::ipc_per_process(),
    };

    match liaison.attempt_listen() {
        Ok(Some(listener)) => daemonize(listener, cfg)?,
        Ok(None) => {}
        Err(err) => tracing::error!("Error starting sidecar {}", err),
    }

    Ok(IpcChannel::from(liaison.connect_to_server()?).into())
}

#[cfg(feature = "tracing")]
fn enable_tracing() -> anyhow::Result<()> {
    let subscriber = tracing_subscriber::fmt();

    match config::Config::get().log_method {
        config::LogMethod::Stdout => subscriber.with_writer(io::stdout).init(),
        config::LogMethod::Stderr => subscriber.with_writer(io::stderr).init(),
        config::LogMethod::File(path) => {
            let log_file = std::fs::File::options()
                .create(true)
                .truncate(false)
                .write(true)
                .append(true)
                .open(path)?;
            tracing_subscriber::fmt()
                .with_writer(std::sync::Mutex::new(log_file))
                .init()
        }
        config::LogMethod::Disabled => return Ok(()),
    };

    Ok(())
}
