// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::enter_listener_loop;
use crate::setup::pid_shm_path;
use datadog_ipc::platform::{
    named_pipe_name_from_raw_handle, FileBackedHandle, MappedMem, NamedShmHandle,
};

use datadog_crashtracker_ffi::{ddog_crasht_init_windows, Metadata};
use ddcommon::Endpoint;
use ddcommon_ffi::CharSlice;
use futures::FutureExt;
use lazy_static::lazy_static;
use manual_future::ManualFuture;
use spawn_worker::{write_crashtracking_trampoline, SpawnWorker, Stdio};
use std::ffi::CStr;
use std::io::{self, Error};
use std::os::windows::io::{AsRawHandle, IntoRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::select;
use tracing::{error, info};
use winapi::{
    shared::{
        sddl::ConvertSidToStringSidA,
        winerror::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_TOKEN},
    },
    um::{
        handleapi::CloseHandle,
        processthreadsapi::{
            GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
        },
        securitybaseapi::GetTokenInformation,
        winbase::LocalFree,
        winnt::{TokenUser, HANDLE, TOKEN_QUERY, TOKEN_USER},
    },
};

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
                error!("Couldn't store pid to shared memory: {err}");
                return;
            }
        };
        shm.as_slice_mut().copy_from_slice(&pid.to_ne_bytes());

        info!("Starting sidecar, pid: {}", pid);

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
            error!("Error: {err}")
        }
    }

    info!(
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

#[no_mangle]
pub extern "C" fn ddog_setup_crashtracking(
    endpoint: Option<&Endpoint>,
    metadata: Metadata,
) -> bool {
    // Ensure unique process names - we spawn one sidecar per console session id (see
    // setup/windows.rs for the reasoning)
    match write_crashtracking_trampoline(&format!(
        "datadog-crashtracking-{}",
        primary_sidecar_identifier()
    )) {
        Ok((path, _)) => {
            if let Some(path_str) = path.into_os_string().into_string().ok() {
                unsafe {
                    return ddog_crasht_init_windows(CharSlice::from(path_str.as_str()), endpoint, metadata);
                }
            } else {
                error!("Failed to convert path to string");
            }
        }
        Err(e) => {
            error!("Failed to write crashtracking trampoline: {}", e);
        }
    }

    false
}

lazy_static! {
    static ref SIDECAR_IDENTIFIER: String = fetch_sidecar_identifier();
}

fn fetch_sidecar_identifier() -> String {
    unsafe {
        let mut access_token = null_mut();

        'token: {
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut access_token) != 0 {
                break 'token;
            }
            let mut err = Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_NO_TOKEN as i32) {
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut access_token) != 0 {
                    break 'token;
                }
                err = Error::last_os_error();
            }
            error!("Failed fetching thread token: {:?}", err);
            return "".to_string();
        }

        let mut info_buffer_size = 0;
        if GetTokenInformation(
            access_token,
            TokenUser,
            null_mut(),
            0,
            &mut info_buffer_size,
        ) == 0
        {
            let err = Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
                error!("Failed fetching thread token: {:?}", err);
                CloseHandle(access_token);
                return "".to_string();
            }
        }

        let user_token_mem = Vec::<u8>::with_capacity(info_buffer_size as usize);
        let user_token = user_token_mem.as_ptr() as *const TOKEN_USER;
        if GetTokenInformation(
            access_token,
            TokenUser,
            user_token as *mut _,
            info_buffer_size,
            &mut info_buffer_size,
        ) == 0
        {
            error!("Failed fetching thread token: {:?}", Error::last_os_error());
            CloseHandle(access_token);
            return "".to_string();
        }

        let mut string_sid = null_mut();
        let success = ConvertSidToStringSidA((*user_token).User.Sid, &mut string_sid);
        CloseHandle(access_token);

        if success == 0 {
            error!("Failed stringifying SID: {:?}", Error::last_os_error());
            return "".to_string();
        }

        let str = String::from_utf8_lossy(CStr::from_ptr(string_sid).to_bytes()).to_string();
        LocalFree(string_sid as HANDLE);
        str
    }
}

pub fn primary_sidecar_identifier() -> &'static str {
    SIDECAR_IDENTIFIER.as_str()
}

#[test]
fn test_fetch_identifier() {
    assert!(primary_sidecar_identifier().starts_with("S-"));
}
