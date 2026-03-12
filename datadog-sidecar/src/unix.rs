// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use spawn_worker::{getpid, SpawnWorker, Stdio, TrampolineData};


use crate::config::Config;
use crate::enter_listener_loop;
use datadog_ipc::{SeqpacketConn, SeqpacketListener};
use nix::fcntl::{fcntl, OFlag, F_GETFL, F_SETFL};
use nix::sys::socket::{shutdown, Shutdown};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::time::Instant;
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
use spawn_worker::{entrypoint, get_dl_path_raw};
#[cfg(target_os = "linux")]
use std::ffi::CStr;
#[cfg(target_os = "linux")]
use tracing::warn;

#[no_mangle]
#[allow(unused)]
pub extern "C" fn ddog_daemon_entry_point(trampoline_data: &TrampolineData) {
    #[cfg(feature = "tracing")]
    crate::log::enable_logging().ok();

    if let Err(err) = nix::unistd::setsid() {
        error!("Error calling setsid(): {err}")
    }

    #[cfg(target_os = "linux")]
    let _ = prctl::set_name("dd-ipc-helper");

    #[cfg(target_os = "linux")]
    if let Err(e) = init_crashtracker(trampoline_data.dependency_paths) {
        warn!("Failed to initialize crashtracker: {e}");
    }

    let buf_size = Config::get().pipe_buffer_size;
    if buf_size > 0 {
        datadog_ipc::platform::set_socket_buffer_size(buf_size);
    }

    let now = Instant::now();

    let appsec_started = Config::get()
        .appsec_config
        .as_ref()
        .map(crate::appsec::maybe_start)
        .unwrap_or(false);

    if let Some(fd) = spawn_worker::recv_passed_fd() {
        let seqpacket_listener = SeqpacketListener::from_owned_fd(fd);
        info!("Starting sidecar, pid: {}", getpid());
        let acquire_listener = move || {
            // Convert to async listener (also sets non-blocking mode).
            let async_listener = seqpacket_listener.into_async_listener()?;

            // shutdown to gracefully dequeue, and immediately relinquish ownership of the socket
            // while shutting down
            let cancel = {
                let listener_fd = async_listener.as_raw_fd();
                move || stop_listening(listener_fd)
            };

            Ok((
                move |handler| accept_socket_loop(async_listener, handler),
                cancel,
            ))
        };
        if let Err(err) = enter_listener_loop(acquire_listener) {
            error!("Error: {err}")
        }
    }

    if appsec_started {
        crate::appsec::shutdown();
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
    async_listener: tokio::io::unix::AsyncFd<SeqpacketListener>,
    handler: Box<dyn Fn(SeqpacketConn)>,
) -> io::Result<()> {
    #[allow(clippy::unwrap_used)]
    let mut termsig = signal(SignalKind::terminate()).unwrap();
    loop {
        select! {
            _ = termsig.recv() => {
                stop_listening(async_listener.as_raw_fd());
                break;
            }
            ready = async_listener.readable() => {
                match ready {
                    Ok(mut guard) => {
                        match guard.try_io(|inner| inner.get_ref().try_accept()) {
                            Ok(Ok(conn)) => {
                                let buf_size = Config::get().pipe_buffer_size;
                                if buf_size > 0 {
                                    let _ = conn.set_rcvbuf_size(buf_size);
                                }
                                handler(conn);
                            }
                            Ok(Err(e)) => {
                                error!("IPC accept error: {e}");
                                break;
                            }
                            Err(_would_block) => continue,
                        }
                    }
                    Err(e) => {
                        error!("IPC listener error: {e}");
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn setup_daemon_process(
    listener: SeqpacketListener,
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

/// No-op: retained for FFI compatibility.
/// The master PID is now tracked by MasterListener::start() directly.
pub fn set_sidecar_master_pid(_pid: u32) {}

#[cfg(target_os = "linux")]
fn init_crashtracker(dependency_paths: *const *const libc::c_char) -> anyhow::Result<()> {
    let entrypoint = entrypoint!(ddog_crashtracker_entry_point);
    let entrypoint_path = match unsafe { get_dl_path_raw(entrypoint.ptr as *const libc::c_void) } {
        (Some(path), _) => path,
        _ => anyhow::bail!("Failed to find crashtracker entrypoint"),
    };

    let mut receiver_args = vec![
        "crashtracker_receiver".to_string(),
        "".to_string(),
        entrypoint_path.into_string()?,
    ];

    unsafe {
        let mut descriptors = dependency_paths;
        if !descriptors.is_null() {
            loop {
                if (*descriptors).is_null() {
                    break;
                }
                receiver_args.push(CStr::from_ptr(*descriptors).to_string_lossy().into_owned());
                descriptors = descriptors.add(1);
            }
        }
    }
    receiver_args.push(entrypoint.symbol_name.into_string()?);

    let output = match &Config::get().log_method {
        LogMethod::Stdout => Some(format!("/proc/{}/fd/1", unsafe { libc::getpid() })),
        LogMethod::Stderr => Some(format!("/proc/{}/fd/2", unsafe { libc::getpid() })),
        LogMethod::File(file) => file.to_str().map(|s| s.to_string()),
        LogMethod::Disabled => None,
    };

    let mut config_builder = CrashtrackerConfiguration::builder()
        .create_alt_stack(true)
        .use_alt_stack(true)
        .resolve_frames(StacktraceCollection::EnabledWithSymbolsInReceiver)
        .demangle_names(true);
    if let Some(ep) = Config::get().crashtracker_endpoint.as_ref() {
        config_builder = config_builder.endpoint_url(&ep.url.to_string());
        if let Some(api_key) = ep.api_key.as_deref() {
            config_builder = config_builder.endpoint_api_key(api_key);
        }
        config_builder = config_builder
            .endpoint_timeout_ms(ep.timeout_ms)
            .endpoint_use_system_resolver(ep.use_system_resolver);
        if let Some(test_token) = ep.test_token.as_deref() {
            config_builder = config_builder.endpoint_test_token(test_token);
        }
    }
    libdd_crashtracker::init(
        config_builder.build()?,
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

#[no_mangle]
pub extern "C" fn ddog_crashtracker_entry_point(_trampoline_data: &TrampolineData) {
    unsafe {
        if let Err(e) = libdd_crashtracker::receiver_entry_point_stdin() {
            eprintln!("{e}");
            libc::exit(1)
        }
    }
}
