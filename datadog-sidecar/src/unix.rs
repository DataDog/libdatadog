// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use spawn_worker::getpid;

use std::ffi::CString;
use std::os::unix::net::UnixListener as StdUnixListener;

use crate::config::Config;
use crate::enter_listener_loop;
use nix::fcntl::{fcntl, OFlag, F_GETFL, F_SETFL};
use nix::sys::socket::{shutdown, Shutdown};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::prelude::AsRawFd;
use std::time::Instant;
use tokio::net::{UnixListener, UnixStream};
use tokio::select;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{error, info};

#[cfg(target_os = "linux")]
use crate::config::LogMethod;
#[cfg(target_os = "linux")]
use libdd_crashtracker::{
    CrashtrackerConfiguration, CrashtrackerReceiverConfig, Metadata, StacktraceCollection,
};
#[cfg(target_os = "linux")]
use tracing::warn;

/// Run the sidecar daemon using a socket fd passed via the `__DD_INTERNAL_PASSED_FD`
/// environment variable (set by [`spawn_exec_binary`][spawn_worker::spawn_exec_binary]).
pub fn run_daemon_from_passed_fd() {
    #[cfg(feature = "tracing")]
    crate::log::enable_logging().ok();

    if let Err(err) = nix::unistd::setsid() {
        error!("Error calling setsid(): {err}")
    }

    #[cfg(target_os = "linux")]
    let _ = prctl::set_name("dd-ipc-helper");

    #[cfg(target_os = "linux")]
    if let Err(e) = init_crashtracker_standalone() {
        warn!("Failed to initialize crashtracker: {e}");
    }

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
                move || stop_listening(listener_fd)
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

fn stop_listening(listener_fd: RawFd) {
    // We need to drop O_NONBLOCK, as accept() on a shutdown socket will just give
    // EAGAIN instead of EINVAL
    #[allow(clippy::unwrap_used)]
    let flags = OFlag::from_bits_truncate(fcntl(listener_fd, F_GETFL).ok().unwrap());
    _ = fcntl(listener_fd, F_SETFL(flags & !OFlag::O_NONBLOCK));
    _ = shutdown(listener_fd, Shutdown::Both);
}

async fn accept_socket_loop(
    listener: UnixListener,
    handler: Box<dyn Fn(UnixStream)>,
) -> io::Result<()> {
    #[allow(clippy::unwrap_used)]
    let mut termsig = signal(SignalKind::terminate()).unwrap();
    loop {
        select! {
            _ = termsig.recv() => {
                stop_listening(listener.as_raw_fd());
                break;
            }
            accept = listener.accept() => {
                if let Ok((socket, _)) = accept {
                    handler(socket);
                } else {
                    break;
                }
            }
        }
    }
    Ok(())
}

pub fn primary_sidecar_identifier() -> u32 {
    unsafe { libc::geteuid() }
}

fn maybe_start_appsec() -> bool {
    let cfg = match &Config::get().appsec_config {
        Some(c) => c.clone(),
        None => return false,
    };

    info!("Starting appsec helper");

    // The AppSec helper is a separate shared library that must be dlopen'd into this
    // process before its entry point can be resolved via dlsym.
    #[allow(clippy::unwrap_used)]
    let lib_path = CString::new(cfg.shared_lib_path.as_encoded_bytes()).unwrap();
    let lib_handle = unsafe { libc::dlopen(lib_path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
    if lib_handle.is_null() {
        let reason = unsafe { libc::dlerror() };
        let reason_str = if reason.is_null() {
            "unknown error".to_owned()
        } else {
            unsafe { std::ffi::CStr::from_ptr(reason).to_string_lossy().into_owned() }
        };
        error!("Failed to load appsec helper library: {reason_str}");
        return false;
    }

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

/// Initialise the crashtracker for a *standalone binary* (i.e. `datadog-ipc-helper`).
///
/// On a crash, `/proc/<pid>/exe` is re-exec'd with the `crashtracker` subcommand.
/// The binary's `main()` dispatches to the receiver based on that argument.
#[cfg(target_os = "linux")]
pub fn init_crashtracker_standalone() -> anyhow::Result<()> {
    // The receiver is re-exec'd from /proc/<pid>/exe with argv:
    //   datadog-ipc-helper  crashtracker
    // main() dispatches to receiver_entry_point_stdin() based on argv[1].
    let receiver_args = vec![
        "datadog-ipc-helper".to_string(),
        "crashtracker".to_string(),
    ];

    let output = match &Config::get().log_method {
        LogMethod::Stdout => Some(format!("/proc/{}/fd/1", unsafe { libc::getpid() })),
        LogMethod::Stderr => Some(format!("/proc/{}/fd/2", unsafe { libc::getpid() })),
        LogMethod::File(file) => file.to_str().map(|s| s.to_string()),
        LogMethod::Disabled => None,
    };

    libdd_crashtracker::init(
        CrashtrackerConfiguration::new(
            vec![],
            true,
            true,
            Config::get().crashtracker_endpoint.clone(),
            StacktraceCollection::EnabledWithSymbolsInReceiver,
            vec![],
            None,
            None,
            true,
        )?,
        CrashtrackerReceiverConfig::new(
            receiver_args,
            vec![],
            format!("/proc/{}/exe", unsafe { libc::getpid() }),
            output,
            None,
        )?,
        Metadata::new(
            "libdatadog".to_string(),
            crate::sidecar_version!().to_string(),
            "SIDECAR".to_string(),
            vec![
                "is_crash:true".to_string(),
                "severity:crash".to_string(),
                format!("library_version:{}", crate::sidecar_version!()),
                "library:sidecar".to_string(),
            ],
        ),
    )
}

