// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use crate::setup::Liaison;
use libc::getpid;
use libdd_ipc::platform::PIPE_PATH;
use libdd_ipc::{AsyncConn, SeqpacketConn, SeqpacketListener};
use std::io;
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;

pub type IpcClient = AsyncConn;
pub type IpcServer = SeqpacketListener;

pub struct NamedPipeLiaison {
    socket_path: String,
}

impl Liaison for NamedPipeLiaison {
    fn connect_to_server(&self) -> io::Result<SeqpacketConn> {
        SeqpacketConn::connect(&self.socket_path)
    }

    fn attempt_listen(&self) -> io::Result<Option<SeqpacketListener>> {
        match SeqpacketListener::bind(&self.socket_path) {
            Ok(listener) => Ok(Some(listener)),
            Err(ref e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => Ok(None),
            Err(e) => Err(e),
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
        Self {
            socket_path: format!(
                "{}{}{}-libdd.{}",
                PIPE_PATH,
                prefix.as_ref(),
                primary_sidecar_identifier(),
                crate::sidecar_version!()
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
    use super::Liaison;
    use libdd_ipc::{SeqpacketConn, SeqpacketListener};

    #[test]
    fn test_shared_dir_can_connect_to_socket() -> anyhow::Result<()> {
        use rand::distributions::Alphanumeric;
        use rand::{Rng, thread_rng};
        let random_prefix: Vec<u8> = thread_rng().sample_iter(&Alphanumeric).take(8).collect();
        let liaison = super::NamedPipeLiaison::new(String::from_utf8_lossy(&random_prefix));
        basic_liaison_connection_test(&liaison)?;
        Ok(())
    }

    pub fn basic_liaison_connection_test<T>(liaison: &T) -> Result<(), anyhow::Error>
    where
        T: Liaison + Sync,
    {
        {
            let listener: SeqpacketListener = liaison.attempt_listen().unwrap().unwrap();
            // can't listen twice when some listener is active
            assert!(liaison.attempt_listen().unwrap().is_none());

            // connect_to_server() does not return until the listener accepts the pipe and sends
            // its PID, so run the blocking client while awaiting the accept.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let (client, srv): (SeqpacketConn, SeqpacketConn) = std::thread::scope(|scope| {
                let client_thread = scope.spawn(|| liaison.connect_to_server().unwrap());
                let srv = runtime.block_on(listener.accept_async()).unwrap();
                (client_thread.join().unwrap(), srv)
            });
            client.send_raw_blocking(vec![255], &[]).unwrap();
            let mut buf = vec![0u8; libdd_ipc::max_message_size() + libdd_ipc::HANDLE_SUFFIX_SIZE];
            let (n, _) = srv.recv_raw_blocking(&mut buf).unwrap();
            assert_eq!(n, 1);
            assert_eq!(buf[0], 255);
            drop(client);
        }

        // we should be able to open a new listener now
        let _listener = liaison.attempt_listen().unwrap().unwrap();
        Ok(())
    }
}
