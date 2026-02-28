// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io;
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tracing::{error, info};

use crate::config::Config;
use crate::config::IpcMode::{InstancePerProcess, Shared};
use crate::entry::MainLoopConfig;
use crate::service::blocking::SidecarTransport;
use crate::setup::{Liaison, SharedDirLiaison};
use datadog_ipc::transport::blocking::BlockingTransport;

static MASTER_LISTENER: OnceLock<Mutex<Option<MasterListener>>> = OnceLock::new();

pub struct MasterListener {
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread_handle: Option<JoinHandle<()>>,
    pid: i32,
}

impl MasterListener {
    /// Start the master listener thread.
    ///
    /// This spawns a new OS thread that calls enter_listener_loop_with_config
    /// to create a Tokio runtime and listen for worker connections.
    /// Only one listener can be active per process.
    pub fn start(pid: i32, config: Config) -> io::Result<()> {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex
            .lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if listener_guard.is_some() {
            return Err(io::Error::other("Master listener is already running"));
        }

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let thread_handle = thread::Builder::new()
            .name(format!("ddtrace-sidecar-listener-{}", pid))
            .spawn(move || {
                if let Err(e) = run_listener(config, shutdown_rx) {
                    error!("Listener thread error: {}", e);
                }
            })
            .map_err(|e| io::Error::other(format!("Failed to spawn listener thread: {}", e)))?;

        *listener_guard = Some(MasterListener {
            shutdown_tx: Some(shutdown_tx),
            thread_handle: Some(thread_handle),
            pid,
        });

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
            // Signal shutdown by sending to or dropping the oneshot sender
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
    ///
    /// Used for fork detection: child processes inherit the listener state
    /// but don't own the actual thread.
    pub fn is_active(pid: i32) -> bool {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        if let Ok(listener_guard) = listener_mutex.lock() {
            listener_guard.as_ref().is_some_and(|l| l.pid == pid)
        } else {
            false
        }
    }

    /// Clear inherited listener state after fork.
    ///
    /// Child processes must call this to prevent attempting to use the
    /// parent's listener thread, which doesn't exist in the child.
    pub fn clear_inherited_state() -> io::Result<()> {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex
            .lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if listener_guard.is_some() {
            info!("Clearing inherited master listener state in child process");
            *listener_guard = None;
        }

        Ok(())
    }
}

/// Accept connections in a loop for thread mode.
async fn accept_socket_loop_thread(
    listener: UnixListener,
    handler: Box<dyn Fn(tokio::net::UnixStream)>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> io::Result<()> {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                info!("Shutdown signal received in thread listener");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((socket, _)) => {
                        info!("Accepted new worker connection");
                        handler(socket);
                    }
                    Err(e) => {
                        error!("Failed to accept worker connection: {}", e);
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Entry point for thread listener - calls enter_listener_loop_with_config
fn run_listener(config: Config, shutdown_rx: oneshot::Receiver<()>) -> io::Result<()> {
    info!("Listener thread running, creating IPC server");

    let acquire_listener = move || {
        let liaison: SharedDirLiaison = match config.ipc_mode {
            Shared => Liaison::ipc_shared(),
            InstancePerProcess => Liaison::ipc_per_process(),
        };

        let std_listener = liaison
            .attempt_listen()?
            .ok_or_else(|| io::Error::other("Failed to create IPC listener"))?;

        std_listener.set_nonblocking(true)?;
        let listener = UnixListener::from_std(std_listener)?;

        info!("IPC server listening for worker connections");

        let cancel = || {};
        Ok((
            move |handler| accept_socket_loop_thread(listener, handler, shutdown_rx),
            cancel,
        ))
    };

    let loop_config = MainLoopConfig {
        enable_ctrl_c_handler: false,
        enable_crashtracker: false,
        external_shutdown_rx: None,
    };

    crate::entry::enter_listener_loop_with_config(acquire_listener, loop_config)
        .map_err(|e| io::Error::other(format!("Thread listener failed: {}", e)))?;

    info!("Listener thread exiting");
    Ok(())
}

/// Connect to the master listener as a worker.
///
/// Establishes a connection to the master listener thread for the given PID.
pub fn connect_to_master(pid: i32) -> io::Result<Box<SidecarTransport>> {
    info!("Connecting to master listener (PID {})", pid);

    let config = Config::get();

    let liaison: SharedDirLiaison = match config.ipc_mode {
        Shared => Liaison::ipc_shared(),
        InstancePerProcess => Liaison::ipc_per_process(),
    };

    let channel = liaison
        .connect_to_server()
        .map_err(|e| io::Error::other(format!("Failed to connect to master listener: {}", e)))?;

    let transport = BlockingTransport::from(channel);

    let sidecar_transport = Box::new(SidecarTransport {
        inner: Mutex::new(transport),
        reconnect_fn: None,
    });

    info!("Successfully connected to master listener");
    Ok(sidecar_transport)
}
