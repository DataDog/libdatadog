// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

use datadog_ipc::platform::locks::FLock;
use spawn_worker::{assert_child_exit, fork::set_default_child_panic_handler};
use tempfile::tempdir;

static ENV_LOCK_PATH: &str = "__LOCK_PATH";

#[test]
#[cfg_attr(miri, ignore)]
fn test_file_locking_works_as_expected() {
    let d = tempdir().unwrap();
    let lock_path = d.path().join("file.lock");
    let (mut local, remote) = UnixStream::pair().unwrap();
    local.set_nonblocking(false).ok();

    // Fork a child that holds the lock and signals readiness via the socket.
    let child_pid = unsafe {
        match spawn_worker::fork::fork().unwrap() {
            spawn_worker::fork::Fork::Parent(pid) => pid,
            spawn_worker::fork::Fork::Child => {
                set_default_child_panic_handler();
                drop(local);

                let lock_path = std::env::var(ENV_LOCK_PATH)
                    .unwrap_or_else(|_| d.path().join("file.lock").to_str().unwrap().to_string());
                let _l = FLock::try_rw_lock(lock_path).unwrap();
                let mut c: UnixStream = remote.try_clone().unwrap();
                c.write_all(&[0]).unwrap();
                let mut buf = [0; 10];
                assert!(c.read(&mut buf).unwrap() > 0);
                std::process::exit(0);
            }
        }
    };

    // (remote is not used in parent — drop it so the child owns both ends)
    drop(remote);

    let mut buf = [0; 10];
    // give macOS runners on CI more time to read
    #[cfg(target_os = "macos")]
    let read_timeout = Duration::from_secs(10);
    #[cfg(not(target_os = "macos"))]
    let read_timeout = Duration::from_millis(500);
    local.set_read_timeout(Some(read_timeout)).unwrap();
    // wait for child to signal it's ready
    assert!(local.read(&mut buf).unwrap() > 0);

    // must fail, as file is locked by another process
    let err = FLock::try_rw_lock(&lock_path).err().unwrap();
    assert_eq!(io::ErrorKind::WouldBlock, err.kind());

    local.write_all(&[0]).unwrap(); // signal child to shut down

    assert_child_exit!(child_pid);
    assert!(lock_path.exists()); // child exited abruptly and lock file was left in place

    // must succeed as no other process is holding the lock
    {
        let _lock = FLock::try_rw_lock(&lock_path).unwrap();
        assert!(lock_path.exists());
    }
    // lock file is removed when locked FLock is dropped
    assert!(!lock_path.exists());
}
