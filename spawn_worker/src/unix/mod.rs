// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use nix::libc;
use std::{
    ffi::{CStr, CString},
    ptr,
};

pub mod spawn;

// Reexport nix::WaitStatus
pub use nix::sys::wait::WaitStatus;

pub type FnPointer = *const libc::c_void;

/// returns the path of the library from which the symbol pointed to by *addr* was loaded from
///
/// # Safety
/// addr must be a valid address accepted by dladdr(2)
pub unsafe fn get_dl_path_raw(addr: FnPointer) -> (Option<CString>, Option<CString>) {
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
pub unsafe fn fork() -> Result<Fork, std::io::Error> {
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
