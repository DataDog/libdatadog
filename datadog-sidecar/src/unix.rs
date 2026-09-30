// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use spawn_worker::{SpawnWorker, Stdio, TrampolineData, getpid};

use crate::config::Config;
use crate::enter_listener_loop;
use libdd_ipc::{SeqpacketConn, SeqpacketListener};
use nix::fcntl::{F_GETFL, F_SETFL, OFlag, fcntl};
use nix::sys::socket::{Shutdown, shutdown};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use tokio::select;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Notify;
use tracing::{error, info, warn};

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

#[unsafe(no_mangle)]
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
    if let Err(e) = init_crashtracker(if trampoline_data.argc > 0 {
        Some(trampoline_data.dependency_paths)
    } else {
        None
    }) {
        warn!("Failed to initialize crashtracker: {e}");
    }

    let buf_size = Config::get().pipe_buffer_size;
    if buf_size > 0 {
        libdd_ipc::platform::set_socket_buffer_size(buf_size);
    }

    let now = Instant::now();

    if let Some(fd) = spawn_worker::recv_passed_fd() {
        let seqpacket_listener = SeqpacketListener::from_owned_fd(fd);
        info!("Starting sidecar, pid: {}", getpid());
        let acquire_listener = move || {
            // Convert to async listener (also sets non-blocking mode).
            let async_listener = seqpacket_listener.into_async_listener()?;

            // shutdown to gracefully dequeue, and immediately relinquish ownership of the socket
            // while shutting down
            let shutdown = Arc::new(Notify::new());
            let cancel = {
                let listener_fd = async_listener.as_raw_fd();
                let shutdown = shutdown.clone();
                move || {
                    stop_listening(listener_fd);
                    // notify_one, not notify_waiters: it leaves a permit behind if the loop is
                    // between iterations, so the cancellation cannot be missed.
                    shutdown.notify_one();
                }
            };

            Ok((
                move |handler| accept_socket_loop(async_listener, handler, shutdown),
                cancel,
            ))
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
    shutdown: Arc<Notify>,
) -> io::Result<()> {
    #[allow(clippy::unwrap_used)]
    let mut termsig = signal(SignalKind::terminate()).unwrap();
    loop {
        select! {
            _ = termsig.recv() => {
                stop_listening(async_listener.as_raw_fd());
                break;
            }
            _ = shutdown.notified() => {
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
                            Ok(Err(e)) if e.raw_os_error() == Some(libc::EMSGSIZE) => {
                                warn!("IPC accept: oversized datagram discarded (EMSGSIZE)");
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

/// Thread-mode master PID, or 0 for the per-user subprocess sidecar.
static THREAD_SIDECAR_PID: AtomicU32 = AtomicU32::new(0);

/// Use the shared memory of the thread-mode sidecar in `master_pid`, or of the subprocess
/// sidecar for `None`.
pub fn use_thread_sidecar_shm_namespace(master_pid: Option<u32>) {
    THREAD_SIDECAR_PID.store(master_pid.unwrap_or(0), Ordering::Relaxed);
}

/// Qualify SHM names by effective UID for subprocess sidecars and master PID for thread mode.
/// Multiple masters for one UID must not replace each other's live segments.
pub fn shm_namespace() -> ShmNamespace {
    match THREAD_SIDECAR_PID.load(Ordering::Relaxed) {
        0 => ShmNamespace::User(primary_sidecar_identifier()),
        pid => ShmNamespace::Thread(pid),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShmNamespace {
    User(u32),
    Thread(u32),
}

impl std::fmt::Display for ShmNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShmNamespace::User(uid) => write!(f, "{uid}"),
            ShmNamespace::Thread(pid) => write!(f, "t{pid}"),
        }
    }
}

/// Allow initializing crashtracker independently for thread-mode sidecar.
#[cfg(target_os = "linux")]
pub fn build_crashtracker_receiver_config(
    dependency_paths: Option<*const *const libc::c_char>,
    output: Option<String>,
) -> anyhow::Result<CrashtrackerReceiverConfig> {
    let entrypoint = entrypoint!(ddog_crashtracker_entry_point);
    let entrypoint_path = match unsafe { get_dl_path_raw(entrypoint.ptr as *const libc::c_void) } {
        (Some(path), _) => path,
        _ => anyhow::bail!("Failed to find crashtracker entrypoint"),
    };
    let entrypoint_path_str = entrypoint_path.into_string()?;

    let mut receiver_args = vec!["crashtracker_receiver".to_string()];
    let mut receiver_env = vec![];
    let entrypoint_name = entrypoint.symbol_name.into_string()?;

    if let Some(dependency_paths) = dependency_paths {
        receiver_args.push("".to_string());
        receiver_args.push(entrypoint_path_str.clone());
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
        receiver_args.push(entrypoint_name);
    } else {
        // direct mode: ld.so uses argv[1] as the library to exec
        receiver_args.push(entrypoint_path_str.clone());
        receiver_env.push(("_DD_SIDECAR_DIRECT_EXEC".to_string(), entrypoint_name));
        if let Ok(env) = std::env::var("_DD_SIDECAR_PATH_DEPS") {
            if !env.is_empty() {
                receiver_env.push(("_DD_SIDECAR_PATH_DEPS".to_string(), env));
            }
        }
    }

    CrashtrackerReceiverConfig::new(
        receiver_args,
        receiver_env,
        format!("/proc/{}/exe", unsafe { libc::getpid() }),
        output,
        None,
    )
}

#[cfg(target_os = "linux")]
fn init_crashtracker(dependency_paths: Option<*const *const libc::c_char>) -> anyhow::Result<()> {
    let output = match &Config::get().log_method {
        LogMethod::Stdout => Some(format!("/proc/{}/fd/1", unsafe { libc::getpid() })),
        LogMethod::Stderr => Some(format!("/proc/{}/fd/2", unsafe { libc::getpid() })),
        LogMethod::File(file) => file.to_str().map(|s| s.to_string()),
        LogMethod::Disabled => None,
    };
    let receiver_config = build_crashtracker_receiver_config(dependency_paths, output)?;

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
    let tags = vec![
        "is_crash:true".to_string(),
        "severity:crash".to_string(),
        format!("library_version:{}", crate::sidecar_version!()),
        "library:sidecar".to_string(),
        "language:php".to_string(),
    ];

    libdd_crashtracker::init(
        config_builder.build()?,
        receiver_config,
        Metadata::new(
            "libdatadog".to_string(),
            crate::sidecar_version!().to_string(),
            "SIDECAR".to_string(),
            tags,
        ),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_crashtracker_entry_point(_trampoline_data: &TrampolineData) {
    unsafe {
        if let Err(e) = libdd_crashtracker::receiver_entry_point_stdin() {
            eprintln!("{e}");
            libc::exit(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Isolate the process-wide namespace change from other tests.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn thread_mode_sidecars_get_names_of_their_own() {
        // SAFETY: the child only formats names and leaves through _exit.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                let own = crate::tracer::shm_limiter_path();
                use_thread_sidecar_shm_namespace(Some(4242));
                let first = crate::tracer::shm_limiter_path();
                use_thread_sidecar_shm_namespace(Some(4243));
                let second = crate::tracer::shm_limiter_path();
                use_thread_sidecar_shm_namespace(None);
                let back = crate::tracer::shm_limiter_path();
                let ok = first != second && first != own && second != own && back == own;
                unsafe { libc::_exit(if ok { 0 } else { 1 }) }
            }
            pid => {
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                assert_eq!(libc::WEXITSTATUS(status), 0, "sidecars would share names");
            }
        }
    }
}
