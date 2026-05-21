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

/// Return the path to the dynamic linker (PT_INTERP) of the current process.
#[cfg(target_os = "linux")]
pub fn read_pt_interp_self() -> Option<PathBuf> {
    // Auxiliary vector entries for the current process's executable PHDRs.
    // SAFETY: getauxval is signal-safe and idempotent.
    let phdr_addr = unsafe { libc::getauxval(libc::AT_PHDR) } as usize;
    let phent = unsafe { libc::getauxval(libc::AT_PHENT) } as usize;
    let phnum = unsafe { libc::getauxval(libc::AT_PHNUM) } as usize;

    if phdr_addr == 0 || phent == 0 || phnum == 0 {
        return None;
    }

    // Walk the in-memory program headers.  We need two passes:
    // 1. Find PT_PHDR to compute the load-base offset (PIE ASLR correction).
    // 2. Find PT_INTERP to get the interpreter path's virtual address.
    let mut load_base: isize = 0;
    let mut interp_vaddr: usize = 0;

    for i in 0..phnum {
        // SAFETY: AT_PHDR + i*phent is within the mapped PHDR table placed by the kernel.
        let ph = (phdr_addr + i * phent) as *const libc::Elf64_Phdr;
        let p_type = unsafe { (*ph).p_type };
        let p_vaddr = unsafe { (*ph).p_vaddr } as usize;

        if p_type == libc::PT_PHDR {
            // load_base = runtime_addr_of_PHDRs − link-time vaddr of PHDRs
            load_base = phdr_addr as isize - p_vaddr as isize;
        }
        if p_type == libc::PT_INTERP {
            interp_vaddr = p_vaddr;
        }
    }

    if interp_vaddr == 0 {
        return None;
    }

    // Compute the runtime address of the null-terminated interpreter path.
    let interp_ptr = load_base.checked_add(interp_vaddr as isize)? as *const libc::c_char;
    // SAFETY: the interpreter path is a valid C string placed by the kernel in the mapped
    // PT_INTERP segment; it is readable for the lifetime of the process.
    let interp = unsafe { CStr::from_ptr(interp_ptr) };
    Some(PathBuf::from(interp.to_string_lossy().as_ref()))
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
