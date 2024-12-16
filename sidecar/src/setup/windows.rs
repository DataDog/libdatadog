// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::one_way_shared_memory::open_named_shm;
use crate::primary_sidecar_identifier;
use crate::setup::Liaison;
use arrayref::array_ref;
use datadog_ipc::platform::metadata::ProcessHandle;
use datadog_ipc::platform::{Channel, PIPE_PATH};
use libc::getpid;
use std::error::Error;
use std::ffi::CString;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::ptr::null_mut;
use std::time::{Duration, Instant};
use std::{env, io, mem};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tracing::warn;
use winapi::{
    shared::{
        minwindef::DWORD,
        winerror::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY},
    },
    um::{
        fileapi::{CreateFileA, OPEN_EXISTING},
        handleapi::INVALID_HANDLE_VALUE,
        minwinbase::SECURITY_ATTRIBUTES,
        winbase::{
            CreateNamedPipeA, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
            PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES,
        },
        winnt::{GENERIC_READ, GENERIC_WRITE},
    },
};

pub type IpcClient = NamedPipeServer;
pub type IpcServer = OwnedHandle;

pub struct NamedPipeLiaison {
    socket_path: CString,
}

pub fn pid_shm_path(pipe_path: &str) -> CString {
    CString::new(&pipe_path[PIPE_PATH.len() - 1..]).unwrap()
}

impl Liaison for NamedPipeLiaison {
    fn connect_to_server(&self) -> io::Result<Channel> {
        let timeout_end = Instant::now() + Duration::from_secs(2);
        let pipe = loop {
            let h = unsafe {
                CreateFileA(
                    self.socket_path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    null_mut(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    null_mut(),
                )
            };
            if h == INVALID_HANDLE_VALUE {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                    return Err(error);
                }
            } else {
                break h;
            }

            if Instant::now() > timeout_end {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            std::thread::yield_now();
        };

        let socket_path = self.socket_path.clone();
        // Have a ProcessHandle::Getter() so that we don't immediately block in case the sidecar is
        // still starting up, but only the first time we want to submit shared memory
        Ok(Channel::from_client_handle_and_pid(
            unsafe { OwnedHandle::from_raw_handle(pipe as RawHandle) },
            ProcessHandle::Getter(Box::new(move || {
                // Await the shared memory handle which will contain the pid of the sidecar
                // As it may not be immediately available during startup
                let timeout_end = Instant::now() + Duration::from_secs(2);
                let mut last_error: Option<Box<dyn Error>> = None;
                let pid_path = pid_shm_path(&String::from_utf8_lossy(socket_path.as_bytes()));
                loop {
                    match open_named_shm(&pid_path) {
                        Ok(shm) => {
                            #[cfg(windows_seh_wrapper)]
                            let pid = {
                                let mut pid = 0;
                                if let Err(e) = microseh::try_seh(|| {
                                    pid = u32::from_ne_bytes(*array_ref![shm.as_slice(), 0, 4])
                                }) {
                                    last_error = Some(Box::new(e));
                                }
                                pid
                            };

                            #[cfg(not(windows_seh_wrapper))]
                            let pid = u32::from_ne_bytes(*array_ref![shm.as_slice(), 0, 4]);

                            if pid != 0 {
                                return Ok(ProcessHandle::Pid(pid));
                            }
                        }
                        Err(e) => last_error = Some(Box::new(e)),
                    }
                    if Instant::now() > timeout_end {
                        warn!("Reading the sidecar pid from {} timed out after {:?}. (last error: {:?})",
                            pid_path.to_string_lossy(), timeout_end, last_error);
                        return Err(io::Error::from(io::ErrorKind::TimedOut));
                    }
                    std::thread::yield_now();
                }
            })),
        ))
    }

    fn attempt_listen(&self) -> io::Result<Option<OwnedHandle>> {
        let mut sec_attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1, // We want this one to be inherited
        };
        match unsafe {
            CreateNamedPipeA(
                self.socket_path.as_ptr(),
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
            )
        } {
            INVALID_HANDLE_VALUE => {
                let error = io::Error::last_os_error();
                if match error.raw_os_error() {
                    Some(code) => code as u32 == ERROR_ACCESS_DENIED,
                    None => true,
                } {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
            h => Ok(Some(unsafe {
                OwnedHandle::from_raw_handle(h as RawHandle)
            })),
        }
    }

    fn ipc_shared() -> Self {
        Self::new_default_location()
    }

    fn ipc_per_process() -> Self {
        Self::new(format!("libdatadog_{}_", unsafe { getpid() }))
    }
}

impl NamedPipeLiaison {
    pub fn new<P: AsRef<str>>(prefix: P) -> Self {
        // Due to the restriction on Global\ namespace for shared memory we have to distinguish
        // individual sidecar sessions. Fetch the session_id to effectively namespace the
        // Named Pipe names too.
        Self {
            socket_path: CString::new(format!(
                "{}{}{}-libdd.{}",
                PIPE_PATH,
                prefix.as_ref(),
                primary_sidecar_identifier(),
                crate::sidecar_version!()
            ))
            .unwrap(),
        }
    }

    pub fn new_default_location() -> Self {
        Self::new("libdatadog_")
    }
}

impl Default for NamedPipeLiaison {
    fn default() -> Self {
        Self::ipc_per_process()
    }
}

pub type DefaultLiason = NamedPipeLiaison;

#[cfg(test)]
mod tests {
    use super::Liaison;
    use futures::future;
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};
    use std::io::Write;
    use std::os::windows::io::IntoRawHandle;
    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::NamedPipeServer;
    use winapi::um::{handleapi::CloseHandle, winnt::HANDLE};

    #[tokio::test]
    async fn test_shared_dir_can_connect_to_socket() -> anyhow::Result<()> {
        let random_prefix: Vec<u8> = thread_rng().sample_iter(&Alphanumeric).take(8).collect();
        let liaison = super::NamedPipeLiaison::new(String::from_utf8_lossy(&random_prefix));
        basic_liaison_connection_test(liaison).await.unwrap();
        Ok(())
    }

    pub async fn basic_liaison_connection_test<T>(liaison: T) -> Result<(), anyhow::Error>
    where
        T: Liaison + Send + Sync + 'static,
    {
        let liaison = {
            let raw_handle = liaison.attempt_listen().unwrap().unwrap().into_raw_handle();
            let mut srv = unsafe { NamedPipeServer::from_raw_handle(raw_handle) }.unwrap();

            // can't listen twice when some listener is active
            //assert!(liaison.attempt_listen().unwrap().is_none());
            // a liaison can try connecting to existing socket to ensure its valid, adding
            // connection to accept queue but we can drain any preexisting connections
            // in the queue
            let (_, result) = future::join(
                srv.connect(),
                tokio::spawn(async move { (liaison.connect_to_server().unwrap(), liaison) }),
            )
            .await;
            let (mut client, liaison) = result.unwrap();
            assert_eq!(1, client.write(&[255]).unwrap());
            let mut buf = [0; 1];
            assert_eq!(1, srv.read(&mut buf).await.unwrap());

            // for this test: Somehow, NamedPipeServer remains tangled with the event-loop and won't
            // free itself in time
            unsafe { CloseHandle(raw_handle as HANDLE) };
            std::mem::forget(srv);

            liaison
        };

        // we should be able to open new listener now
        let _listener = liaison.attempt_listen().unwrap().unwrap();
        Ok(())
    }
}
