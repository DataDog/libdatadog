// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::fs;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::sync::Mutex;
use std::time::{self, Instant};
use std::{
    io::{self},
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::select;

use nix::{sys::wait::waitpid, unistd::Pid};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;


use crate::ipc::interface::blocking::TelemetryTransport;
use crate::ipc::interface::TelemetryServer;
use crate::ipc::platform::Channel as IpcChannel;

use crate::{
    fork::{fork_fn, getpid},
    ipc::setup::{self, Liaison},
};

use super::config;

async fn main_loop(listener: UnixListener) -> tokio::io::Result<()> {
    let counter = Arc::new(AtomicI32::new(0));
    let token = CancellationToken::new();
    let cloned_counter = Arc::clone(&counter);
    let cloned_token = token.clone();

    tokio::spawn(async move {
        let mut consecutive_no_active_connections = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            if cloned_counter.load(Ordering::Acquire) <= 0 {
                consecutive_no_active_connections += 1;
            } else {
                consecutive_no_active_connections = 0;
            }

            if consecutive_no_active_connections > 1 {
                cloned_token.cancel();
                tracing::info!("no active connections - shutting down");
            }
        }
    });

    let server = TelemetryServer::default();

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
        tokio::spawn(async move {
            server.accept_connection(socket).await;
            cloned_counter.fetch_add(-1, Ordering::AcqRel);
            tracing::info!("connection closed");
        });
    }
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

fn daemonize(listener: StdUnixListener) -> io::Result<()> {
    unsafe {
        let pid = fork_fn(listener, |listener| {
            fork_fn(listener, |listener| {
                let now = Instant::now();
                tracing::info!(
                    "[{}] starting sidecar, pid: {}",
                    time::SystemTime::now()
                        .duration_since(time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                    getpid()
                );

                enable_tracing().ok();

                if let Err(err) = enter_listener_loop(listener) {
                    tracing::error!("Error: {err}")
                }
                tracing::info!(
                    "shutting down sidecar, pid: {}, total runtime: {:.3}s",
                    getpid(),
                    now.elapsed().as_secs_f64()
                )
            })
            .ok();
        })?;
        waitpid(Pid::from_raw(pid), None)?;
    };
    Ok(())
}

pub fn start_or_connect_to_sidecar() -> io::Result<TelemetryTransport> {
    let cfg = config::FromEnv::config();

    let liaison = match cfg.ipc_mode {
        config::IpcMode::Shared => setup::DefaultLiason::ipc_shared(),
        config::IpcMode::InstancePerProcess => setup::DefaultLiason::ipc_per_process(),
    };

    if let Some(listener) = liaison.attempt_listen()? {
        daemonize(listener)?;
    };

    Ok(IpcChannel::from(liaison.connect_to_server()?).into())
}

fn enable_tracing() -> anyhow::Result<()> {
    let subscriber = tracing_subscriber::fmt();

    match config::FromEnv::config().log_method {
        config::LogMethod::Stdout => subscriber.with_writer(io::stdout).init(),
        config::LogMethod::Stderr => subscriber.with_writer(io::stderr).init(),
        config::LogMethod::File(path) => {
            let log_file = fs::File::options().write(true).append(true).open(path)?;
            tracing_subscriber::fmt().with_writer(Mutex::new(log_file)).init()
        },
        config::LogMethod::Disabled => return Ok(()),
    };

    Ok(())
}
