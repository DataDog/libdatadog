// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use std::os::unix::net::{UnixDatagram, UnixStream};

pub enum Fork {
    Parent(libc::pid_t),
    Child,
}

pub trait ForkSafe {}

/// Forkable is meant to hold instances considered ForkSafe
///
#[derive(Clone)]
#[repr(transparent)]
pub struct Forkable<T: ?Sized + 'static> {
    inner: T,
}

impl<T: ?Sized> ForkSafe for Forkable<T> {}

impl<T> Forkable<T> {
    pub fn mark_as(inner: T) -> Self {
        Self { inner }
    }
    pub fn take(self) -> T {
        self.inner
    }
}

impl<T: ?Sized> std::ops::Deref for Forkable<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

macro_rules! fork_safe {
    ($($I:ident),*) => {
        $(
            impl ForkSafe for $I {}
        )*
    }
}

// Mark types known to be fork safe
fork_safe!(UnixDatagram, UnixStream);
fork_safe!(u8, u16, u32, u64, u128, i8, i16, i32, i64);

macro_rules! impl_forksafe_for_tuple {
    () => { impl ForkSafe for () {} };
    ($A:ident $($I:ident)*) => {
        impl_forksafe_for_tuple!($($I)*);

        impl<$A : ForkSafe, $($I: ForkSafe),*> ForkSafe for ($A, $($I),*) {}
    }
}

impl<T: ForkSafe> ForkSafe for Option<T> {}

// Implement tuples "auto_trait" ForkSafe for *all tuples that contain only ForkSafe values
impl_forksafe_for_tuple!(A B C D E F G H I J K L M);

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

/// Forks supplied funciton pointer into a new process returning Pid of the process and call exit(0) once child function pointers
/// stops executing
///
/// Function pointer is used to prevent capturing values in a closure, and all Args must be marked as ForkSafe
///
/// Marker ForkSafe should only be applied to types and traits that do not rely on global state
/// especially Mutexes shared with already running threads must be avoided
///
/// # Errors
/// function will return an Error if
///
/// This function will return an error if .
pub fn safer_fork<Args>(args: Args, f: fn(Args) -> ()) -> Result<libc::pid_t, std::io::Error>
where
    Args: ForkSafe,
{
    match unsafe { fork() }? {
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
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
    };

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
            match nix::sys::wait::waitpid(Some(nix::unistd::Pid::from_raw($pid)), None).unwrap() {
                nix::sys::wait::WaitStatus::Exited(pid, exit_code) => {
                    if exit_code != 0 {
                        panic!(
                            "Child ({}) exited with code {} instead of expected {}",
                            pid, exit_code, $expected_exit_code
                        );
                    }
                }
                other => {
                    panic!("unsupported child status {:?}", other);
                }
            }
        }};
    }

    use crate::fork::{getpid, Forkable};

    use super::safer_fork;

    #[test]
    fn test_fork_subprocess() {
        let (mut sock_a, sock_b) = UnixStream::pair().unwrap();
        let pid = safer_fork(sock_b, |mut sock_b| {
            set_default_child_panic_handler();

            sock_b
                .write_all(format!("child-{}", getpid()).as_bytes())
                .unwrap();
        })
        .unwrap();
        assert_ne!(pid, getpid());

        let mut out = String::new();
        sock_a.read_to_string(&mut out).unwrap();

        assert_child_exit!(pid);
        assert_eq!(format!("child-{}", pid), out);
    }

    #[test]
    fn test_fork_subprocess_tuple_arg() {
        let pid = safer_fork((1, Forkable::mark_as(1)), |(a, b)| {
            set_default_child_panic_handler();

            assert_eq!(a, b.take());
        })
        .unwrap();
        assert_child_exit!(pid);
    }

    #[test]
    #[cfg(unix)]
    fn test_fork_trigger_error() {
        let pid = safer_fork((), |_| {
            set_default_child_panic_handler();

            // Limit the number of processes the child process tree is able to contain
            rlimit::setrlimit(rlimit::Resource::NPROC, 1, 1).unwrap();
            let err = safer_fork((), |_| {}).unwrap_err();
            assert_eq!(std::io::ErrorKind::WouldBlock, err.kind());
        })
        .unwrap();
        assert_child_exit!(pid);
    }
}
