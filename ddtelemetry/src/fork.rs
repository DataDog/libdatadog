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
pub unsafe fn fork_fn<F>(mut f: F) -> Result<libc::pid_t, std::io::Error>
where
    F: FnMut(),
{
    match fork()? {
        Fork::Parent(pid) => Ok(pid),
        Fork::Child => {
            f();
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
    use std::{
        io::{Read, Write},
        os::unix::{
            net::UnixStream,
            prelude::{AsRawFd, FromRawFd},
        },
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
    fn test_fork_subprocess() {
        let (mut sock_a, mut sock_b) = UnixStream::pair().unwrap();
        let pid = unsafe {
            fork_fn(|| {
                set_default_child_panic_handler();
                {
                    // Free unused socket
                    OwnedFd::from_raw_fd(sock_a.as_raw_fd());
                }
                println!("CJK");

                sock_b
                    .write_all(format!("child-{}", getpid()).as_bytes())
                    .unwrap();

                println!("CJK 2");
            })
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
        assert_eq!(format!("child-{}", pid), out);
    }

    #[test]
    #[cfg(unix)]
    fn test_fork_trigger_error() {
        let pid = unsafe {
            fork_fn(|| {
                set_default_child_panic_handler();

                // Limit the number of processes the child process tree is able to contain
                rlimit::setrlimit(rlimit::Resource::NPROC, 1, 1).unwrap();
                let err = fork_fn(|| {}).unwrap_err();
                assert_eq!(std::io::ErrorKind::WouldBlock, err.kind());
            })
        }
        .unwrap();
        assert_child_exit!(pid);
    }
}
