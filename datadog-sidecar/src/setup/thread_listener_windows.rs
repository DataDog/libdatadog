// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use tokio::sync::oneshot;
use tracing::{error, info};

use crate::config::Config;
use crate::entry::MainLoopConfig;
use crate::service::blocking::SidecarTransport;
use crate::setup::Liaison;
use crate::setup::NamedPipeLiaison;
use datadog_ipc::{SeqpacketConn, SeqpacketListener};
use futures::FutureExt;
use manual_future::ManualFuture;

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

        let liaison = NamedPipeLiaison::new(format!("libdatadog_{}_", pid));
        let listener = liaison
            .attempt_listen()?
            .ok_or_else(|| io::Error::other("Failed to bind master listener pipe"))?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let thread_handle = thread::Builder::new()
            .name(format!("ddtrace-sidecar-listener-{}", pid))
            .spawn(move || {
                if let Err(e) = run_listener_windows(listener, shutdown_rx) {
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
    pub fn shutdown() -> io::Result<()> {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex
            .lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if let Some(mut master) = listener_guard.take() {
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
async fn accept_socket_loop_thread_windows(
    listener: SeqpacketListener,
    handler: Box<dyn Fn(SeqpacketConn)>,
    shutdown_rx: oneshot::Receiver<()>,
) -> io::Result<()> {
    let (closed_future, close_completer) = ManualFuture::new();
    let close_completer = Arc::new(Mutex::new(Some(close_completer)));

    tokio::spawn({
        let close_completer = Arc::clone(&close_completer);
        async move {
            let _ = shutdown_rx.await;
            if let Some(completer) = close_completer.lock().ok().and_then(|mut g| g.take()) {
                completer.complete(()).await;
            }
        }
    });

    let listener = Arc::new(listener);
    let cancellation = closed_future.shared();
    loop {
        let listener_clone = Arc::clone(&listener);
        tokio::select! {
            _ = cancellation.clone() => {
                info!("Shutdown signal received in Windows pipe listener");
                break;
            }
            result = tokio::task::spawn_blocking(move || listener_clone.accept_blocking()) => {
                match result {
                    Ok(Ok(conn)) => handler(conn),
                    Ok(Err(e)) => {
                        error!("Failed to accept worker connection: {}", e);
                        break;
                    }
                    Err(e) => {
                        error!("Listener task panicked: {}", e);
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Entry point for Windows named pipe listener.
fn run_listener_windows(
    listener: SeqpacketListener,
    shutdown_rx: oneshot::Receiver<()>,
) -> io::Result<()> {
    info!("Listener thread running, creating Windows named pipe server");

    let loop_config = MainLoopConfig {
        enable_ctrl_c_handler: false,
        enable_crashtracker: false,
        external_shutdown_rx: None,
        init_shm_eagerly: true,
    };

    crate::entry::enter_listener_loop_with_config(
        move || {
            let cancel = || {};
            Ok((
                move |handler| accept_socket_loop_thread_windows(listener, handler, shutdown_rx),
                cancel,
            ))
        },
        loop_config,
    )
    .map_err(|e| io::Error::other(format!("Windows thread listener failed: {}", e)))?;

    info!("Listener thread exiting");
    Ok(())
}

/// Connect to the master listener as a worker using Windows Named Pipes.
pub fn connect_to_master(pid: i32) -> io::Result<Box<SidecarTransport>> {
    info!("Connecting to master listener via named pipe (PID {})", pid);

    let liaison = NamedPipeLiaison::new(format!("libdatadog_{}_", pid));
    let conn = liaison.connect_to_server().map_err(|e| {
        io::Error::other(format!("Failed to connect to master listener: {}", e))
    })?;

    info!("Successfully connected to master listener");
    Ok(Box::new(SidecarTransport::from(conn)))
}
