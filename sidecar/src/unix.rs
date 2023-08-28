// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use spawn_worker::{entrypoint, getpid, Stdio};

use std::fs::File;
use std::os::unix::net::UnixListener as StdUnixListener;

use futures::future;
use manual_future::ManualFuture;
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
use tokio::{select, spawn};

use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, Receiver};
use tokio::task::JoinHandle;

use crate::interface::blocking::SidecarTransport;
use crate::interface::SidecarServer;
use datadog_ipc::platform::Channel as IpcChannel;
use ddtelemetry::data::metrics::{MetricNamespace, MetricType};
use ddtelemetry::metrics::ContextKey;
use ddtelemetry::worker::{
    LifecycleAction, TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerHandle,
};

use crate::setup::{self, Liaison};

use crate::config::{self, Config};

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
        let acquire_listener = move || {
            let listener = UnixListener::from_std(listener)?;
            listener.set_nonblocking(true)?;

            // shutdown to gracefully dequeue, and immediately relinquish ownership of the socket while shutting down
            let cancel = {
                let listener_fd = listener.as_raw_fd();
                move || {
                    // We need to drop O_NONBLOCK, as accept() on a shutdown socket will just give EAGAIN instead of EINVAL
                    let flags =
                        OFlag::from_bits_truncate(fcntl(listener_fd, F_GETFL).ok().unwrap());
                    _ = fcntl(listener_fd, F_SETFL(flags & !OFlag::O_NONBLOCK));
                    _ = shutdown(listener_fd, Shutdown::Both);
                }
            };

            Ok((|handler| accept_socket_loop(listener, handler), cancel))
        };
        if let Err(err) = enter_listener_loop(acquire_listener) {
            tracing::error!("Error: {err}")
        }
    }

    tracing::info!(
        "shutting down sidecar, pid: {}, total runtime: {:.3}s",
        getpid(),
        now.elapsed().as_secs_f64()
    )
}

async fn accept_socket_loop<F>(
    server: UnixListener,
    handler: Box<dyn Fn(UnixStream)>,
) -> io::Result<()> {
    while let Ok((socket, _)) = listener.accept().await {
        handler(socket);
    }
    Ok(())
}

pub fn setup_daemon_process(
    listener: &UnixListener,
    cfg: Config,
    spawn_cfg: &mut SpawnWorker,
) -> io::Result<()> {
    spawn_cfg
        .shared_lib_dependencies(cfg.library_dependencies.clone())
        .daemonize(true)
        .pass_fd(listener)
        .stdin(Stdio::Null);

    match cfg.log_method {
        config::LogMethod::File(path) => {
            let file = File::options()
                .write(true)
                .append(true)
                .truncate(false)
                .create(true)
                .open(path)?;
            let (out, err) = (Stdio::Fd(file.try_clone()?.into(), Stdio::Fd(file.into())));
            spawn_cfg.stdout(out);
            spawn_cfg.stderr(err);
        }
        config::LogMethod::Disabled => {
            spawn_cfg.stdout(Stdio::Null);
            spawn_cfg.stderr(Stdio::Null);
        }
        _ => {}
    }

    Ok(())
}
