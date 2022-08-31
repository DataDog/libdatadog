// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    env, fs, io,
    os::unix::{
        net::{UnixListener, UnixStream},
        prelude::PermissionsExt,
    },
    path::{Path, PathBuf},
};

use crate::ipc::platform::{FLock, IsListening};

use super::Liaison;

fn ensure_dir_world_writable<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let mut perm = path.as_ref().metadata()?.permissions();
    perm.set_mode(0o777);
    fs::set_permissions(path, perm)
}

fn ensure_dir_exists<P: AsRef<Path>>(path: P) -> io::Result<()> {
    if path.as_ref().exists() {
        return Ok(());
    }

    fs::create_dir_all(&path)?;
    ensure_dir_world_writable(&path)?;

    Ok(())
}

pub struct SharedDirLiaison {
    socket_path: PathBuf,
    lock_path: PathBuf,
}

impl Liaison for SharedDirLiaison {
    fn connect_to_server(&self) -> io::Result<UnixStream> {
        UnixStream::connect(&self.socket_path)
    }

    fn attempt_listen(&self) -> io::Result<Option<UnixListener>> {
        let dir = self.socket_path.parent().unwrap_or_else(|| Path::new("/"));
        ensure_dir_exists(dir)?;

        let _g = match FLock::rw_lock(&self.lock_path) {
            Ok(lock) => lock,
            // failing to acquire lock
            // means that another process is creating the socket
            Err(_) => return Ok(None),
        };

        if self.socket_path.exists() {
            // if socket is already listening, then creating listener is not available
            if UnixListener::is_listening(&self.socket_path)? {
                return Ok(None);
            }
            fs::remove_file(&self.socket_path)?;
        }
        Ok(Some(UnixListener::bind(&self.socket_path)?))
    }
}

impl SharedDirLiaison {
    pub fn new<P: AsRef<Path>>(base_dir: P) -> Self {
        let versioned_socket_basename = concat!("libdd.", env!("CARGO_PKG_VERSION"), ".sock");
        let base_dir = base_dir.as_ref();
        let socket_path = base_dir
            .join(versioned_socket_basename)
            .with_extension(".sock");
        let lock_path = base_dir
            .join(versioned_socket_basename)
            .with_extension(".sock.lock");

        Self {
            socket_path,
            lock_path,
        }
    }

    pub fn new_tmp_dir() -> Self {
        Self::new(env::temp_dir().join("libdatadog"))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        io,
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
    };

    use crate::ipc::{
        platform::{UnixListenerBindAbstract, UnixStreamConnectAbstract},
        setup::Liaison,
    };

    pub struct AbstractUnixSocketLiaison {
        path: PathBuf,
    }

    impl Liaison for AbstractUnixSocketLiaison {
        fn connect_to_server(&self) -> io::Result<UnixStream> {
            UnixStream::connect_abstract(&self.path)
        }

        fn attempt_listen(&self) -> io::Result<Option<UnixListener>> {
            match UnixListener::bind_abstract(&self.path) {
                Ok(l) => Ok(Some(l)),
                Err(ref e) if e.kind() == io::ErrorKind::AddrInUse => Ok(None),
                Err(err) => Err(err),
            }
        }
    }

    impl Default for AbstractUnixSocketLiaison {
        fn default() -> Self {
            let path = PathBuf::from(concat!("libdatadog/", env!("CARGO_PKG_VERSION"), ".sock"));
            Self { path }
        }
    }

    #[test]
    fn test_abstract_socket_can_connect() {
        let l = AbstractUnixSocketLiaison::default();
        super::tests::basic_liaison_connection_test(&l).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};

    use crate::ipc::setup::Liaison;

    #[test]
    fn test_tmp_dir_can_connect_to_socket() -> anyhow::Result<()> {
        let liaison = super::SharedDirLiaison::new_tmp_dir();
        basic_liaison_connection_test(&liaison).unwrap();
        // socket file will still exist - even if we close everything
        assert!(liaison.socket_path.exists());
        Ok(())
    }

    pub fn basic_liaison_connection_test<T>(liaison: &T) -> Result<(), anyhow::Error>
    where
        T: Liaison,
    {
        {
            let listener = liaison.attempt_listen().unwrap().unwrap();
            // can't listen twice when some listener is active
            assert!(liaison.attempt_listen().unwrap().is_none());
            // a liaison can try connecting to existing socket to ensure its valid, adding connection to accept queue
            // but we can drain any preexisting connections in the queue
            listener.set_nonblocking(true).unwrap();
            loop {
                match listener.accept() {
                    Ok(_) => continue,
                    Err(e) => {
                        assert_eq!(io::ErrorKind::WouldBlock, e.kind());
                        break;
                    }
                }
            }
            listener.set_nonblocking(false).unwrap();

            let mut client = liaison.connect_to_server().unwrap();
            let (mut srv, _) = listener.accept().unwrap();
            assert_eq!(1, client.write(&[255]).unwrap());
            let mut buf = [0; 1];
            assert_eq!(1, srv.read(&mut buf).unwrap());
            drop(listener);
            drop(client);
        }

        // we should be able to open new listener now
        let _listener = liaison.attempt_listen().unwrap().unwrap();
        Ok(())
    }
}
