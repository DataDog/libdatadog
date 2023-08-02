// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{env, io, mem};
use std::os::raw::c_void;
use std::ptr::null_mut;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::ERROR_ACCESS_DENIED;
use winapi::um::minwinbase::SECURITY_ATTRIBUTES;

use crate::setup::Liaison;

pub type IpcClient = NamedPipeClient;
pub type IpcServer = NamedPipeServer;

pub struct NamedPipeLiaison {
    socket_path: String,
}

impl Liaison for NamedPipeLiaison {
    fn connect_to_server(&self) -> io::Result<NamedPipeClient> {
        ClientOptions::new().open(&self.socket_path)
    }

    fn attempt_listen(&self) -> io::Result<Option<NamedPipeServer>> {
        let options = ServerOptions::new()
            .first_pipe_instance(true);

        let sec_attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1, // We want this one to be inherited
        };
        match unsafe { options.create_with_security_attributes_raw(&self.socket_path, &sec_attributes as *mut c_void) } {
            Ok(server) => Ok(Some(server)),
            Err(e) => {
                if e.raw_os_error().map_or(true, |r| r == ERROR_ACCESS_DENIED) {
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
        Self {
            socket_path: format!("\\\\.\\pipe\\{}libdd.{}", prefix.as_ref(), env!("CARGO_PKG_VERSION")),
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
    use std::os::windows::io::AsRawHandle;
    use futures::future;
    use rand::{Rng, thread_rng};
    use rand::distributions::Alphanumeric;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use datadog_ipc::platform::named_pipe_name_from_raw_handle;

    use super::Liaison;

    #[tokio::test]
    async fn test_shared_dir_can_connect_to_socket() -> anyhow::Result<()> {
        let random_prefix =  thread_rng().sample_iter(&Alphanumeric).take(8).collect();
        let liaison = super::NamedPipeLiaison::new(random_prefix);
        basic_liaison_connection_test(&liaison).await.unwrap();
        // socket file will still exist - even if we close everything
        assert!(liaison.socket_path.exists());
        Ok(())
    }

    pub async fn basic_liaison_connection_test<T>(liaison: &T) -> Result<(), anyhow::Error>
    where
        T: Liaison,
    {
        {
            let mut srv = liaison.attempt_listen().unwrap().unwrap();
            let pipe_name = named_pipe_name_from_raw_handle(srv.as_raw_handle()).unwrap();

            // can't listen twice when some listener is active
            assert!(liaison.attempt_listen().unwrap().is_none());
            // a liaison can try connecting to existing socket to ensure its valid, adding connection to accept queue
            // but we can drain any preexisting connections in the queue
            let (_, mut client) = future::join(srv.connect(), tokio::spawn(async move {
                liaison.connect_to_server().unwrap()
            })).await;
            let mut client = client.unwrap();
            assert_eq!(1, client.write(&[255]).await.unwrap());
            let mut buf = [0; 1];
            assert_eq!(1, srv.read(&mut buf).await.unwrap());
            drop(srv);
            drop(client);
        }

        // we should be able to open new listener now
        let _listener = liaison.attempt_listen().unwrap().unwrap();
        Ok(())
    }
}
