// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use spawn_worker::{getpid, SpawnWorker, Stdio};

use std::os::unix::net::UnixListener as StdUnixListener;

use crate::config::Config;
use crate::enter_listener_loop;
use nix::fcntl::{fcntl, OFlag, F_GETFL, F_SETFL};
use nix::sys::socket::{shutdown, Shutdown};
use std::io;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::time::Instant;
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info};

#[no_mangle]
pub extern "C" fn ddog_daemon_entry_point() {
    #[cfg(feature = "tracing")]
    crate::log::enable_logging().ok();

    if let Err(err) = nix::unistd::setsid() {
        error!("Error calling setsid(): {err}")
    }

    #[cfg(target_os = "linux")]
    let _ = prctl::set_name("dd-ipc-helper");

    let now = Instant::now();

    if let Some(fd) = spawn_worker::recv_passed_fd() {
        let listener: StdUnixListener = fd.into();
        info!("Starting sidecar, pid: {}", getpid());
        let acquire_listener = move || {
            listener.set_nonblocking(true)?;
            let listener = UnixListener::from_std(listener)?;

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
            error!("Error: {err}")
        }
    }

    info!(
        "shutting down sidecar, pid: {}, total runtime: {:.3}s",
        getpid(),
        now.elapsed().as_secs_f64()
    )
}

async fn accept_socket_loop(
    listener: UnixListener,
    handler: Box<dyn Fn(UnixStream)>,
) -> io::Result<()> {
    while let Ok((socket, _)) = listener.accept().await {
        handler(socket);
    }
    Ok(())
}

pub fn setup_daemon_process(
    listener: StdUnixListener,
    spawn_cfg: &mut SpawnWorker,
) -> io::Result<()> {
    spawn_cfg
        .daemonize(true)
        .process_name("datadog-ipc-helper")
        .pass_fd(unsafe { OwnedFd::from_raw_fd(listener.into_raw_fd()) })
        .stdin(Stdio::Null);

    Ok(())
}
