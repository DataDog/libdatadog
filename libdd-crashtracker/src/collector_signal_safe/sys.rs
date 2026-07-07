// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::c_char;

unsafe extern "C" {
    static mut environ: *mut *mut c_char;
}

pub struct FdSink {
    fd: i32,
}

impl FdSink {
    pub fn new(fd: i32) -> Self {
        Self { fd }
    }
}

impl crate::protocol::ByteSink for FdSink {
    type Error = ();

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        let mut off = 0usize;
        while off < bytes.len() {
            let n = write(self.fd, &bytes[off..]);
            if n > 0 {
                off += n as usize;
                continue;
            }
            return Err(());
        }
        Ok(())
    }
}

mod raw_common {
    use core::ffi::CStr;
    use core::num::NonZeroI32;
    use rustix::fd::{BorrowedFd, IntoRawFd};

    #[inline]
    fn neg_errno(err: rustix::io::Errno) -> i32 {
        -err.raw_os_error()
    }

    #[inline]
    pub unsafe fn borrowed_fd(fd: i32) -> BorrowedFd<'static> {
        BorrowedFd::borrow_raw(fd)
    }

    pub fn write(fd: i32, bytes: &[u8]) -> isize {
        match rustix::io::retry_on_intr(|| rustix::io::write(unsafe { borrowed_fd(fd) }, bytes)) {
            Ok(n) => n as isize,
            Err(err) => neg_errno(err) as isize,
        }
    }

    pub fn close(fd: i32) {
        unsafe {
            rustix::io::close(fd);
        }
    }

    pub fn fcntl_dupfd(fd: i32, min_fd: i32) -> i32 {
        match rustix::io::fcntl_dupfd_cloexec(unsafe { borrowed_fd(fd) }, min_fd) {
            Ok(fd) => match rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::empty()) {
                Ok(()) => fd.into_raw_fd(),
                Err(err) => neg_errno(err),
            },
            Err(err) => neg_errno(err),
        }
    }

    pub fn fd_valid(fd: i32) -> bool {
        fd >= 0 && rustix::io::fcntl_getfd(unsafe { borrowed_fd(fd) }).is_ok()
    }

    pub fn pipe(fds: &mut [i32; 2]) -> bool {
        match rustix::pipe::pipe() {
            Ok((read_fd, write_fd)) => {
                fds[0] = read_fd.into_raw_fd();
                fds[1] = write_fd.into_raw_fd();
                true
            }
            Err(_) => false,
        }
    }

    pub fn open_readwrite(path: *const u8) -> i32 {
        let path = unsafe { CStr::from_ptr(path.cast()) };
        match rustix::fs::openat(
            rustix::fs::CWD,
            path,
            rustix::fs::OFlags::RDWR,
            rustix::fs::Mode::empty(),
        ) {
            Ok(fd) => fd.into_raw_fd(),
            Err(err) => neg_errno(err),
        }
    }

    pub fn access_executable(path: *const u8) -> bool {
        let path = unsafe { CStr::from_ptr(path.cast()) };
        rustix::fs::accessat(
            rustix::fs::CWD,
            path,
            rustix::fs::Access::EXEC_OK,
            rustix::fs::AtFlags::empty(),
        )
        .is_ok()
    }

    pub fn mprotect_none(addr: *mut u8, len: usize) -> bool {
        unsafe { rustix::mm::mprotect(addr.cast(), len, rustix::mm::MprotectFlags::empty()) }
            .is_ok()
    }

    pub fn getpid() -> i32 {
        rustix::process::getpid().as_raw_pid()
    }

    pub fn kill(pid: i32, sig: i32) -> i32 {
        let Some(pid) = rustix::process::Pid::from_raw(pid) else {
            return -libc::EINVAL;
        };
        let Some(sig) = NonZeroI32::new(sig) else {
            return -libc::EINVAL;
        };
        let sig = unsafe { rustix::process::Signal::from_raw_nonzero_unchecked(sig) };
        match rustix::process::kill_process(pid, sig) {
            Ok(()) => 0,
            Err(err) => neg_errno(err),
        }
    }

    pub fn waitpid_nohang_status(pid: i32, status: &mut i32) -> i32 {
        let Some(pid) = rustix::process::Pid::from_raw(pid) else {
            return -libc::EINVAL;
        };
        match rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG) {
            Ok(Some((waited, wait_status))) => {
                *status = wait_status.as_raw();
                waited.as_raw_pid()
            }
            Ok(None) => 0,
            Err(err) => neg_errno(err),
        }
    }

    pub fn poll_sleep_ms(timeout_ms: i32) {
        if timeout_ms <= 0 {
            return;
        }
        let ts = rustix::thread::Timespec {
            tv_sec: (timeout_ms / 1000) as i64,
            tv_nsec: ((timeout_ms % 1000) as i64) * 1_000_000,
        };
        let _ = rustix::thread::nanosleep(&ts);
    }

    pub fn monotonic_nanos() -> i64 {
        let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        ts.tv_sec
            .wrapping_mul(1_000_000_000)
            .wrapping_add(ts.tv_nsec as i64)
    }
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod raw {
    use core::arch::asm;
    use core::ffi::c_void;

    pub use super::raw_common::{
        access_executable, close, fcntl_dupfd, fd_valid, getpid, kill, monotonic_nanos,
        mprotect_none, open_readwrite, pipe, poll_sleep_ms, waitpid_nohang_status, write,
    };

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn syscall1(nr: i64, a0: usize) -> isize {
        let ret: isize;
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a0,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
        ret
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn syscall3(nr: i64, a0: usize, a1: usize, a2: usize) -> isize {
        let ret: isize;
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
        ret
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn syscall6(
        nr: i64,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> isize {
        let ret: isize;
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            in("r9") a5,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
        ret
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    unsafe fn syscall1(nr: i64, a0: usize) -> isize {
        let ret: isize;
        asm!(
            "svc 0",
            in("x8") nr,
            inlateout("x0") a0 => ret,
            options(nostack),
        );
        ret
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    unsafe fn syscall3(nr: i64, a0: usize, a1: usize, a2: usize) -> isize {
        let ret: isize;
        asm!(
            "svc 0",
            in("x8") nr,
            inlateout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            options(nostack),
        );
        ret
    }

    #[cfg(target_arch = "aarch64")]
    #[inline]
    unsafe fn syscall6(
        nr: i64,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> isize {
        let ret: isize;
        asm!(
            "svc 0",
            in("x8") nr,
            inlateout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
            in("x4") a4,
            in("x5") a5,
            options(nostack),
        );
        ret
    }

    pub fn dup2(oldfd: i32, newfd: i32) -> i32 {
        if oldfd == newfd {
            return newfd;
        }
        unsafe { syscall3(libc::SYS_dup3, oldfd as usize, newfd as usize, 0) as i32 }
    }

    pub fn close_range_from(first_fd: i32) -> bool {
        first_fd >= 0
            && unsafe {
                syscall3(
                    libc::SYS_close_range,
                    first_fd as usize,
                    u32::MAX as usize,
                    0,
                ) == 0
            }
    }

    pub fn fork_supported() -> bool {
        true
    }

    pub unsafe fn fork_raw() -> isize {
        #[cfg(target_arch = "x86_64")]
        {
            let ret: isize;
            asm!(
                "syscall",
                inlateout("rax") libc::SYS_clone as isize => ret,
                in("rdi") libc::SIGCHLD as usize,
                in("rsi") 0usize,
                in("rdx") 0usize,
                in("r10") 0usize,
                in("r8") 0usize,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack),
            );
            ret
        }
        #[cfg(target_arch = "aarch64")]
        {
            let ret: isize;
            asm!(
                "svc 0",
                in("x8") libc::SYS_clone,
                inlateout("x0") libc::SIGCHLD as usize => ret,
                in("x1") 0usize,
                in("x2") 0usize,
                in("x3") 0usize,
                in("x4") 0usize,
                options(nostack),
            );
            ret
        }
    }

    pub fn exit_process(code: i32) -> ! {
        loop {
            unsafe {
                syscall1(libc::SYS_exit_group, code as usize);
            }
        }
    }

    pub fn gettid() -> i32 {
        rustix::thread::gettid().as_raw_pid()
    }

    pub fn read_own_mem(pid: i32, src: usize, dst: &mut [u8]) -> bool {
        let local = libc::iovec {
            iov_base: dst.as_mut_ptr() as *mut c_void,
            iov_len: dst.len(),
        };
        let remote = libc::iovec {
            iov_base: src as *mut c_void,
            iov_len: dst.len(),
        };
        let ret = unsafe {
            syscall6(
                libc::SYS_process_vm_readv,
                pid as usize,
                &local as *const libc::iovec as usize,
                1,
                &remote as *const libc::iovec as usize,
                1,
                0,
            )
        };
        ret == dst.len() as isize
    }
}

#[cfg(not(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
mod raw {
    pub use super::raw_common::{
        access_executable, close, fcntl_dupfd, fd_valid, getpid, kill, monotonic_nanos,
        mprotect_none, open_readwrite, pipe, poll_sleep_ms, waitpid_nohang_status, write,
    };

    pub fn dup2(oldfd: i32, newfd: i32) -> i32 {
        unsafe { libc::dup2(oldfd, newfd) }
    }

    pub fn close_range_from(_first_fd: i32) -> bool {
        false
    }

    pub fn fork_supported() -> bool {
        false
    }

    pub unsafe fn fork_raw() -> isize {
        -(libc::ENOSYS as isize)
    }

    pub fn exit_process(code: i32) -> ! {
        unsafe {
            libc::_exit(code);
        }
    }

    pub fn gettid() -> i32 {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            let mut tid = 0u64;
            unsafe {
                let _ = libc::pthread_threadid_np(0 as libc::pthread_t, &mut tid);
            }
            tid as i32
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            unsafe { libc::getpid() }
        }
    }

    pub fn read_own_mem(_pid: i32, _src: usize, _dst: &mut [u8]) -> bool {
        false
    }
}

pub use raw::{
    access_executable, close, close_range_from, dup2, exit_process, fcntl_dupfd, fd_valid,
    fork_raw, fork_supported, getpid, gettid, kill, monotonic_nanos, mprotect_none, open_readwrite,
    pipe, poll_sleep_ms, read_own_mem, waitpid_nohang_status, write,
};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ChildReap {
    Reaped(i32),
    NoChild,
    WaitFailed(i32),
    TimedOut,
}

pub fn reap_child(
    pid: i32,
    timeout_ms: i64,
    poll_ms: i32,
    kill_on_timeout: bool,
    kill_timeout_ms: i64,
) -> ChildReap {
    let mut remaining_timeout_ms = timeout_ms;
    let mut should_kill = kill_on_timeout;
    loop {
        match wait_child_until(pid, remaining_timeout_ms, poll_ms) {
            ChildReap::TimedOut if should_kill => {
                let _ = kill(pid, libc::SIGKILL);
                remaining_timeout_ms = kill_timeout_ms;
                should_kill = false;
            }
            result => return result,
        }
    }
}

fn wait_child_until(pid: i32, timeout_ms: i64, poll_ms: i32) -> ChildReap {
    let start = monotonic_nanos();
    loop {
        let mut status = 0i32;
        let waited = waitpid_nohang_status(pid, &mut status);
        if waited == pid {
            return ChildReap::Reaped(status);
        }
        if waited < 0 {
            return if waited == -libc::ECHILD {
                ChildReap::NoChild
            } else {
                ChildReap::WaitFailed(-waited)
            };
        }

        poll_sleep_ms(poll_ms);
        let elapsed_ms = (monotonic_nanos() - start) / 1_000_000;
        if elapsed_ms >= timeout_ms {
            return ChildReap::TimedOut;
        }
    }
}

pub fn environ_ptr() -> *mut *mut c_char {
    unsafe { environ }
}

pub unsafe fn cstr_has_prefix(s: *const c_char, prefix: &[u8]) -> bool {
    let mut i = 0usize;
    while i < prefix.len() {
        let c = *s.add(i);
        if c == 0 || c as u8 != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

pub fn errno() -> i32 {
    unsafe { *errno_location() }
}

pub fn set_errno(errno: i32) {
    unsafe {
        *errno_location() = errno;
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe fn errno_location() -> *mut i32 {
    libc::__error()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
unsafe fn errno_location() -> *mut i32 {
    libc::__errno_location()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector_signal_safe::Sink;

    #[test]
    fn fd_sink_writes_to_pipe() {
        let mut fds = [0i32; 2];
        assert!(pipe(&mut fds));
        let mut sink = FdSink::new(fds[1]);
        assert!(sink.put(b"abc"));
        close(fds[1]);

        let mut out = [0u8; 3];
        let n = unsafe { libc::read(fds[0], out.as_mut_ptr().cast(), out.len()) };
        close(fds[0]);
        assert_eq!(n, 3);
        assert_eq!(&out, b"abc");
    }
}
