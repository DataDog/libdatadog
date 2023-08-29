// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use arrayref::array_ref;
use datadog_ipc::platform::{
    AsyncChannel, Channel, FileBackedHandle, NamedPipe, NamedShmHandle, ProcessHandle, PIPE_PATH,
};
use kernel32::WTSGetActiveConsoleSessionId;
use std::ffi::CString;
use std::os::raw::c_void;
use std::ptr::null_mut;
use std::time::{Duration, Instant};
use std::{env, io, mem};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use winapi::{DWORD, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, SECURITY_ATTRIBUTES};

use crate::setup::Liaison;

pub type IpcClient = NamedPipeServer;
pub type IpcServer = NamedPipeServer;

pub struct NamedPipeLiaison {
    socket_path: String,
}

pub fn pid_shm_path(pipe_path: &str) -> CString {
    CString::new(&pipe_path[PIPE_PATH.len() - 1..]).unwrap()
}

impl Liaison for NamedPipeLiaison {
    fn connect_to_server(&self) -> io::Result<Channel> {
        let timeout_end = Instant::now() + Duration::from_secs(2);
        let pipe = loop {
            match ClientOptions::new().open(&self.socket_path) {
                Ok(client) => break client,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => (),
                Err(e) => return Err(e),
            }

            if Instant::now() > timeout_end {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            std::thread::yield_now();
        };
        let socket_path = self.socket_path.clone();
        // Have a ProcessHandle::Getter() so that we don't immediately block in case the sidecar is still starting up, but only the first time we want to submit shared memory
        Ok(Channel::from(AsyncChannel::from_raw_and_process(
            NamedPipe::Client(pipe),
            ProcessHandle::Getter(Box::new(move || {
                // Await the shared memory handle which will contain the pid of the sidecar - it may not be immediately available during startup
                let timeout_end = Instant::now() + Duration::from_secs(2);
                loop {
                    if let Ok(shm) = NamedShmHandle::open(pid_shm_path(&socket_path)) {
                        let shm = shm.map()?;
                        let pid = u32::from_ne_bytes(*array_ref![shm.as_slice(), 0, 4]);
                        if pid != 0 {
                            return Ok(ProcessHandle::Pid(pid));
                        }
                    }
                    if Instant::now() > timeout_end {
                        return Err(io::Error::from(io::ErrorKind::TimedOut));
                    }
                    std::thread::yield_now();
                }
            })),
        )))
    }

    fn attempt_listen(&self) -> io::Result<Option<NamedPipeServer>> {
        let mut options = ServerOptions::new();
        options.first_pipe_instance(true); // To assert existence of the pipe or get a new pipe

        let mut sec_attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1, // We want this one to be inherited
        };
        match unsafe {
            options.create_with_security_attributes_raw(
                &self.socket_path,
                &mut sec_attributes as *mut SECURITY_ATTRIBUTES as *mut c_void,
            )
        } {
            Ok(server) => Ok(Some(server)),
            Err(e) => {
                if e.raw_os_error()
                    .map_or(true, |r| r as u32 == ERROR_ACCESS_DENIED)
                {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }

    fn ipc_shared() -> Self {
        Self::new_default_location()
    }

    fn ipc_per_process() -> Self {
        //TODO: implement per pid handling
        Self::new_default_location()
    }
}

impl NamedPipeLiaison {
    pub fn new<P: AsRef<str>>(prefix: P) -> Self {
        // Due to the restriction on Global\ namespace for shared memory we have to distinguish individual sidecar sessions.
        // Fetch the session_id to effectively namespace the Named Pipe names too.
        let session_id = unsafe { WTSGetActiveConsoleSessionId() };
        Self {
            socket_path: format!(
                "{}{}{}-libdd.{}",
                PIPE_PATH,
                prefix.as_ref(),
                session_id,
                env!("CARGO_PKG_VERSION")
            ),
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
    use futures::future;
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::Liaison;

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
            let mut srv = liaison.attempt_listen().unwrap().unwrap();

            // can't listen twice when some listener is active
            assert!(liaison.attempt_listen().unwrap().is_none());
            // a liaison can try connecting to existing socket to ensure its valid, adding connection to accept queue
            // but we can drain any preexisting connections in the queue
            let (_, result) = future::join(
                srv.connect(),
                tokio::spawn(async move { (liaison.connect_to_server().unwrap(), liaison) }),
            )
            .await;
            let (mut client, liaison) = result.unwrap();
            assert_eq!(1, client.write(&[255]).await.unwrap());
            let mut buf = [0; 1];
            assert_eq!(1, srv.read(&mut buf).await.unwrap());
            srv.disconnect();
            liaison
        };

        // we should be able to open new listener now
        let _listener = liaison.attempt_listen().unwrap().unwrap();
        Ok(())
    }
}
