// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
#[cfg(unix)]
use libdd_crashtracker;
use spawn_worker::Stdio;
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
use crate::service::{init_telemetry_sender, telemetry_action_receiver_task};
use crate::tracer::SHM_LIMITER;
use crate::watchdog::Watchdog;

async fn main_loop<L, C, Fut>(listener: L, cancel: Arc<C>) -> io::Result<()>
where
    L: FnOnce(Box<dyn Fn(IpcClient)>) -> Fut,
    Fut: Future<Output = io::Result<()>>,
    C: Fn() + Sync + Send + 'static,
{
    let counter = Arc::new(AtomicI32::new(0));
    let cloned_counter = Arc::clone(&counter);
    let config = Config::get();
    let max_idle_linger_time = config.idle_linger_time;

    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let mut last_seen_connection_time = Instant::now();

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
        match libdd_crashtracker::get_receiver_unix_socket(socket_path.to_str().unwrap_or_default())
        {
            Ok(listener) => loop {
                if let Err(e) =
                    libdd_crashtracker::async_receiver_entry_point_unix_listener(&listener).await
                {
                    tracing::warn!("Got error while receiving crash report: {e}");
                }
            },
            Err(e) => tracing::error!("Failed setting up the crashtracker listener: {e}"),
        }
    });

    // Init. Early, before we start listening.
    drop(SHM_LIMITER.lock());

    let server = SidecarServer::default();

    // Initialize telemetry sender synchronously before spawning the receiver task
    // This ensures the sender is available immediately, avoiding race conditions
    // where FFI calls might try to send telemetry before the receiver task starts
    if let Some(rx) = init_telemetry_sender() {
        tokio::spawn(telemetry_action_receiver_task(server.clone(), rx));
    }

    let (shutdown_complete_tx, shutdown_complete_rx) = mpsc::channel::<()>(1);

    let mut watchdog = Watchdog::from_receiver(shutdown_complete_rx);
    if config.max_memory != 0 {
        watchdog.max_memory_usage_bytes = config.max_memory;
    }
    let watchdog_handle = watchdog.spawn_watchdog(server.clone());
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

/// Start or connect to the sidecar daemon, spawning `binary_path` as a standalone executable.
///
/// Does not use any trampoline mechanism.
/// The binary at `binary_path` is fork+exec'd directly with the IPC socket fd passed via
/// the `__DD_INTERNAL_PASSED_FD` environment variable.  All sidecar configuration is
/// communicated via the environment variables produced by [`Config::to_env`].
pub fn start_or_connect_with_exec_binary(
    binary_path: std::path::PathBuf,
    mut cfg: Config,
) -> anyhow::Result<SidecarTransport> {
    let liaison = match cfg.ipc_mode {
        config::IpcMode::Shared => setup::DefaultLiason::ipc_shared(),
        config::IpcMode::InstancePerProcess => setup::DefaultLiason::ipc_per_process(),
    };

    let err = match liaison.attempt_listen() {
        Ok(Some(listener)) => {
            daemonize_exec(listener, binary_path, &mut cfg)?;
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

fn build_child_env(cfg: &mut Config) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = cfg
        .to_env()
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect();
    for (k, v) in cfg.child_env.iter() {
        env.push((k.clone(), v.clone()));
    }
    env.push(("LSAN_OPTIONS".into(), "detect_leaks=0".into()));
    env
}

#[cfg(unix)]
fn daemonize_exec(
    listener: IpcServer,
    binary_path: std::path::PathBuf,
    cfg: &mut Config,
) -> anyhow::Result<()> {
    let (stdout, stderr) = match cfg.log_method {
        config::LogMethod::File(ref path) => {
            match File::options()
                .append(true)
                .truncate(false)
                .create(true)
                .open(path)
            {
                Ok(file) => {
                    let (out, err) = (Stdio::from(&file), Stdio::from(&file));
                    (out, err)
                }
                Err(e) => {
                    tracing::warn!("Failed to open logfile for sidecar: {:?}", e);
                    cfg.log_method = config::LogMethod::Disabled;
                    (Stdio::Null, Stdio::Null)
                }
            }
        }
        config::LogMethod::Disabled => (Stdio::Null, Stdio::Null),
        _ => (Stdio::Inherit, Stdio::Inherit),
    };

    let env = build_child_env(cfg);
    let fd: std::os::unix::io::OwnedFd = listener.into();
    let child =
        spawn_worker::spawn_exec_binary(&binary_path, "ipc-helper", &env, fd, stdout, stderr)
            .context("Could not spawn the sidecar daemon")?;

    // Wait for the intermediate child (the grandchild is now the daemon)
    child
        .wait()
        .map(|_| ())
        .map_err(|e| anyhow::format_err!("wait for sidecar intermediate child failed: {e}"))
}

#[cfg(windows)]
fn daemonize_exec(
    listener: IpcServer,
    binary_path: std::path::PathBuf,
    cfg: &mut Config,
) -> anyhow::Result<()> {
    let (stdout, stderr) = match cfg.log_method {
        config::LogMethod::File(ref path) => {
            match File::options()
                .append(true)
                .truncate(false)
                .create(true)
                .open(path)
            {
                Ok(file) => {
                    let (out, err) = (Stdio::from(&file), Stdio::from(&file));
                    (out, err)
                }
                Err(e) => {
                    tracing::warn!("Failed to open logfile for sidecar: {:?}", e);
                    cfg.log_method = config::LogMethod::Disabled;
                    (Stdio::Null, Stdio::Null)
                }
            }
        }
        config::LogMethod::Disabled => (Stdio::Null, Stdio::Null),
        // Windows Stdio has no Inherit variant; fall back to Null
        _ => (Stdio::Null, Stdio::Null),
    };

    let env = build_child_env(cfg);
    let _child =
        spawn_worker::spawn_exec_binary(&binary_path, "ipc-helper", &env, listener, stdout, stderr)
            .context("Could not spawn the sidecar daemon")?;
    Ok(())
}
