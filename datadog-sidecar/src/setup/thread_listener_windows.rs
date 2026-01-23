// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use tokio::sync::oneshot;
use tracing::{error, info};

use crate::config::Config;
use crate::entry::MainLoopConfig;
use crate::service::blocking::SidecarTransport;
use datadog_ipc::platform::metadata::ProcessHandle;
use datadog_ipc::platform::Channel;
use datadog_ipc::transport::blocking::BlockingTransport;

static MASTER_LISTENER: OnceLock<Mutex<Option<MasterListener>>> = OnceLock::new();

pub struct MasterListener {
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread_handle: Option<JoinHandle<()>>,
    pid: i32,
}

impl MasterListener {
    /// Start the master listener thread using Windows Named Pipes.
    ///
    /// This spawns a new OS thread that creates a named pipe server
    /// to listen for worker connections. Only one listener can be active per process.
    pub fn start(pid: i32, _config: Config) -> io::Result<()> {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex
            .lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if listener_guard.is_some() {
            return Err(io::Error::other("Master listener is already running"));
        }

        let pipe_name = format!(r"\\.\pipe\ddtrace_sidecar_{}", pid);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let thread_handle = thread::Builder::new()
            .name(format!("ddtrace-sidecar-listener-{}", pid))
            .spawn(move || {
                if let Err(e) = run_listener_windows(pipe_name, shutdown_rx) {
                    error!("Listener thread error: {}", e);
                }
            })
            .map_err(|e| io::Error::other(format!("Failed to spawn listener thread: {}", e)))?;

        *listener_guard = Some(MasterListener {
            shutdown_tx: Some(shutdown_tx),
            thread_handle: Some(thread_handle),
            pid,
        });

        info!("Started Windows named pipe listener (PID {})", pid);
        Ok(())
    }

    /// Shutdown the master listener thread.
    ///
    /// Sends shutdown signal and joins the listener thread. This is blocking
    /// and will wait for the thread to exit cleanly.
    pub fn shutdown() -> io::Result<()> {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex
            .lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if let Some(mut master) = listener_guard.take() {
            // Signal shutdown by sending to the oneshot sender
            if let Some(tx) = master.shutdown_tx.take() {
                let _ = tx.send(());
            }

            if let Some(handle) = master.thread_handle.take() {
                handle
                    .join()
                    .map_err(|_| io::Error::other("Failed to join listener thread"))?;
            }

            info!("Master listener thread shut down successfully");
            Ok(())
        } else {
            Err(io::Error::other("No master listener is running"))
        }
    }

    /// Check if the master listener is active for the given PID.
    pub fn is_active(pid: i32) -> bool {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        if let Ok(listener_guard) = listener_mutex.lock() {
            listener_guard.as_ref().is_some_and(|l| l.pid == pid)
        } else {
            false
        }
    }

    /// Clear inherited listener state.
    /// Kept for API compatibility with Unix version.
    pub fn clear_inherited_state() -> io::Result<()> {
        Ok(())
    }
}

/// Accept connections in a loop for Windows named pipes.
async fn accept_pipe_loop_windows(
    pipe_name: String,
    handler: Box<dyn Fn(tokio::net::windows::named_pipe::NamedPipeServer)>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> io::Result<()> {
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .max_instances(254) // Windows allows up to 255 instances
        .create(&pipe_name)?;

    info!("Named pipe server created at: {}", pipe_name);

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                info!("Shutdown signal received in Windows pipe listener");
                break;
            }
            result = server.connect() => {
                match result {
                    Ok(_) => {
                        info!("Accepted new worker connection on named pipe");
                        handler(server);

                        server = ServerOptions::new()
                            .create(&pipe_name)?;
                    }
                    Err(e) => {
                        error!("Failed to accept worker connection: {}", e);
                        match ServerOptions::new().create(&pipe_name) {
                            Ok(new_server) => server = new_server,
                            Err(e2) => {
                                error!("Failed to recover named pipe: {}", e2);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Entry point for Windows named pipe listener
fn run_listener_windows(pipe_name: String, shutdown_rx: oneshot::Receiver<()>) -> io::Result<()> {
    info!("Listener thread running, creating Windows named pipe server");

    let acquire_listener = move || {
        let cancel = || {};
        let pipe_name_clone = pipe_name.clone();
        Ok((
            move |handler| accept_pipe_loop_windows(pipe_name_clone, handler, shutdown_rx),
            cancel,
        ))
    };

    let loop_config = MainLoopConfig {
        enable_ctrl_c_handler: false,
        enable_crashtracker: false,
        external_shutdown_rx: None,
    };

    crate::entry::enter_listener_loop_with_config(acquire_listener, loop_config)
        .map_err(|e| io::Error::other(format!("Windows thread listener failed: {}", e)))?;

    info!("Listener thread exiting");
    Ok(())
}

/// Connect to the master listener as a worker using Windows Named Pipes.
///
/// Establishes a connection to the master listener thread for the given PID.
pub fn connect_to_master(pid: i32) -> io::Result<Box<SidecarTransport>> {
    info!("Connecting to master listener via named pipe (PID {})", pid);

    let pipe_name = format!(r"\\.\pipe\ddtrace_sidecar_{}", pid);

    let client = ClientOptions::new().open(&pipe_name)?;

    info!("Connected to named pipe: {}", pipe_name);

    let raw_handle = client.as_raw_handle();
    let owned_handle = unsafe { OwnedHandle::from_raw_handle(raw_handle) };

    std::mem::forget(client);

    let process_handle =
        ProcessHandle::Getter(Box::new(move || Ok(ProcessHandle::Pid(pid as u32))));
    let channel = Channel::from_client_handle_and_pid(owned_handle, process_handle);

    let transport = BlockingTransport::from(channel);

    let sidecar_transport = Box::new(SidecarTransport {
        inner: Mutex::new(transport),
        reconnect_fn: None,
    });

    info!("Successfully connected to master listener");
    Ok(sidecar_transport)
}
