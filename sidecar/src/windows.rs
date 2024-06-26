// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::enter_listener_loop;
use crate::setup::pid_shm_path;
use datadog_ipc::platform::{
    named_pipe_name_from_raw_handle, FileBackedHandle, MappedMem, NamedShmHandle,
};
use futures::FutureExt;
use kernel32::WTSGetActiveConsoleSessionId;
use manual_future::ManualFuture;
use spawn_worker::{SpawnWorker, Stdio};
use std::io;
use std::os::windows::io::{AsRawHandle, IntoRawHandle, OwnedHandle};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::select;

#[no_mangle]
pub extern "C" fn ddog_daemon_entry_point() {
    #[cfg(feature = "tracing")]
    crate::log::enable_logging().ok();

    let now = Instant::now();

    let pid = unsafe { libc::getpid() };

    if let Some(handle) = spawn_worker::recv_passed_handle() {
        let mut shm = match named_pipe_name_from_raw_handle(handle.as_raw_handle())
            .ok_or(io::Error::from(io::ErrorKind::InvalidInput))
            .and_then(|name| NamedShmHandle::create(pid_shm_path(&name), 4))
            .and_then(FileBackedHandle::map)
        {
            Ok(ok) => ok,
            Err(err) => {
                tracing::error!("Couldn't store pid to shared memory: {err}");
                return;
            }
        };
        shm.as_slice_mut().copy_from_slice(&pid.to_ne_bytes());

        tracing::info!("Starting sidecar, pid: {}", pid);

        let acquire_listener = move || unsafe {
            let (closed_future, close_completer) = ManualFuture::new();
            let close_completer = Arc::from(Mutex::new(Some(close_completer)));
            let pipe = NamedPipeServer::from_raw_handle(handle.into_raw_handle())?;

            let cancel = move || {
                if let Some(completer) = close_completer.lock().unwrap().take() {
                    tokio::spawn(completer.complete(()));
                }
            };

            // We pass the shm to ensure we drop the shm handle with the pid immediately after
            // cancellation To avoid actual race conditions
            Ok((
                |handler| accept_socket_loop(pipe, closed_future, handler, shm),
                cancel,
            ))
        };

        if let Err(err) = enter_listener_loop(acquire_listener) {
            tracing::error!("Error: {err}")
        }
    }

    tracing::info!(
        "shutting down sidecar, pid: {}, total runtime: {:.3}s",
        pid,
        now.elapsed().as_secs_f64()
    )
}

async fn accept_socket_loop(
    mut pipe: NamedPipeServer,
    cancellation: ManualFuture<()>,
    handler: Box<dyn Fn(NamedPipeServer)>,
    _: MappedMem<NamedShmHandle>,
) -> io::Result<()> {
    let name = named_pipe_name_from_raw_handle(pipe.as_raw_handle())
        .ok_or(io::Error::from(io::ErrorKind::InvalidInput))?;

    let cancellation = cancellation.shared();
    loop {
        select! {
            _ = cancellation.clone() => break,
            result = pipe.connect() => result?,
        }
        let connected_pipe = pipe;
        pipe = ServerOptions::new().create(&name)?;
        handler(connected_pipe);
    }
    // drops pipe and shm here
    Ok(())
}

pub fn setup_daemon_process(listener: OwnedHandle, spawn_cfg: &mut SpawnWorker) -> io::Result<()> {
    // Ensure unique process names - we spawn one sidecar per console session id (see
    // setup/windows.rs for the reasoning)
    spawn_cfg
        .process_name(format!(
            "datadog-ipc-helper-{}",
            primary_sidecar_identifier()
        ))
        .pass_handle(listener)
        .stdin(Stdio::Null);

    Ok(())
}

pub fn primary_sidecar_identifier() -> u32 {
    unsafe { WTSGetActiveConsoleSessionId() }
}
