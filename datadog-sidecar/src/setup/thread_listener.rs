// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::io;
use tokio::net::UnixListener;
use tokio::runtime::Runtime;
use tracing::{info, error};

use crate::config::Config;
use crate::config::IpcMode::{InstancePerProcess, Shared};
use crate::service::blocking::SidecarTransport;
use crate::service::SidecarServer;
use crate::setup::{Liaison, SharedDirLiaison};
use datadog_ipc::platform::AsyncChannel;
use datadog_ipc::transport::blocking::BlockingTransport;

static MASTER_LISTENER: OnceLock<Mutex<Option<MasterListener>>> = OnceLock::new();

pub struct MasterListener {
    shutdown_tx: mpsc::Sender<()>,
    thread_handle: Option<JoinHandle<()>>,
    pid: i32,
}

impl MasterListener {
    /// Start the master listener thread.
    ///
    /// This spawns a new OS thread with a Tokio runtime that listens for
    /// worker connections. Only one listener can be active per process.
    pub fn start(pid: i32, config: Config) -> io::Result<()> {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex.lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if listener_guard.is_some() {
            return Err(io::Error::other("Master listener is already running"));
        }

        let (shutdown_tx, shutdown_rx) = mpsc::channel();

        // Wrap shutdown receiver in Arc<Mutex<>> for sharing with async function
        let shutdown_rx = Arc::new(Mutex::new(shutdown_rx));

        let runtime = Runtime::new()
            .map_err(|e| io::Error::other(format!("Failed to create Tokio runtime: {}", e)))?;

        let thread_handle = thread::Builder::new()
            .name(format!("ddtrace-sidecar-listener-{}", pid))
            .spawn(move || {
                runtime.block_on(async {
                    if let Err(e) = run_listener(config, shutdown_rx).await {
                        error!("Listener thread error: {}", e);
                    }
                });
            })
            .map_err(|e| io::Error::other(format!("Failed to spawn listener thread: {}", e)))?;

        *listener_guard = Some(MasterListener {
            shutdown_tx,
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
        let mut listener_guard = listener_mutex.lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if let Some(mut master) = listener_guard.take() {
            let _ = master.shutdown_tx.send(());

            // Give the runtime a moment to process shutdown
            std::thread::sleep(Duration::from_millis(100));

            if let Some(handle) = master.thread_handle.take() {
                handle.join()
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
        let mut listener_guard = listener_mutex.lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if listener_guard.is_some() {
            info!("Clearing inherited master listener state in child process");
            *listener_guard = None;
        }

        Ok(())
    }
}

/// Async listener loop that accepts worker connections.
///
/// This runs in the listener thread's Tokio runtime and handles:
/// - Accepting new worker connections
/// - Spawning handlers for each connection
/// - Graceful shutdown on signal
async fn run_listener(config: Config, shutdown_rx: Arc<Mutex<mpsc::Receiver<()>>>) -> io::Result<()> {
    info!("Listener thread running, creating IPC server");

    // Create IPC server using the platform-specific Liaison
    let liaison: SharedDirLiaison = match config.ipc_mode {
        Shared => Liaison::ipc_shared(),
        InstancePerProcess => Liaison::ipc_per_process(),
    };

    let std_listener = liaison.attempt_listen()?
        .ok_or_else(|| io::Error::other("Failed to create IPC listener"))?;

    std_listener.set_nonblocking(true)?;
    let ipc_server = UnixListener::from_std(std_listener)?;

    info!("IPC server listening for worker connections");

    let server = SidecarServer::default();

    loop {
        if let Ok(rx) = shutdown_rx.lock() {
            if rx.try_recv().is_ok() || matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
                info!("Shutdown signal received, exiting listener loop");
                break;
            }
        }

        match tokio::time::timeout(Duration::from_millis(100), ipc_server.accept()).await {
            Ok(Ok((client, _addr))) => {
                info!("Accepted new worker connection");
                let server_clone = server.clone();

                tokio::spawn(async move {
                    handle_worker_connection(client, server_clone).await;
                });
            }
            Ok(Err(e)) => {
                error!("Failed to accept worker connection: {}", e);
            }
            Err(_) => {
                // Timeout - continue loop to check shutdown signal
                continue;
            }
        }
    }

    info!("Listener thread shutting down");
    Ok(())
}

/// Handle a single worker connection.
///
/// Processes requests from the worker and sends responses until the
/// connection is closed.
async fn handle_worker_connection(
    client: tokio::net::UnixStream,
    server: SidecarServer,
) {
    info!("Handling worker connection");
    server.accept_connection(AsyncChannel::from(client)).await;
    info!("Worker connection handler exiting");
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

    let channel = liaison.connect_to_server()
        .map_err(|e| io::Error::other(format!("Failed to connect to master listener: {}", e)))?;

    let transport = BlockingTransport::from(channel);

    let sidecar_transport = Box::new(SidecarTransport {
        inner: Mutex::new(transport),
        reconnect_fn: None, // Reconnection handled by caller
    });

    info!("Successfully connected to master listener");
    Ok(sidecar_transport)
}
