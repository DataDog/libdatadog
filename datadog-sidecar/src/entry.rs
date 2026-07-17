// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
#[cfg(target_os = "linux")]
use spawn_worker::read_pt_interp_self;
use spawn_worker::{entrypoint, Entrypoint, Stdio};
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
use tokio::sync::{mpsc, oneshot};

use crate::service::blocking::SidecarTransport;
use crate::service::SidecarServer;

use crate::setup::{self, IpcClient, IpcServer, Liaison};

use crate::config::{self, Config};
use crate::self_telemetry::self_telemetry;
use crate::service::{init_telemetry_sender, telemetry_action_receiver_task};
use crate::tracer::SHM_LIMITER;
use crate::watchdog::Watchdog;
use crate::{ddog_daemon_entry_point, setup_daemon_process};

/// Configuration for main_loop behavior
pub struct MainLoopConfig {
    pub enable_ctrl_c_handler: bool,
    pub external_shutdown_rx: Option<oneshot::Receiver<()>>,
    /// Set to false in thread mode so the worker's UID can be obtained on the
    /// first connection and used to fchown the SHM.
    pub init_shm_eagerly: bool,
}

impl Default for MainLoopConfig {
    fn default() -> Self {
        Self {
            enable_ctrl_c_handler: true,
            external_shutdown_rx: None,
            init_shm_eagerly: true,
        }
    }
}

pub async fn main_loop<L, C, Fut>(
    listener: L,
    cancel: Arc<C>,
    loop_config: MainLoopConfig,
) -> io::Result<()>
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

    if let Some(shutdown_rx) = loop_config.external_shutdown_rx {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let _ = shutdown_rx.await;
            tracing::info!("External shutdown signal received");
            cancel();
        });
    }

    if loop_config.enable_ctrl_c_handler {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(err) = tokio::signal::ctrl_c().await {
                tracing::error!("Error setting up signal handler {}", err);
            }
            tracing::info!("Received Ctrl-C Signal, shutting down");
            cancel();
        });
    }

    if loop_config.init_shm_eagerly {
        drop(SHM_LIMITER.lock());
    }

    let server = SidecarServer::default();
    // Initialize telemetry synchronously so both the in-process helper and FFI callers can enqueue
    // actions before the receiver task gets its first poll.
    let (in_process_telemetry, telemetry_rx) = init_telemetry_sender();

    #[cfg(unix)]
    let appsec = config.appsec_config.as_ref().and_then(|appsec_config| {
        crate::appsec::AppSec::start(appsec_config, in_process_telemetry.clone())
    });

    #[cfg(not(unix))]
    drop(in_process_telemetry);

    #[cfg(unix)]
    let server = match appsec.as_ref() {
        Some(appsec) => server.with_appsec_backend(appsec.backend()),
        None => server,
    };

    if let Some(rx) = telemetry_rx {
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
                server.accept_connection(socket).await;
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
    #[cfg(unix)]
    if let Some(appsec) = appsec {
        appsec.shutdown().await;
    }

    Ok(())
}

pub fn enter_listener_loop<F, L, Fut, C>(acquire_listener: F) -> anyhow::Result<()>
where
    F: FnOnce() -> io::Result<(L, C)>,
    L: FnOnce(Box<dyn Fn(IpcClient)>) -> Fut,
    Fut: Future<Output = io::Result<()>>,
    C: Fn() + Sync + Send + 'static,
{
    enter_listener_loop_with_config(acquire_listener, MainLoopConfig::default())
}

pub fn enter_listener_loop_with_config<F, L, Fut, C>(
    acquire_listener: F,
    loop_config: MainLoopConfig,
) -> anyhow::Result<()>
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
        .block_on(main_loop(listener, Arc::new(cancel), loop_config))
        .map_err(|e| e.into())
}

pub fn daemonize(listener: IpcServer, cfg: Config) -> anyhow::Result<()> {
    daemonize_with_entrypoint(listener, cfg, entrypoint!(ddog_daemon_entry_point))
}

fn daemonize_with_entrypoint(
    listener: IpcServer,
    mut cfg: Config,
    daemon_entrypoint: Entrypoint,
) -> anyhow::Result<()> {
    #[allow(unused_unsafe)] // the unix method is unsafe
    let mut spawn_cfg = unsafe { spawn_worker::SpawnWorker::new() };

    #[cfg(target_os = "linux")]
    if cfg.spawn_without_trampoline && read_pt_interp_self().is_some() {
        spawn_cfg.spawn_method(spawn_worker::SpawnMethod::Direct);
    }

    spawn_cfg.target(daemon_entrypoint);

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

    // In ASAN builds the sidecar is the "main object" when exec'd directly by
    // ld.so, so libclang_rt.asan lands behind libc in the link map. ASAN
    // would otherwise abort with "does not come first in initial library list."
    // set_env replaces any inherited ASAN_OPTIONS so getenv in the child finds
    // our value first.
    #[cfg(target_os = "linux")]
    {
        let asan_init =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"__asan_init".as_ptr() as *const _) };
        if !asan_init.is_null() {
            let existing = std::env::var("ASAN_OPTIONS").unwrap_or_default();
            let asan_opts = if existing.is_empty() {
                "verify_asan_link_order=0".to_owned()
            } else {
                format!("{}:verify_asan_link_order=0", existing)
            };
            spawn_cfg.set_env("ASAN_OPTIONS", asan_opts);
        }
    }

    setup_daemon_process(listener, &mut spawn_cfg)?;

    spawn_cfg
        .shared_lib_dependencies(cfg.library_dependencies)
        .wait_spawn()
        .map_err(io::Error::other)
        .context("Could not spawn the sidecar daemon")?;

    Ok(())
}

pub fn start_or_connect_to_sidecar(cfg: Config) -> anyhow::Result<SidecarTransport> {
    start_or_connect_to_sidecar_with_entrypoint(cfg, entrypoint!(ddog_daemon_entry_point))
}

pub fn start_or_connect_to_sidecar_with_entrypoint(
    cfg: Config,
    daemon_entrypoint: Entrypoint,
) -> anyhow::Result<SidecarTransport> {
    // On Windows, named-pipe buffer sizes are fixed at creation time.  Set the global before
    // attempt_listen so that the initial server pipe (created by this process and handed to the
    // daemon) uses the configured size.  The daemon restores the same value at startup so that
    // subsequent try_accept calls also use the right size.
    #[cfg(windows)]
    if cfg.pipe_buffer_size > 0 {
        datadog_ipc::platform::set_pipe_buffer_size(cfg.pipe_buffer_size);
    }

    let liaison = setup::liaison_for_ipc_mode(cfg.ipc_mode);

    let err = match liaison.attempt_listen() {
        Ok(Some(listener)) => {
            daemonize_with_entrypoint(listener, cfg, daemon_entrypoint)?;
            None
        }
        Ok(None) => None,
        err => err.context("Error starting sidecar").err(),
    };

    Ok(SidecarTransport::from(
        liaison
            .connect_to_server()
            .map_err(|e| err.unwrap_or(e.into()))?,
    ))
}
