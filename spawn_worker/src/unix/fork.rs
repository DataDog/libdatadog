// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use nix::libc;
pub enum Fork {
    Parent(libc::pid_t),
    Child,
}

/// Forks process into a new standalone process
///
/// # Errors
///
/// This function will return an error if child process can't be forked
///
/// # Safety
///
/// Existing state of the process must allow safe forking, e.g. no background threads should be running
/// as any locks held by these threads will be locked forever
///
/// When forking a multithreaded application, no code should allocate or access other potentially locked resources
/// until call to exec is executed
pub(crate) unsafe fn fork() -> Result<Fork, std::io::Error> {
    let res = libc::fork();
    match res {
        -1 => Err(std::io::Error::last_os_error()),
        0 => Ok(Fork::Child),
        res => Ok(Fork::Parent(res)),
    }
}

/// Runs supplied closure in separate process via fork(2)
///
/// # Safety
///
/// Existing state of the process must allow safe forking, e.g. no background threads should be running
/// as any locks held by these threads will be locked forever
///
/// When forking a multithreaded application, no code should allocate or access other potentially locked resources
/// until call to exec is executed
#[cfg(test)]
unsafe fn fork_fn<Args>(args: Args, f: fn(Args) -> ()) -> Result<libc::pid_t, std::io::Error> {
    match fork()? {
        Fork::Parent(pid) => Ok(pid),
        Fork::Child => {
            f(args);
            std::process::exit(0)
        }
    }
}

/// Sets test panic handler that will ensure exit(1) is called after
/// the original panic handler
pub fn set_default_child_panic_handler() {
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        old_hook(p);
        std::process::exit(1);
    }));
}

#[cfg(test)]
pub mod tests {
    use io_lifetimes::OwnedFd;
    use std::{
        io::{Read, Write},
        os::unix::{
            net::UnixStream,
            prelude::{AsRawFd, FromRawFd},
        },
    };

    use crate::{assert_child_exit, fork::set_default_child_panic_handler, getpid};

    #[test]
    #[ignore]
    fn test_fork_subprocess() {
        let (mut sock_a, sock_b) = UnixStream::pair().unwrap();
        let pid = unsafe {
            super::fork_fn(
                (sock_a.as_raw_fd(), sock_b.as_raw_fd()),
                |(sock_a, sock_b)| {
                    set_default_child_panic_handler();
                    {
                        // Free unused socket
                        OwnedFd::from_raw_fd(sock_a);
                    }
                    let mut sock_b = UnixStream::from_raw_fd(sock_b);

                    sock_b
                        .write_all(format!("child-{}", getpid()).as_bytes())
                        .unwrap();
                },
            )
        }
        .unwrap();
        assert_ne!(pid, getpid());
        unsafe {
            // Free unused socket
            OwnedFd::from_raw_fd(sock_b.as_raw_fd());
        }

        let mut out = String::new();
        sock_a.read_to_string(&mut out).unwrap();

        assert_child_exit!(pid);
        assert_eq!(format!("child-{pid}"), out);
    }

    #[test]
    #[ignore]
    #[cfg(unix)]
    fn test_fork_trigger_error() {
        let pid = unsafe {
            super::fork_fn((), |_| {
                set_default_child_panic_handler();

                // Limit the number of processes the child process tree is able to contain
                rlimit::setrlimit(rlimit::Resource::NPROC, 1, 1).unwrap();
                let err = crate::fork::fork_fn((), |_| {}).unwrap_err();
                assert_eq!(std::io::ErrorKind::WouldBlock, err.kind());
            })
        }
        .unwrap();
        assert_child_exit!(pid);
    }
}
