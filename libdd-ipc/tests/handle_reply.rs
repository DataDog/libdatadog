#![cfg(unix)]
// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_ipc::SeqpacketConn;
use libdd_ipc::platform::{FileBackedHandle, ShmHandle};

#[test]
#[cfg(unix)]
#[cfg_attr(miri, ignore = "requires native IPC, shared memory, and exec")]
fn returned_handle_does_not_survive_exec() {
    use libdd_ipc::platform::{OwnedFileHandle, PlatformHandle};
    use std::os::fd::AsRawFd;
    let (server, client) = SeqpacketConn::socketpair().unwrap();
    let handle = ShmHandle::new(8192).unwrap();
    let mut original = handle.clone().map().unwrap();
    original.as_slice_mut()[8000] = 42;
    let sent: PlatformHandle<OwnedFileHandle> = handle.into();
    server
        .send_raw_blocking(vec![1], &[sent.as_raw_fd()])
        .unwrap();
    let mut buf = [0; 1];
    let (_, mut received) = client.recv_raw_blocking(&mut buf).unwrap();
    let returned = received.pop().unwrap();
    let descriptor = returned;
    let fd = descriptor.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0);
    let inode = nix::sys::stat::fstat(fd).unwrap().st_ino;
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "exec_mapping_child", "--nocapture"])
        .env("DD_IPC_EXEC_FD", fd.to_string())
        .env("DD_IPC_EXEC_INODE", inode.to_string())
        .output()
        .unwrap();
    let observed = result.status.code().expect("child exited normally");
    assert!(
        matches!(observed, 0 | 42),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    eprintln!(
        "returned_fd_flags={flags}; exec_exit={observed}; parent_marker={}",
        original.as_slice()[8000]
    );
    assert_eq!(observed, 0, "returned mapping leaked through exec");
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    assert_eq!(original.as_slice()[8000], 42);
}

#[test]
#[cfg(unix)]
#[cfg_attr(miri, ignore = "requires native descriptor access after exec")]
fn exec_mapping_child() {
    let Ok(fd) = std::env::var("DD_IPC_EXEC_FD") else {
        return;
    };
    let fd: libc::c_int = fd.parse().unwrap();
    let inode: libc::ino_t = std::env::var("DD_IPC_EXEC_INODE").unwrap().parse().unwrap();
    if nix::sys::stat::fstat(fd).is_ok_and(|stat| stat.st_ino == inode) {
        let mut byte: u8 = 0;
        // SAFETY: the child owns this inherited descriptor and byte is live.
        assert_eq!(
            unsafe { libc::pread(fd, (&mut byte as *mut u8).cast(), 1, 8000) },
            1
        );
        assert_eq!(byte, 42);
        byte = 43;
        // SAFETY: as above, with a valid initialized byte as the write source.
        assert_eq!(
            unsafe { libc::pwrite(fd, (&byte as *const u8).cast(), 1, 8000) },
            1
        );
        std::process::exit(42);
    }
}
