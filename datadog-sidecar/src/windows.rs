// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::enter_listener_loop;
use libdd_ipc::platform::ProcessIdentityGuard;
use libdd_ipc::{AsyncConn, SeqpacketListener};

use futures::FutureExt;
use libdd_common::Endpoint;
use libdd_common::MutexExt;
use libdd_common_ffi::CharSlice;
use libdd_crashtracker_ffi::{Metadata, ddog_crasht_init_windows};
use manual_future::ManualFuture;
use spawn_worker::{SpawnWorker, Stdio, TrampolineData, write_crashtracking_trampoline};
use std::ffi::CStr;
use std::io::{self, Error};
use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::select;
use tracing::{error, info};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, LocalFree},
    Security::{
        Authorization::ConvertSidToStringSidA, GetSidSubAuthority, GetSidSubAuthorityCount,
        GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_USER, TokenIntegrityLevel, TokenUser,
    },
};

pub mod remote_config_notification;

/// cbindgen:ignore
#[unsafe(no_mangle)]
pub extern "C" fn ddog_daemon_entry_point(_trampoline_data: &TrampolineData) {
    #[cfg(feature = "tracing")]
    crate::log::enable_logging().ok();

    // Restore the pipe buffer size the PHP parent process configured before spawning us,
    // so subsequent try_accept calls use the same buffer size.
    let buf_size = crate::config::Config::get().pipe_buffer_size;
    if buf_size > 0 {
        libdd_ipc::platform::set_pipe_buffer_size(buf_size);
    }

    let now = Instant::now();

    let pid = unsafe { libc::getpid() };

    if let Some(handle) = spawn_worker::recv_passed_handle() {
        info!("Starting sidecar, pid: {}", pid);

        let acquire_listener = move || {
            let (closed_future, close_completer) = ManualFuture::new();
            let close_completer = Arc::from(Mutex::new(Some(close_completer)));
            let listener = SeqpacketListener::from_owned_fd(handle);

            let cancel = move || {
                if let Some(completer) = close_completer.lock_or_panic().take() {
                    tokio::spawn(completer.complete(()));
                }
            };

            Ok((
                |handler| accept_socket_loop(listener, closed_future, handler),
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
    listener: SeqpacketListener,
    cancellation: ManualFuture<()>,
    handler: Box<dyn Fn(AsyncConn)>,
) -> io::Result<()> {
    let cancellation = cancellation.shared();
    loop {
        select! {
            _ = cancellation.clone() => break,
            result = listener.accept_async() => {
                handler(result?);
            }
        }
    }
    Ok(())
}

pub fn setup_daemon_process(
    listener: SeqpacketListener,
    spawn_cfg: &mut SpawnWorker,
) -> io::Result<()> {
    // Ensure unique process names - we spawn one sidecar per console session id (see
    // setup/windows.rs for the reasoning)
    let raw = listener.into_raw_handle();
    let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
    spawn_cfg
        .process_name(format!(
            "datadog-ipc-helper-{}",
            primary_sidecar_identifier()
        ))
        .pass_handle(owned)
        .stdin(Stdio::Null);

    Ok(())
}

pub fn ddog_setup_crashtracking(endpoint: Option<&Endpoint>, metadata: Metadata) -> bool {
    // The trampoline and its registration must belong to the process identity.
    let _identity = ProcessIdentityGuard::enter();
    // Ensure unique process names - we spawn one sidecar per console session id (see
    // setup/windows.rs for the reasoning)
    match write_crashtracking_trampoline(&format!(
        "datadog-crashtracking-{}",
        primary_sidecar_identifier()
    )) {
        Ok((path, _)) => {
            if let Ok(path_str) = path.into_os_string().into_string() {
                return ddog_crasht_init_windows(
                    CharSlice::from(path_str.as_str()),
                    endpoint,
                    metadata,
                );
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

static SIDECAR_IDENTIFIER: LazyLock<String> = LazyLock::new(fetch_sidecar_identifier);

/// `GetCurrentProcessToken()`: an inline pseudo-handle from processthreadsapi.h (Windows 8+),
/// not exported and thus not in windows-sys. It must never be closed.
const CURRENT_PROCESS_TOKEN: HANDLE = -4;

fn fetch_sidecar_identifier() -> String {
    unsafe {
        // Unlike OpenProcessToken, not access-checked against an impersonating thread.
        // Never the thread token: the sidecar inherits the process identity.
        let access_token = CURRENT_PROCESS_TOKEN;

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
                error!("Failed fetching process token: {:?}", err);
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
            error!(
                "Failed fetching process token: {:?}",
                Error::last_os_error()
            );
            return "".to_string();
        }

        let mut string_sid = null_mut();
        let success = ConvertSidToStringSidA((*user_token).User.Sid, &mut string_sid);

        if success == 0 {
            error!("Failed stringifying SID: {:?}", Error::last_os_error());
            return "".to_string();
        }

        let user_sid =
            String::from_utf8_lossy(CStr::from_ptr(string_sid.cast()).to_bytes()).to_string();
        LocalFree(string_sid.cast());

        // Also include the integrity level so that elevated (admin) and non-elevated processes
        // of the same user get different sidecar identifiers and thus different sidecars.
        // Without this, a non-elevated PHP process would try to connect to a sidecar spawned by
        // an elevated PHP process and fail with access denied.
        let integrity_level = fetch_integrity_level(access_token).unwrap_or(0);

        format!("{}-{:x}", user_sid, integrity_level)
    }
}

/// Returns the mandatory integrity level RID from the token (e.g. 0x1000=Low, 0x2000=Medium,
/// 0x3000=High/admin, 0x4000=System), or None on failure.
unsafe fn fetch_integrity_level(token: HANDLE) -> Option<u32> {
    unsafe {
        let mut size = 0u32;
        GetTokenInformation(token, TokenIntegrityLevel, null_mut(), 0, &mut size);
        if size == 0 {
            return None;
        }

        let buf = Vec::<u8>::with_capacity(size as usize);
        let label = buf.as_ptr() as *const TOKEN_MANDATORY_LABEL;
        if GetTokenInformation(token, TokenIntegrityLevel, label as *mut _, size, &mut size) == 0 {
            return None;
        }

        let sid = (*label).Label.Sid;
        let count = *GetSidSubAuthorityCount(sid) as u32;
        if count == 0 {
            return None;
        }
        Some(*GetSidSubAuthority(sid, count - 1))
    }
}

pub fn primary_sidecar_identifier() -> &'static str {
    &SIDECAR_IDENTIFIER
}

/// What shared-memory names are qualified by: one sidecar per user session.
pub fn shm_namespace() -> &'static str {
    primary_sidecar_identifier()
}

#[test]
fn test_fetch_identifier() {
    assert!(primary_sidecar_identifier().starts_with("S-"));
}

#[test]
fn test_fetch_identifier_while_impersonating() {
    use windows_sys::Win32::Security::{ImpersonateAnonymousToken, RevertToSelf};
    use windows_sys::Win32::System::Threading::GetCurrentThread;

    struct Anonymous;
    impl Drop for Anonymous {
        fn drop(&mut self) {
            unsafe { RevertToSelf() };
        }
    }

    // Not impersonating, every token API yields the process identity, which the daemon inherits.
    let process_identifier = fetch_sidecar_identifier();
    assert!(process_identifier.starts_with("S-"));

    assert_ne!(unsafe { ImpersonateAnonymousToken(GetCurrentThread()) }, 0);
    let _anonymous = Anonymous;
    // The thread token would give ANONYMOUS LOGON (S-1-5-7), and OpenProcessToken is denied.
    let identifier = fetch_sidecar_identifier();
    assert!(!identifier.starts_with("S-1-5-7-"), "{identifier}");
    assert_eq!(identifier, process_identifier);
}
