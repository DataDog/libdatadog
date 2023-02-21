use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

use ddtelemetry::ipc::platform::locks::FLock;
use spawn_worker::{assert_child_exit, entrypoint, fork::set_default_child_panic_handler, Stdio};
use tempfile::tempdir;

static ENV_LOCK_PATH: &str = "__LOCK_PATH";

#[no_mangle]
pub extern "C" fn flock_test_entrypoint() {
    set_default_child_panic_handler();
    let lock_path = std::env::var(ENV_LOCK_PATH).unwrap();

    let _l = FLock::try_rw_lock(lock_path).unwrap();
    let mut c: UnixStream = spawn_worker::recv_passed_fd().unwrap().into();

    c.write_all(&[0]).unwrap(); // signal readiness
    let mut buf = [0; 10];
    assert!(c.read(&mut buf).unwrap() > 0); // wait for signal to closepp

    std::process::exit(0); // exit without explicitly freeing
}

#[test]
fn test_file_locking_works_as_expected() {
    let d = tempdir().unwrap();
    let lock_path = d.path().join("file.lock");
    let (mut local, remote) = UnixStream::pair().unwrap();
    local.set_nonblocking(false).ok();

    let child = unsafe { spawn_worker::SpawnWorker::new() }
        .target(entrypoint!(flock_test_entrypoint))
        .pass_fd(remote.try_clone().unwrap())
        .stdin(Stdio::Null)
        .append_env(ENV_LOCK_PATH, lock_path.as_os_str())
        .spawn()
        .unwrap();

    let mut buf = [0; 10];
    local
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    // wait for child to signal its ready
    assert!(local.read(&mut buf).unwrap() > 0);

    // must fail, as file is locked by another process
    let err = FLock::try_rw_lock(&lock_path).err().unwrap();
    assert_eq!(io::ErrorKind::WouldBlock, err.kind());

    local.write_all(&[0]).unwrap(); // signal child to shut down

    assert_child_exit!(child.pid.unwrap());
    assert!(lock_path.exists()); // child exited abbruptly and lock file was left in place

    // must succeed as no other process is holding the lock
    {
        let _lock = FLock::try_rw_lock(&lock_path).unwrap();
        assert!(lock_path.exists());
    }
    // lock file is removed when locked FLock is dropped
    assert!(!lock_path.exists());
}
