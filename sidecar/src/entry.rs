// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use spawn_worker::{entrypoint, getpid, Stdio};

use std::fs::File;

use nix::fcntl::{fcntl, OFlag, F_GETFL, F_SETFL};
use nix::sys::socket::{shutdown, Shutdown};
use std::os::unix::prelude::AsRawFd;
use std::time::{self, Instant};
use std::{
    io::{self},
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::Duration,
};
use std::process::Stdio;

use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::interface::blocking::SidecarTransport;
use crate::interface::SidecarServer;
use datadog_ipc::platform::{AsyncChannel, Channel as IpcChannel};

use crate::setup::{self, Liaison};

use crate::config::{self, Config};
use crate::self_telemetry::self_telemetry;


#[cfg(unix)]
type ServerListener = std::os::unix::net::UnixListener;
#[cfg(windows)]
type ServerListener = tokio::net::windows::named_pipe::NamedPipeServer;

async fn main_loop(listener: ServerListener) -> io::Result<()> {
    let counter = Arc::new(AtomicI32::new(0));
    let cloned_counter = Arc::clone(&counter);

    // shutdown to gracefully dequeue, and immediately relinquish ownership of the socket while shutting down
    #[cfg(unix)]
    let cancel = {
        let listener_fd = listener.as_raw_fd();
        move || {
            // We need to drop O_NONBLOCK, as accept() on a shutdown socket will just give EAGAIN instead of EINVAL
            let flags = OFlag::from_bits_truncate(fcntl(listener_fd, F_GETFL).ok().unwrap());
            _ = fcntl(listener_fd, F_SETFL(flags & !OFlag::O_NONBLOCK));
            _ = shutdown(listener_fd, Shutdown::Both);
        }
    };
    #[cfg(windows)]
    let cancel = || {};

    tokio::spawn(async move {
        let mut last_seen_connection_time = time::Instant::now();
        let max_idle_linger_time = config::Config::get().idle_linger_time;

        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;

            if cloned_counter.load(Ordering::Acquire) > 0 {
                last_seen_connection_time = time::Instant::now();
            }

            if last_seen_connection_time.elapsed() > max_idle_linger_time {
                cancel();
                tracing::info!("No active connections - shutting down");
                break;
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

    let server = SidecarServer::default();
    let (shutdown_complete_tx, shutdown_complete_rx) = mpsc::channel::<()>(1);
    let telemetry_handle = self_telemetry(server.clone(), shutdown_complete_rx);

    while let Ok((socket, _)) = listener.accept().await {
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
    // Shutdown final sender so the receiver can complete
    drop(shutdown_complete_tx);
    let _ = telemetry_handle.await;
    _ = server.trace_flusher.join().await;
    Ok(())
}

fn enter_listener_loop(acquire_listener: F) -> anyhow::Result<()> where F: FnOnce() -> io::Result<ServerListener> {
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _g = runtime.enter();

    let listener = acquire_listener()?;

    runtime.block_on(main_loop(listener)).map_err(|e| e.into())
}

fn daemonize(listener: ServerListener, cfg: Config) -> io::Result<()> {
    let mut spawn_cfg = unsafe { spawn_worker::SpawnWorker::new() };
    spawn_cfg
        .pass_fd(listener)
        .stdin(Stdio::Null)
        .daemonize(true)
        .process_name("datadog-ipc-helper")
        .target(entrypoint!(ddog_daemon_entry_point));
    #[cfg(unix)]
    spawn_cfg.shared_lib_dependencies(cfg.library_dependencies.clone());
    for (env, val) in cfg.to_env().into_iter() {
        spawn_cfg.append_env(env, val);
    }
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

pub fn start_or_connect_to_sidecar(cfg: config::Config) -> io::Result<SidecarTransport> {
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
