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

use crate::ipc::platform::{self, locks::FLock};

/// Implementations of this interface must provide behavior repeatable across processes with the same version
/// of library.
/// Allowing all instances of the same version of the library to establish a shared connection
pub trait Liaison {
    fn connect_to_server(&self) -> io::Result<UnixStream>;
    fn attempt_listen(&self) -> io::Result<Option<UnixListener>>;
}

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

        let _g = match FLock::try_rw_lock(&self.lock_path) {
            Ok(lock) => lock,
            // failing to acquire lock
            // means that another process is creating the socket
            Err(err) => {
                println!("failed_locking");
                return Err(err);
            }
        };

        if self.socket_path.exists() {
            // if socket is already listening, then creating listener is not available
            if platform::sockets::is_listening(&self.socket_path)? {
                println!("already_listening");
                // return Err(io::Error::new(io::ErrorKind::Other, "already listening"));
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

impl Default for SharedDirLiaison {
    fn default() -> Self {
        Self::new_tmp_dir()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        io,
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
    };

    use spawn_worker::getpid;

    use crate::ipc::platform;

    use super::Liaison;

    pub struct AbstractUnixSocketLiaison {
        path: PathBuf,
    }
    pub type DefaultLiason = AbstractUnixSocketLiaison;

    impl Liaison for AbstractUnixSocketLiaison {
        fn connect_to_server(&self) -> io::Result<UnixStream> {
            platform::sockets::connect_abstract(&self.path)
        }

        fn attempt_listen(&self) -> io::Result<Option<UnixListener>> {
            match platform::sockets::bind_abstract(&self.path) {
                Ok(l) => Ok(Some(l)),
                Err(ref e) if e.kind() == io::ErrorKind::AddrInUse => Ok(None),
                Err(err) => Err(err),
            }
        }
    }

    impl AbstractUnixSocketLiaison {
        pub fn ipc_shared() -> Self {
            let path = PathBuf::from(concat!("libdatadog/", env!("CARGO_PKG_VERSION"), ".sock"));
            Self { path }
        }

        pub fn ipc_in_process() -> Self {
            let path = PathBuf::from(format!(
                concat!("libdatadog/", env!("CARGO_PKG_VERSION"), ".{}.sock"),
                getpid()
            ));
            Self { path }
        }
    }

    impl Default for AbstractUnixSocketLiaison {
        fn default() -> Self {
            Self::ipc_shared()
        }
    }

    #[test]
    fn test_abstract_socket_can_connect() {
        let l = AbstractUnixSocketLiaison::ipc_in_process();
        super::tests::basic_liaison_connection_test(&l).unwrap();
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
pub type DefaultLiason = SharedDirLiaison;

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Read, Write},
        thread,
        time::Duration,
    };

    use tempfile::tempdir;

    use super::Liaison;

    #[test]
    fn test_shared_dir_can_connect_to_socket() -> anyhow::Result<()> {
        let tmpdir = tempdir().unwrap();
        let liaison = super::SharedDirLiaison::new(tmpdir.path());
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
        // sleep to give time to OS to free up resources
        thread::sleep(Duration::from_millis(10));

        // we should be able to open new listener now
        let _listener = liaison.attempt_listen().unwrap().unwrap();
        Ok(())
    }
}
