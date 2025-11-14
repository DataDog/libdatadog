// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::enter_listener_loop;
use crate::one_way_shared_memory::open_named_shm;
use crate::service::blocking::SidecarTransport;
use crate::setup::pid_shm_path;
use arrayref::array_ref;
use datadog_ipc::platform::metadata::ProcessHandle;
use datadog_ipc::platform::{
    named_pipe_name_from_raw_handle, Channel, FileBackedHandle, MappedMem, NamedShmHandle,
    PIPE_PATH,
};

use futures::FutureExt;
use libdd_common::Endpoint;
use libdd_common::MutexExt;
use libdd_common_ffi::CharSlice;
use libdd_crashtracker_ffi::{ddog_crasht_init_windows, Metadata};
use manual_future::ManualFuture;
use spawn_worker::{write_crashtracking_trampoline, SpawnWorker, Stdio, TrampolineData};
use std::ffi::{CStr, CString};
use std::io::{self, Error};
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::ptr::null_mut;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

static MASTER_LISTENER: Mutex<Option<(thread::JoinHandle<()>, Arc<OwnedHandle>)>> =
    Mutex::new(None);
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::select;
use tracing::{error, info, warn};
use winapi::{
    shared::{
        minwindef::DWORD,
        sddl::ConvertSidToStringSidA,
        winerror::{
            ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_TOKEN, ERROR_PIPE_BUSY,
        },
    },
    um::{
        fileapi::{CreateFileA, OPEN_EXISTING},
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        minwinbase::SECURITY_ATTRIBUTES,
        processthreadsapi::{
            GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
        },
        securitybaseapi::GetTokenInformation,
        winbase::{
            CreateNamedPipeA, LocalFree, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
            PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES,
        },
        winnt::{TokenUser, GENERIC_READ, GENERIC_WRITE, HANDLE, TOKEN_QUERY, TOKEN_USER},
    },
};

// Helper function to generate the named pipe endpoint name for a master process
fn endpoint_name_for_master(master_pid: i32) -> String {
    format!(
        "{}libdatadog_master_{}_{}",
        PIPE_PATH,
        master_pid,
        crate::sidecar_version!()
    )
}

// Create and bind a Windows named pipe server
fn bind_named_pipe_listener(name: &str) -> io::Result<OwnedHandle> {
    let c_name = CString::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let mut sec_attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };

    unsafe {
        let handle = CreateNamedPipeA(
            c_name.as_ptr(),
            FILE_FLAG_OVERLAPPED
                | PIPE_ACCESS_OUTBOUND
                | PIPE_ACCESS_INBOUND
                | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            &mut sec_attributes,
        );

        if handle == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, error));
            }
            return Err(error);
        }

        Ok(OwnedHandle::from_raw_handle(handle as RawHandle))
    }
}

fn connect_named_pipe_client(name: &str) -> io::Result<RawHandle> {
    let c_name = CString::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let timeout_end = Instant::now() + Duration::from_secs(2);
    loop {
        let handle = unsafe {
            CreateFileA(
                c_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(error);
            }
        } else {
            return Ok(handle as RawHandle);
        }

        if Instant::now() > timeout_end {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        std::thread::yield_now();
    }
}

async fn accept_pipe_loop(
    pipe_listener: Arc<OwnedHandle>,
    handler: Box<dyn Fn(NamedPipeServer)>,
) -> io::Result<()> {
    let name = named_pipe_name_from_raw_handle(pipe_listener.as_raw_handle())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

    let raw_handle = pipe_listener.as_raw_handle();
    let mut pipe = unsafe { NamedPipeServer::from_raw_handle(raw_handle) }?;

    loop {
        match pipe.connect().await {
            Ok(_) => {
                let connected_pipe = pipe;
                pipe = ServerOptions::new().create(&name)?;
                handler(connected_pipe);
            }
            Err(e) => {
                error!("Error accepting pipe connection: {}", e);
                break;
            }
        }
    }

    Ok(())
}

fn stop_listening_on_handle(raw: RawHandle) {
    unsafe {
        CloseHandle(raw as HANDLE);
    }
}

pub fn transport_from_owned_handle(handle: OwnedHandle) -> io::Result<SidecarTransport> {
    let raw: RawHandle = handle.as_raw_handle();

    let name = named_pipe_name_from_raw_handle(raw)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;

    let getter = ProcessHandle::Getter(Box::new(move || {
        let timeout_end = Instant::now() + Duration::from_secs(2);
        let mut last_err: Option<Box<dyn std::error::Error>> = None;
        let pid_path = pid_shm_path(&name);
        loop {
            match open_named_shm(&pid_path) {
                Ok(shm) => {
                    let pid = u32::from_ne_bytes(*array_ref![shm.as_slice(), 0, 4]);
                    if pid != 0 {
                        return Ok(ProcessHandle::Pid(pid));
                    }
                }
                Err(e) => last_err = Some(Box::new(e)),
            }
            if Instant::now() > timeout_end {
                warn!(
                    "Reading sidecar pid from {} timed out (last error: {:?})",
                    pid_path.to_string_lossy(),
                    last_err
                );
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            std::thread::yield_now();
        }
    }));

    let channel = Channel::from_client_handle_and_pid(handle, getter);
    Ok(channel.into())
}

pub fn start_master_listener_windows(master_pid: i32) -> io::Result<()> {
    let name = endpoint_name_for_master(master_pid);

    let pipe_listener = match bind_named_pipe_listener(&name) {
        Ok(l) => l,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => return Ok(()),
        Err(e) => return Err(e),
    };

    let pipe_listener = Arc::new(pipe_listener);
    let pipe_listener_for_shutdown = pipe_listener.clone();

    let handle = thread::Builder::new()
        .name("dd-sidecar".into())
        .spawn(move || {
            let pipe_listener_clone = pipe_listener.clone();
            let acquire_listener = move || -> io::Result<_> {
                let raw = pipe_listener.as_raw_handle() as isize;
                let cancel = move || stop_listening_on_handle(raw as RawHandle);
                Ok((
                    move |handler| accept_pipe_loop(pipe_listener_clone.clone(), handler),
                    cancel,
                ))
            };

            let _ = enter_listener_loop(acquire_listener);
        })
        .map_err(io::Error::other)?;

    match MASTER_LISTENER.lock() {
        Ok(mut guard) => *guard = Some((handle, pipe_listener_for_shutdown)),
        Err(e) => {
            error!("Failed to acquire lock for storing master listener: {}", e);
            return Err(io::Error::other("Mutex poisoned"));
        }
    }

    Ok(())
}

pub fn connect_worker_windows(master_pid: i32) -> io::Result<OwnedHandle> {
    let name = endpoint_name_for_master(master_pid);

    let mut last_error = None;
    for _ in 0..10 {
        match connect_named_pipe_client(&name) {
            Ok(raw) => {
                return Ok(unsafe { OwnedHandle::from_raw_handle(raw) });
            }
            Err(e) => {
                last_error = Some(e);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    error!("Failed to connect to master listener");
    Err(last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::Other, "Connection failed")))
}

pub fn shutdown_master_listener_windows() -> io::Result<()> {
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

    if let Some((handle, pipe_listener)) = listener_data {
        let raw = pipe_listener.as_raw_handle();
        stop_listening_on_handle(raw);

        let (tx, rx) = std::sync::mpsc::channel();
        let helper_handle = std::thread::spawn(move || {
            let result = handle.join();
            let _ = tx.send(result);
        });

        // Wait up to 500ms for proper shutdown
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                error!("Listener thread panicked during shutdown");
            }
            Err(err) => {
                error!("Timeout waiting for listener thread to shut down: {}", err);
            }
        }

        // Join the helper thread to clean up its TLS
        if let Err(_) = helper_handle.join() {
            error!("Helper thread panicked");
        }
    }

    Ok(())
}

/// cbindgen:ignore
#[no_mangle]
pub extern "C" fn ddog_daemon_entry_point(_trampoline_data: &TrampolineData) {
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
                if let Some(completer) = close_completer.lock_or_panic().take() {
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

pub fn ddog_setup_crashtracking(endpoint: Option<&Endpoint>, metadata: Metadata) -> bool {
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
