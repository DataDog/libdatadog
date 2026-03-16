// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use crate::setup::Liaison;
use datadog_ipc::platform::PIPE_PATH;
use datadog_ipc::{SeqpacketConn, SeqpacketListener};
use libc::getpid;
use std::io;

pub type IpcClient = SeqpacketConn;
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
            Err(ref e)
                if e.raw_os_error()
                    == Some(winapi::shared::winerror::ERROR_ACCESS_DENIED as i32) =>
            {
                Ok(None)
            }
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
    use datadog_ipc::{SeqpacketConn, SeqpacketListener};

    #[test]
    fn test_shared_dir_can_connect_to_socket() -> anyhow::Result<()> {
        use rand::distributions::Alphanumeric;
        use rand::{thread_rng, Rng};
        let random_prefix: Vec<u8> = thread_rng().sample_iter(&Alphanumeric).take(8).collect();
        let liaison = super::NamedPipeLiaison::new(String::from_utf8_lossy(&random_prefix));
        basic_liaison_connection_test(&liaison)?;
        Ok(())
    }

    pub fn basic_liaison_connection_test<T>(liaison: &T) -> Result<(), anyhow::Error>
    where
        T: Liaison,
    {
        {
            let listener: SeqpacketListener = liaison.attempt_listen().unwrap().unwrap();
            // can't listen twice when some listener is active
            assert!(liaison.attempt_listen().unwrap().is_none());

            // try_accept() must run concurrently with connect_to_server() because connect()
            // blocks reading the 4-byte PID handshake that try_accept() writes after accepting.
            let srv_thread = std::thread::spawn(move || listener.try_accept().unwrap());
            let client: SeqpacketConn = liaison.connect_to_server().unwrap();
            let srv: SeqpacketConn = srv_thread.join().unwrap();
            client.send_raw_blocking(&mut vec![255], &[]).unwrap();
            let mut buf =
                vec![0u8; datadog_ipc::max_message_size() + datadog_ipc::HANDLE_SUFFIX_SIZE];
            let (n, _) = srv.recv_raw_blocking(&mut buf).unwrap();
            assert_eq!(n, 1);
            assert_eq!(buf[0], 255);
            drop(client);
            // listener was moved into srv_thread and is dropped when the thread completes
        }

        // we should be able to open a new listener now
        let _listener = liaison.attempt_listen().unwrap().unwrap();
        Ok(())
    }
}
