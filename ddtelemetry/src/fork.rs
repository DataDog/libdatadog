// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
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
pub unsafe fn fork() -> Result<Fork, std::io::Error> {
    let res = libc::fork();
    match res {
        -1 => Err(std::io::Error::last_os_error()),
        0 => Ok(Fork::Child),
        res => Ok(Fork::Parent(res)),
    }
}

/// Forks process and executes the supplied function in a new standalone process
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
pub unsafe fn fork_fn<Args>(args: Args, f: fn(Args) -> ()) -> Result<libc::pid_t, std::io::Error> {
    match fork()? {
        Fork::Parent(pid) => Ok(pid),
        Fork::Child => {
            f(args);
            std::process::exit(0)
        }
    }
}

/// Returns PID of current process
pub fn getpid() -> libc::pid_t {
    unsafe { libc::getpid() }
}

#[cfg(test)]
pub mod tests {
    use io_lifetimes::OwnedFd;
    use lazy_static::lazy_static;
    use std::{
        io::{Read, Write},
        os::unix::{
            net::UnixStream,
            prelude::{AsRawFd, FromRawFd},
        },
        sync::{Mutex, MutexGuard},
    };

    use super::fork_fn;

    /// Sets test panic handler that will ensure exit(1) is called after
    /// the original panic handler
    pub fn set_default_child_panic_handler() {
        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |p| {
            old_hook(p);
            std::process::exit(1);
        }));
    }
    lazy_static! {
        static ref PREVENT_CONCURRENT_TESTS: Mutex<()> = Mutex::new(());
    }

    pub fn prevent_concurrent_tests<'a>() -> MutexGuard<'a, ()> {
        PREVENT_CONCURRENT_TESTS.lock().unwrap()
    }

    #[macro_export]
    macro_rules! assert_child_exit {
        ($pid:expr) => {
            assert_child_exit!($pid, 0)
        };
        ($pid:expr, $expected_exit_code:expr) => {{
            loop {
                match nix::sys::wait::waitpid(Some(nix::unistd::Pid::from_raw($pid)), None).unwrap()
                {
                    nix::sys::wait::WaitStatus::Exited(pid, exit_code) => {
                        if exit_code != $expected_exit_code {
                            panic!(
                                "Child ({}) exited with code {} instead of expected {}",
                                pid, exit_code, $expected_exit_code
                            );
                        }
                        break;
                    }
                    _ => continue,
                }
            }
        }};
    }

    use super::getpid;

    #[test]
    #[ignore]
    fn test_fork_subprocess() {
        let _g = prevent_concurrent_tests();
        let (mut sock_a, sock_b) = UnixStream::pair().unwrap();
        let pid = unsafe {
            fork_fn(
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
        let _g = prevent_concurrent_tests();
        let pid = unsafe {
            fork_fn((), |_| {
                set_default_child_panic_handler();

                // Limit the number of processes the child process tree is able to contain
                rlimit::setrlimit(rlimit::Resource::NPROC, 1, 1).unwrap();
                let err = fork_fn((), |_| {}).unwrap_err();
                assert_eq!(std::io::ErrorKind::WouldBlock, err.kind());
            })
        }
        .unwrap();
        assert_child_exit!(pid);
    }
}
