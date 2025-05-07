// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use spawn_worker::{getpid, SpawnWorker, Stdio};

use std::ffi::CString;
use std::os::unix::net::UnixListener as StdUnixListener;

use crate::config::FromEnv;
use crate::enter_listener_loop;
use nix::fcntl::{fcntl, OFlag, F_GETFL, F_SETFL};
use nix::sys::socket::{shutdown, Shutdown};
use std::io;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::time::Instant;
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info};

/// cbindgen:ignore
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

    let appsec_started = maybe_start_appsec();

    if let Some(fd) = spawn_worker::recv_passed_fd() {
        let listener: StdUnixListener = fd.into();
        info!("Starting sidecar, pid: {}", getpid());
        let acquire_listener = move || {
            listener.set_nonblocking(true)?;
            let listener = UnixListener::from_std(listener)?;

            // shutdown to gracefully dequeue, and immediately relinquish ownership of the socket
            // while shutting down
            let cancel = {
                let listener_fd = listener.as_raw_fd();
                move || {
                    // We need to drop O_NONBLOCK, as accept() on a shutdown socket will just give
                    // EAGAIN instead of EINVAL
                    #[allow(clippy::unwrap_used)]
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

    if appsec_started {
        shutdown_appsec();
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

pub fn primary_sidecar_identifier() -> u32 {
    unsafe { libc::geteuid() }
}

fn maybe_start_appsec() -> bool {
    let cfg = FromEnv::appsec_config();
    if cfg.is_none() {
        return false;
    }

    info!("Starting appsec helper");
    #[allow(clippy::unwrap_used)]
    let entrypoint_sym_name = CString::new("appsec_helper_main").unwrap();

    let func_ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, entrypoint_sym_name.as_ptr()) };
    if func_ptr.is_null() {
        error!("Failed to load appsec helper: can't find the symbol 'appsec_helper_main'");
        return false;
    }

    let appsec_entry_fn: extern "C" fn() -> i32 = unsafe { std::mem::transmute(func_ptr) };
    let res = appsec_entry_fn();
    if res != 0 {
        error!("Appsec helper failed to start");
        return false;
    }

    info!("Appsec helper started");
    true
}

fn shutdown_appsec() -> bool {
    info!("Shutting down appsec helper");

    #[allow(clippy::unwrap_used)]
    let shutdown_sym_name = CString::new("appsec_helper_shutdown").unwrap();

    let func_ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, shutdown_sym_name.as_ptr()) };
    if func_ptr.is_null() {
        error!("Failed to load appsec helper: can't find the symbol 'appsec_helper_shutdown'");
        return false;
    }
    let appsec_shutdown_fn: extern "C" fn() -> i32 = unsafe { std::mem::transmute(func_ptr) };
    let res = appsec_shutdown_fn();
    if res != 0 {
        error!("Appsec helper failed to shutdown");
        return false;
    }

    info!("Appsec helper shutdown");
    true
}
