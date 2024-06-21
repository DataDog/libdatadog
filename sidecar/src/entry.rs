// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
#[cfg(unix)]
use datadog_crashtracker;
use spawn_worker::{entrypoint, Stdio};
use std::fs::File;
use std::future::Future;
use std::{
    io,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[cfg(unix)]
use crate::crashtracker::crashtracker_unix_socket_path;
use crate::service::blocking::SidecarTransport;
use crate::service::SidecarServer;
use datadog_ipc::platform::AsyncChannel;

use crate::setup::{self, IpcClient, IpcServer, Liaison};

use crate::config::{self, Config};
use crate::self_telemetry::self_telemetry;
use crate::tracer::SHM_LIMITER;
use crate::watchdog::Watchdog;
use crate::{ddog_daemon_entry_point, setup_daemon_process};

async fn main_loop<L, C, Fut>(listener: L, cancel: Arc<C>) -> io::Result<()>
where
    L: FnOnce(Box<dyn Fn(IpcClient)>) -> Fut,
    Fut: Future<Output = io::Result<()>>,
    C: Fn() + Sync + Send + 'static,
{
    let counter = Arc::new(AtomicI32::new(0));
    let cloned_counter = Arc::clone(&counter);

    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let mut last_seen_connection_time = Instant::now();
            let max_idle_linger_time = Config::get().idle_linger_time;

            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;

                if cloned_counter.load(Ordering::Acquire) > 0 {
                    last_seen_connection_time = Instant::now();
                }

                if last_seen_connection_time.elapsed() > max_idle_linger_time {
                    cancel();
                    tracing::info!("No active connections - shutting down");
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!("Error setting up signal handler {}", err);
        }
        tracing::info!("Received Ctrl-C Signal, shutting down");
        cancel();
    });

    #[cfg(unix)]
    tokio::spawn(async move {
        let socket_path = crashtracker_unix_socket_path();
        let _ = datadog_crashtracker::async_receiver_entry_point_unix_socket(
            socket_path.to_str().unwrap_or_default(),
            false,
        )
        .await;
    });

    // Init. Early, before we start listening.
    drop(SHM_LIMITER.lock());

    let server = SidecarServer::default();
    let (shutdown_complete_tx, shutdown_complete_rx) = mpsc::channel::<()>(1);

    let watchdog_handle = Watchdog::from_receiver(shutdown_complete_rx).spawn_watchdog();
    let telemetry_handle = self_telemetry(server.clone(), watchdog_handle);

    listener(Box::new({
        let shutdown_complete_tx = shutdown_complete_tx.clone();
        let server = server.clone();
        move |socket| {
            tracing::info!("connection accepted");
            counter.fetch_add(1, Ordering::AcqRel);

            let cloned_counter = Arc::clone(&counter);
            let server = server.clone();
            let shutdown_complete_tx = shutdown_complete_tx.clone();
            tokio::spawn(async move {
                server.accept_connection(AsyncChannel::from(socket)).await;
                cloned_counter.fetch_add(-1, Ordering::AcqRel);
                tracing::info!("connection closed");

                // Once all tx/senders are dropped the receiver will complete
                drop(shutdown_complete_tx);
            });
        }
    }))
    .await?;

    // Shutdown final sender so the receiver can complete
    drop(shutdown_complete_tx);

    // Await everything else to completion
    _ = telemetry_handle.await;
    server.shutdown();
    _ = server.trace_flusher.join().await;

    Ok(())
}

pub fn enter_listener_loop<F, L, Fut, C>(acquire_listener: F) -> anyhow::Result<()>
where
    F: FnOnce() -> io::Result<(L, C)>,
    L: FnOnce(Box<dyn Fn(IpcClient)>) -> Fut,
    Fut: Future<Output = io::Result<()>>,
    C: Fn() + Sync + Send + 'static,
{
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    let runtime = builder.enable_all().build()?;
    let _g = runtime.enter();

    let (listener, cancel) = acquire_listener()?;

    runtime
        .block_on(main_loop(listener, Arc::new(cancel)))
        .map_err(|e| e.into())
}

pub fn daemonize(listener: IpcServer, mut cfg: Config) -> anyhow::Result<()> {
    #[allow(unused_unsafe)] // the unix method is unsafe
    let mut spawn_cfg = unsafe { spawn_worker::SpawnWorker::new() };

    spawn_cfg.target(entrypoint!(ddog_daemon_entry_point));

    match cfg.log_method {
        config::LogMethod::File(ref path) => {
            match File::options()
                .append(true)
                .truncate(false)
                .create(true)
                .open(path)
            {
                Ok(file) => {
                    let (out, err) = (Stdio::from(&file), Stdio::from(&file));
                    spawn_cfg.stdout(out);
                    spawn_cfg.stderr(err);
                }
                Err(e) => {
                    tracing::warn!("Failed to open logfile for sidecar: {:?}", e);
                    cfg.log_method = config::LogMethod::Disabled;
                    spawn_cfg.stdout(Stdio::Null);
                    spawn_cfg.stderr(Stdio::Null);
                }
            }
        }
        config::LogMethod::Disabled => {
            spawn_cfg.stdout(Stdio::Null);
            spawn_cfg.stderr(Stdio::Null);
        }
        _ => {}
    }

    for (env, val) in cfg.to_env().into_iter() {
        spawn_cfg.append_env(env, val);
    }
    spawn_cfg.append_env("LSAN_OPTIONS", "detect_leaks=0");

    setup_daemon_process(listener, &mut spawn_cfg)?;

    let mut lib_deps = cfg.library_dependencies;
    if cfg.appsec_config.is_some() {
        lib_deps.push(spawn_worker::LibDependency::Path(
            cfg.appsec_config.unwrap().shared_lib_path.into(),
        ));
    }

    spawn_cfg
        .shared_lib_dependencies(lib_deps)
        .wait_spawn()
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
        .context("Could not spawn the sidecar daemon")?;

    Ok(())
}

pub fn start_or_connect_to_sidecar(cfg: Config) -> anyhow::Result<SidecarTransport> {
    let liaison = match cfg.ipc_mode {
        config::IpcMode::Shared => setup::DefaultLiason::ipc_shared(),
        config::IpcMode::InstancePerProcess => setup::DefaultLiason::ipc_per_process(),
    };

    let err = match liaison.attempt_listen() {
        Ok(Some(listener)) => {
            daemonize(listener, cfg)?;
            None
        }
        Ok(None) => None,
        err => err.context("Error starting sidecar").err(),
    };

    Ok(liaison
        .connect_to_server()
        .map_err(|e| err.unwrap_or(e.into()))?
        .into())
}
