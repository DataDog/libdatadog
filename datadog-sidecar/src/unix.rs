// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use spawn_worker::{getpid, SpawnWorker, Stdio, TrampolineData};

use crate::service::blocking::SidecarTransport;
use crate::setup::{DefaultLiaison, Liaison};
use std::ffi::CString;
use std::os::unix::net::UnixListener as StdUnixListener;

use crate::config::Config;
use crate::enter_listener_loop;
use nix::fcntl::{fcntl, OFlag, F_GETFL, F_SETFL};
use nix::sys::socket::{shutdown, Shutdown};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tokio::net::{UnixListener, UnixStream};
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

static MASTER_LISTENER: Mutex<Option<(thread::JoinHandle<()>, RawFd)>> = Mutex::new(None);

pub fn start_master_listener_unix(master_pid: i32) -> io::Result<()> {
    let liaison = DefaultLiaison::for_master_pid(master_pid as u32);

    let std_listener = match liaison.attempt_listen()? {
        Some(l) => l,
        None => {
            return Ok(());
        }
    };

    let listener_fd = std_listener.as_raw_fd();

    let handle = thread::Builder::new()
        .name("dd-sidecar".into())
        .spawn(move || {
            // Use blocking I/O - no shared tokio Runtime needed
            // This makes the code fork-safe
            use crate::service::sidecar_server::SidecarServer;
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("Failed to create runtime for server initialization: {}", e);
                    return;
                }
            };

            let server = runtime.block_on(async { SidecarServer::default() });

            // Shutdown flag to signal connection threads to stop
            let shutdown_flag = Arc::new(AtomicBool::new(false));

            // Track connection threads and stream fds for forceful shutdown
            let mut handler_threads: Vec<thread::JoinHandle<()>> = Vec::new();
            let active_fds: Arc<Mutex<Vec<RawFd>>> = Arc::new(Mutex::new(Vec::new()));

            loop {
                // Clean up finished threads to avoid accumulating handles
                handler_threads.retain(|h| !h.is_finished());

                match std_listener.accept() {
                    Ok((stream, _addr)) => {
                        // Store the raw fd so we can shutdown the connection later
                        let stream_fd = stream.as_raw_fd();
                        if let Ok(mut fds) = active_fds.lock() {
                            fds.push(stream_fd);
                        }

                        let server = server.clone();
                        let shutdown = shutdown_flag.clone();
                        let fds_cleanup = active_fds.clone();

                        // Spawn a thread for each connection
                        match thread::Builder::new().name("dd-conn-handler".into()).spawn(
                            move || {
                                // Create a minimal single-threaded runtime for this connection only
                                // This runtime will be dropped when the connection closes
                                let runtime = match tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                {
                                    Ok(rt) => rt,
                                    Err(e) => {
                                        error!("Failed to create runtime for connection: {}", e);
                                        return;
                                    }
                                };

                                runtime.block_on(async move {
                                    // Check shutdown flag
                                    if shutdown.load(Ordering::Relaxed) {
                                        return;
                                    }

                                    // Convert std UnixStream to tokio UnixStream
                                    if let Err(e) = stream.set_nonblocking(true) {
                                        error!("Failed to set nonblocking: {}", e);
                                        return;
                                    }

                                    let tokio_stream = match UnixStream::from_std(stream) {
                                        Ok(s) => s,
                                        Err(e) => {
                                            error!("Failed to convert stream: {}", e);
                                            return;
                                        }
                                    };

                                    // Handle the connection using existing async infrastructure
                                    use datadog_ipc::platform::AsyncChannel;

                                    // Use the cloned shared server
                                    server
                                        .accept_connection(AsyncChannel::from(tokio_stream))
                                        .await;

                                    // Remove this fd from active list when done
                                    if let Ok(mut fds) = fds_cleanup.lock() {
                                        fds.retain(|&fd| fd != stream_fd);
                                    }
                                });
                            },
                        ) {
                            Ok(handle) => handler_threads.push(handle),
                            Err(e) => error!("Failed to spawn handler thread: {}", e),
                        }
                    }
                    Err(e) => {
                        match e.kind() {
                            io::ErrorKind::Interrupted => continue,
                            io::ErrorKind::InvalidInput => break, // Socket shut down
                            _ => {
                                error!("Accept error: {}", e);
                                thread::sleep(Duration::from_millis(100));
                            }
                        }
                    }
                }
            }

            info!("Master listener stopped accepting connections");

            // Signal all connection threads to stop
            shutdown_flag.store(true, Ordering::Relaxed);

            // Forcefully shutdown all active connection streams
            // This will cause accept_connection().await to complete immediately
            if let Ok(fds) = active_fds.lock() {
                info!("Forcefully closing {} active connections", fds.len());
                for &fd in fds.iter() {
                    // Shutdown both directions to force connection close
                    let _ = shutdown(fd, Shutdown::Both);
                }
            }

            // Shutdown the server
            server.shutdown();

            // Now join all connection threads - they should exit immediately
            // because all connections were forcefully closed
            info!("Waiting for {} connection threads to finish", handler_threads.len());
            for (i, handle) in handler_threads.into_iter().enumerate() {
                if let Err(e) = handle.join() {
                    error!("Connection thread {} panicked: {:?}", i, e);
                }
            }
            info!("All connection threads finished");
        })
        .map_err(io::Error::other)?;

    match MASTER_LISTENER.lock() {
        Ok(mut guard) => *guard = Some((handle, listener_fd)),
        Err(e) => {
            error!("Failed to acquire lock for storing master listener: {}", e);
            return Err(io::Error::other("Mutex poisoned"));
        }
    }

    Ok(())
}

pub fn connect_worker_unix(master_pid: i32) -> io::Result<SidecarTransport> {
    let liaison = DefaultLiaison::for_master_pid(master_pid as u32);

    let mut last_error = None;
    for _ in 0..10 {
        match liaison.connect_to_server() {
            Ok(channel) => {
                return Ok(channel.into());
            }
            Err(e) => {
                last_error = Some(e);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    error!("Worker failed to connect after 10 attempts");
    Err(last_error.unwrap_or_else(|| io::Error::other("Connection failed")))
}

pub fn shutdown_master_listener_unix() -> io::Result<()> {
    let listener_data = match MASTER_LISTENER.lock() {
        Ok(mut guard) => guard.take(),
        Err(e) => {
            error!(
                "Failed to acquire lock for shutting down master listener: {}",
                e
            );
            return Err(io::Error::other("Mutex poisoned"));
        }
    };

    if let Some((handle, fd)) = listener_data {
        stop_listening(fd);
        let _ = handle.join();
    }

    Ok(())
}

/// Clears inherited resources in child processes after fork().
/// With the new blocking I/O approach, we only need to forget the listener thread handle.
/// Each connection creates its own short-lived runtime, so there's no global runtime to inherit.
pub fn clear_inherited_listener_unix() -> io::Result<()> {
    info!("Child process clearing inherited listener state");
    match MASTER_LISTENER.lock() {
        Ok(mut guard) => {
            if let Some((handle, _fd)) = guard.take() {
                info!("Child forgetting inherited listener thread handle");
                // Forget the handle without joining - parent owns the thread
                std::mem::forget(handle);
                info!("Child successfully forgot listener handle");
            } else {
                info!("Child found no listener to clear");
            }
        }
        Err(e) => {
            error!(
                "Failed to acquire lock for clearing inherited listener: {}",
                e
            );
            return Err(io::Error::other("Mutex poisoned"));
        }
    }

    Ok(())
}

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
    if let Ok(flags_raw) = fcntl(listener_fd, F_GETFL) {
        let flags = OFlag::from_bits_truncate(flags_raw);
        _ = fcntl(listener_fd, F_SETFL(flags & !OFlag::O_NONBLOCK));
        _ = shutdown(listener_fd, Shutdown::Both);
    }
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
    let cfg = &Config::get().appsec_config;
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

#[no_mangle]
pub extern "C" fn ddog_crashtracker_entry_point(_trampoline_data: &TrampolineData) {
    unsafe {
        if let Err(e) = libdd_crashtracker::receiver_entry_point_stdin() {
            eprintln!("{e}");
            libc::exit(1)
        }
    }
}
