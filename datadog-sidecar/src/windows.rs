// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::enter_listener_loop;
use datadog_ipc::platform::named_pipe_name_from_raw_handle;
use std::sync::LazyLock;
use std::ffi::CStr;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};
use std::ptr::null_mut;
use std::io::Error;
use std::time::Instant;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tracing::{error, info};

/// Run the sidecar daemon using a named-pipe handle passed by the parent via
/// `__DD_INTERNAL_PASSED_FD`.  Equivalent to `run_daemon_from_passed_fd` on Unix.
pub fn run_daemon_from_passed_handle() {
    #[cfg(feature = "tracing")]
    crate::log::enable_logging().ok();

    let now = Instant::now();

    if let Some(handle) = spawn_worker::recv_passed_handle() {
        info!("Starting sidecar");

        let acquire_listener = move || {
            let pipe = unsafe {
                NamedPipeServer::from_raw_handle(handle.into_raw_handle())?
            };
            let name = named_pipe_name_from_raw_handle(pipe.as_raw_handle())
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
            let cancel = move || {};
            Ok((
                move |handler: Box<dyn Fn(NamedPipeServer)>| accept_socket_loop(pipe, name, handler),
                cancel,
            ))
        };
        if let Err(err) = enter_listener_loop(acquire_listener) {
            error!("Error: {err}");
        }
    }

    info!(
        "shutting down sidecar, total runtime: {:.3}s",
        now.elapsed().as_secs_f64()
    )
}

async fn accept_socket_loop(
    mut pipe: NamedPipeServer,
    name: String,
    handler: Box<dyn Fn(NamedPipeServer)>,
) -> io::Result<()> {
    loop {
        pipe.connect().await?;
        let connected = pipe;
        pipe = ServerOptions::new().create(&name)?;
        handler(connected);
    }
}

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



static SIDECAR_IDENTIFIER: LazyLock<String> = LazyLock::new(fetch_sidecar_identifier);

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
    &SIDECAR_IDENTIFIER
}

#[test]
fn test_fetch_identifier() {
    assert!(primary_sidecar_identifier().starts_with("S-"));
}
