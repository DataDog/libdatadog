// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

mod helper {
    use nix::libc;
    use std::{ffi::CString, ptr};

    pub struct ExecVec {
        items: Vec<CString>,
        // Always NULL ptr terminated
        ptrs: Vec<*const libc::c_char>,
    }

    impl ExecVec {
        pub fn as_ptr(&self) -> *const *const libc::c_char {
            self.ptrs.as_ptr()
        }

        pub fn empty() -> Self {
            Self {
                items: vec![],
                ptrs: vec![ptr::null()],
            }
        }

        pub fn push(&mut self, item: CString) {
            let l = self.ptrs.len();
            // replace previous trailing null with ptr to the item
            self.ptrs[l - 1] = item.as_ptr();
            self.ptrs.push(ptr::null());
            self.items.push(item);
        }
    }
}

use std::{
    ffi::{self, CString, OsString},
    fs::File,
    ops::RangeInclusive,
    os::unix::{ffi::OsStringExt, prelude::AsRawFd},
};

use io_lifetimes::OwnedFd;

use nix::{sys::wait::WaitStatus, unistd::Pid};

use crate::fork::{fork, Fork};
use nix::libc;

use crate::unix::spawn::helper::ExecVec;

enum ChildStdio {
    Inherit,
    Owned(OwnedFd),
    Ref(libc::pid_t),
}

impl ChildStdio {
    fn as_fd(&self) -> Option<libc::pid_t> {
        match self {
            ChildStdio::Inherit => None,
            ChildStdio::Owned(fd) => Some(fd.as_raw_fd()),
            ChildStdio::Ref(fd) => Some(*fd),
        }
    }
}

pub enum Stdio {
    Inherit,
    Fd(OwnedFd),
    Null,
}

impl Stdio {
    fn as_child_stdio(&self) -> std::io::Result<ChildStdio> {
        match self {
            Stdio::Inherit => Ok(ChildStdio::Inherit),
            Stdio::Fd(fd) => {
                if fd.as_raw_fd() >= 0 && fd.as_raw_fd() <= libc::STDERR_FILENO {
                    Ok(ChildStdio::Owned(fd.try_clone()?))
                } else {
                    Ok(ChildStdio::Ref(fd.as_raw_fd()))
                }
            }
            Stdio::Null => {
                let dev_null = File::options().read(true).write(true).open("/dev/null")?;
                Ok(ChildStdio::Owned(dev_null.into()))
            }
        }
    }
}

impl From<&File> for Stdio {
    fn from(val: &File) -> Self {
        Stdio::Fd(val.try_clone().unwrap().into())
    }
}

pub struct Child {
    pub pid: Option<libc::pid_t>,
}

impl Child {
    pub fn wait(self) -> anyhow::Result<WaitStatus> {
        let pid = match self.pid {
            Some(pid) => Pid::from_raw(pid),
            None => return Ok(WaitStatus::Exited(Pid::from_raw(0), 0)),
        };

        Ok(nix::sys::wait::waitpid(Some(pid), None)?)
    }
}

/// Spawn a standalone binary as a double-forked daemon, passing `fd_to_pass` via the
/// `__DD_INTERNAL_PASSED_FD` environment variable (dup'd to fd 3 in the child).
///
/// The binary is exec'd directly without any trampoline.  All configuration is
/// passed via `env`.
///
/// # Safety
/// Must be called while no other threads are running (or at least while no threads hold
/// locks that would deadlock after fork).
pub fn spawn_exec_binary(
    binary_path: &std::path::Path,
    subcommand: &str,
    env: &[(ffi::OsString, ffi::OsString)],
    fd_to_pass: OwnedFd,
    stdout: Stdio,
    stderr: Stdio,
) -> anyhow::Result<Child> {
    let binary_cstr = CString::new(binary_path.to_string_lossy().as_bytes())
        .map_err(|e| anyhow::format_err!("binary path contains NUL: {e}"))?;

    // argv[0] = binary name, argv[1] = subcommand
    let mut argv = ExecVec::empty();
    let name = binary_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("datadog-ipc-helper");
    argv.push(CString::new(name)?);
    argv.push(CString::new(subcommand)?);

    // Build envp from the caller-supplied list
    let mut envp = ExecVec::empty();
    for (k, v) in env {
        let mut entry = OsString::with_capacity(k.len() + v.len() + 2);
        entry.push(k);
        entry.push("=");
        entry.push(v);
        if let Ok(cs) = CString::new(entry.into_vec()) {
            envp.push(cs);
        }
    }
    // Tell the child which fd to use for the IPC socket
    envp.push(CString::new(format!("{}=3", crate::ENV_PASS_FD_KEY))?);

    // Clone fd and strip close-on-exec so it survives the exec
    let fd_clone = fd_to_pass
        .try_clone()
        .map_err(|e| anyhow::format_err!("failed to clone fd: {e}"))?;
    let flags = nix::fcntl::fcntl(fd_clone.as_raw_fd(), nix::fcntl::F_GETFD)?;
    unsafe {
        libc::fcntl(
            fd_clone.as_raw_fd(),
            libc::F_SETFD,
            flags & !nix::libc::FD_CLOEXEC,
        )
    };

    // Resolve stdio before the fork so we don't allocate after it
    let stdout_child = stdout.as_child_stdio()?;
    let stderr_child = stderr.as_child_stdio()?;
    let stdin_null = Stdio::Null.as_child_stdio()?;

    // --- First fork: parent returns; child becomes the intermediate process ---
    if let Fork::Parent(child_pid) = unsafe { fork()? } {
        return Ok(Child { pid: Some(child_pid) });
    }

    // ---- Intermediate child ----

    if let Some(fd) = stdin_null.as_fd() {
        unsafe { libc::dup2(fd, libc::STDIN_FILENO) };
    }
    if let Some(fd) = stdout_child.as_fd() {
        unsafe { libc::dup2(fd, libc::STDOUT_FILENO) };
    }
    if let Some(fd) = stderr_child.as_fd() {
        unsafe { libc::dup2(fd, libc::STDERR_FILENO) };
    }

    // Move the IPC socket to fd 3
    unsafe { libc::dup2(fd_clone.as_raw_fd(), 3) };

    // Close all other descriptors
    let _ = close_fd_range(4..=i32::MAX, -1);

    // --- Second fork: daemonize.  No `?` here — we're in the child. ---
    match unsafe { fork() } {
        Ok(Fork::Parent(_)) => {
            // musl atexit handlers can do bad things with open file descriptors
            for i in 0..4 {
                unsafe { libc::close(i) };
            }
            unsafe { libc::_exit(0) };
        }
        Ok(Fork::Child) => {}
        Err(_) => unsafe { libc::_exit(1) },
    }

    // ---- Grandchild: exec the sidecar binary ----
    unsafe {
        libc::execve(binary_cstr.as_ptr(), argv.as_ptr(), envp.as_ptr());
        // execve only returns on failure
        libc::_exit(1);
    }
}

#[cfg(target_os = "macos")]
const SELF_FD_DIR: &str = "/dev/fd";

#[cfg(target_os = "linux")]
const SELF_FD_DIR: &str = "/proc/self/fd";

fn list_open_fds() -> std::io::Result<impl Iterator<Item = i32>> {
    let dir = nix::dir::Dir::open(
        SELF_FD_DIR,
        nix::fcntl::OFlag::O_DIRECTORY | nix::fcntl::OFlag::O_RDONLY,
        nix::sys::stat::Mode::empty(),
    )?;
    let dir_fd = dir.as_raw_fd();
    Ok(dir.into_iter().filter_map(move |fd_path| {
        let fd_path = match fd_path {
            Err(_) => return None,
            Ok(p) => p,
        };
        if fd_path.file_name().to_bytes() == b"." || fd_path.file_name().to_bytes() == b".." {
            return None;
        }
        let fd: i32 = std::str::from_utf8(fd_path.file_name().to_bytes())
            .unwrap()
            .parse()
            .unwrap();
        if fd == dir_fd {
            return None;
        }
        Some(fd)
    }))
}

// https://man7.org/linux/man-pages/man2/close_range.2.html is too recent sadly :_()
fn close_fd_range(range: RangeInclusive<i32>, skip_close_fd: i32) -> std::io::Result<()> {
    for fd in list_open_fds()? {
        if fd != skip_close_fd && range.contains(&fd) {
            nix::unistd::close(fd)?;
        }
    }
    Ok(())
}
