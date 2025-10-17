// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use io_lifetimes::OwnedFd;
use nix::libc;
use std::{
    env,
    ffi::{CStr, CString},
    os::unix::prelude::{FromRawFd, RawFd},
    path::PathBuf,
    ptr,
};

pub mod fork;

mod spawn;
pub use spawn::*;

// Reexport nix::WaitStatus
pub use nix::sys::wait::WaitStatus;

use crate::{Entrypoint, ENV_PASS_FD_KEY};

/// returns the path of the library from which the symbol pointed to by *addr* was loaded from
///
/// # Safety
/// addr must be a valid address accepted by dladdr(2)
pub unsafe fn get_dl_path_raw(addr: *const libc::c_void) -> (Option<CString>, Option<CString>) {
    let mut info = libc::Dl_info {
        dli_fname: ptr::null(),
        dli_fbase: ptr::null_mut(),
        dli_sname: ptr::null(),
        dli_saddr: ptr::null_mut(),
    };
    let res = libc::dladdr(addr, &mut info as *mut libc::Dl_info);

    if res == 0 {
        return (None, None);
    }
    let path_name = if info.dli_fbase.is_null() || info.dli_fname.is_null() {
        None
    } else {
        Some(CStr::from_ptr(info.dli_fname).to_owned())
    };

    let symbol_name = if info.dli_saddr.is_null() || info.dli_sname.is_null() {
        None
    } else {
        Some(CStr::from_ptr(info.dli_sname).to_owned())
    };

    (path_name, symbol_name)
}

/// Returns PID of current process
pub fn getpid() -> libc::pid_t {
    unsafe { libc::getpid() }
}

impl Entrypoint {
    pub fn get_fs_path(&self) -> Option<PathBuf> {
        let (path, _) = unsafe { get_dl_path_raw(self.ptr as *const libc::c_void) };

        Some(PathBuf::from(path?.to_str().ok()?.to_owned()))
    }
}

pub fn recv_passed_fd() -> Option<OwnedFd> {
    let val = env::var(ENV_PASS_FD_KEY).ok()?;
    let fd: RawFd = val.parse().ok()?;

    // check if FD number is valid
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).ok()?;

    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[macro_export]
macro_rules! assert_child_exit {
    ($pid:expr, $expected_exit_code:expr) => {{
        loop {
            match nix::sys::wait::waitpid(Some(nix::unistd::Pid::from_raw($pid)), None).unwrap() {
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
    ($pid:expr) => {
        assert_child_exit!($pid, 0)
    };
}
