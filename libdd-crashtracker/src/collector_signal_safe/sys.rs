// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

use super::Sink;

const MAX_WRITE_RETRIES: u32 = 10;

pub struct FdSink {
    fd: i32,
}

impl FdSink {
    pub fn new(fd: i32) -> Self {
        Self { fd }
    }
}

impl Sink for FdSink {
    fn put(&mut self, bytes: &[u8]) -> bool {
        let mut off = 0usize;
        let mut retries = 0u32;
        while off < bytes.len() {
            let n = write(self.fd, &bytes[off..]);
            if n > 0 {
                off += n as usize;
                retries = 0;
                continue;
            }
            if n == -libc::EINTR as isize && retries < MAX_WRITE_RETRIES {
                retries += 1;
                continue;
            }
            return false;
        }
        true
    }
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod raw {
    use super::*;
    use core::arch::asm;
    use core::ffi::c_void;

    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn syscall0(nr: i64) -> isize {
        let ret: isize;
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack, preserves_flags),
        );
        ret
    }

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
    unsafe fn syscall2(nr: i64, a0: usize, a1: usize) -> isize {
        let ret: isize;
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a0,
            in("rsi") a1,
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
    unsafe fn syscall4(nr: i64, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
        let ret: isize;
        asm!(
            "syscall",
            inlateout("rax") nr as isize => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
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
    unsafe fn syscall0(nr: i64) -> isize {
        let ret: isize;
        asm!(
            "svc 0",
            in("x8") nr,
            lateout("x0") ret,
            options(nostack),
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
    unsafe fn syscall2(nr: i64, a0: usize, a1: usize) -> isize {
        let ret: isize;
        asm!(
            "svc 0",
            in("x8") nr,
            inlateout("x0") a0 => ret,
            in("x1") a1,
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
    unsafe fn syscall4(nr: i64, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
        let ret: isize;
        asm!(
            "svc 0",
            in("x8") nr,
            inlateout("x0") a0 => ret,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
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

    #[inline]
    fn cvt(ret: isize) -> isize {
        if ret < 0 && ret >= -4095 {
            ret
        } else {
            ret
        }
    }

    pub fn write(fd: i32, bytes: &[u8]) -> isize {
        cvt(unsafe {
            syscall3(
                libc::SYS_write,
                fd as usize,
                bytes.as_ptr() as usize,
                bytes.len(),
            )
        })
    }

    pub fn close(fd: i32) {
        unsafe {
            syscall1(libc::SYS_close, fd as usize);
        }
    }

    pub fn dup2(oldfd: i32, newfd: i32) -> i32 {
        if oldfd == newfd {
            return newfd;
        }
        unsafe { syscall3(libc::SYS_dup3, oldfd as usize, newfd as usize, 0) as i32 }
    }

    pub fn fcntl_dupfd(fd: i32, min_fd: i32) -> i32 {
        unsafe {
            syscall3(
                libc::SYS_fcntl,
                fd as usize,
                libc::F_DUPFD as usize,
                min_fd as usize,
            ) as i32
        }
    }

    pub fn pipe(fds: &mut [i32; 2]) -> bool {
        unsafe { syscall2(libc::SYS_pipe2, fds.as_mut_ptr() as usize, 0) == 0 }
    }

    pub fn open_readwrite(path: *const u8) -> i32 {
        unsafe {
            syscall4(
                libc::SYS_openat,
                libc::AT_FDCWD as usize,
                path as usize,
                libc::O_RDWR as usize,
                0,
            ) as i32
        }
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

    pub fn getpid() -> i32 {
        unsafe { syscall0(libc::SYS_getpid) as i32 }
    }

    pub fn gettid() -> i32 {
        unsafe { syscall0(libc::SYS_gettid) as i32 }
    }

    pub fn kill(pid: i32, sig: i32) -> i32 {
        unsafe { syscall2(libc::SYS_kill, pid as usize, sig as usize) as i32 }
    }

    pub fn waitpid_nohang(pid: i32) -> i32 {
        let mut status = 0i32;
        unsafe {
            syscall4(
                libc::SYS_wait4,
                pid as usize,
                (&mut status as *mut i32) as usize,
                libc::WNOHANG as usize,
                0,
            ) as i32
        }
    }

    pub fn poll_sleep_ms(timeout_ms: i32) {
        unsafe {
            let _ = syscall3(libc::SYS_poll, 0, 0, timeout_ms as usize);
        }
    }

    #[repr(C)]
    struct KernelTimespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    pub fn monotonic_nanos() -> i64 {
        let mut ts = KernelTimespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let rc = unsafe {
            syscall2(
                libc::SYS_clock_gettime,
                libc::CLOCK_MONOTONIC as usize,
                (&mut ts as *mut KernelTimespec) as usize,
            )
        };
        if rc != 0 {
            return 0;
        }
        ts.tv_sec
            .wrapping_mul(1_000_000_000)
            .wrapping_add(ts.tv_nsec)
    }

    #[repr(C)]
    struct IoVec {
        iov_base: *mut c_void,
        iov_len: usize,
    }

    pub fn read_own_mem(pid: i32, src: usize, dst: &mut [u8]) -> bool {
        let local = IoVec {
            iov_base: dst.as_mut_ptr() as *mut c_void,
            iov_len: dst.len(),
        };
        let remote = IoVec {
            iov_base: src as *mut c_void,
            iov_len: dst.len(),
        };
        let ret = unsafe {
            syscall6(
                libc::SYS_process_vm_readv,
                pid as usize,
                &local as *const IoVec as usize,
                1,
                &remote as *const IoVec as usize,
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
    pub fn write(fd: i32, bytes: &[u8]) -> isize {
        unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) }
    }

    pub fn close(fd: i32) {
        unsafe {
            let _ = libc::close(fd);
        }
    }

    pub fn dup2(oldfd: i32, newfd: i32) -> i32 {
        unsafe { libc::dup2(oldfd, newfd) }
    }

    pub fn fcntl_dupfd(fd: i32, min_fd: i32) -> i32 {
        unsafe { libc::fcntl(fd, libc::F_DUPFD, min_fd) }
    }

    pub fn pipe(fds: &mut [i32; 2]) -> bool {
        unsafe { libc::pipe(fds.as_mut_ptr()) == 0 }
    }

    pub fn open_readwrite(path: *const u8) -> i32 {
        unsafe { libc::open(path.cast(), libc::O_RDWR) }
    }

    pub unsafe fn fork_raw() -> isize {
        libc::fork() as isize
    }

    pub fn exit_process(code: i32) -> ! {
        unsafe {
            libc::_exit(code);
        }
    }

    pub fn getpid() -> i32 {
        unsafe { libc::getpid() }
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

    pub fn kill(pid: i32, sig: i32) -> i32 {
        unsafe { libc::kill(pid, sig) }
    }

    pub fn waitpid_nohang(pid: i32) -> i32 {
        let mut status = 0i32;
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) }
    }

    pub fn poll_sleep_ms(timeout_ms: i32) {
        unsafe {
            let _ = libc::poll(core::ptr::null_mut(), 0, timeout_ms);
        }
    }

    pub fn monotonic_nanos() -> i64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        if rc != 0 {
            return 0;
        }
        ts.tv_sec
            .wrapping_mul(1_000_000_000)
            .wrapping_add(ts.tv_nsec)
    }

    pub fn read_own_mem(_pid: i32, _src: usize, _dst: &mut [u8]) -> bool {
        false
    }
}

pub use raw::{
    close, dup2, exit_process, fcntl_dupfd, fork_raw, getpid, gettid, kill, monotonic_nanos,
    open_readwrite, pipe, poll_sleep_ms, read_own_mem, waitpid_nohang, write,
};

pub fn errno() -> i32 {
    unsafe { *errno_location() }
}

pub fn set_errno(errno: i32) {
    unsafe {
        *errno_location() = errno;
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn errno_location() -> *mut i32 {
    libc::__errno_location()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe fn errno_location() -> *mut i32 {
    libc::__error()
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))
))]
unsafe fn errno_location() -> *mut i32 {
    libc::__errno_location()
}

#[cfg(test)]
mod tests {
    use super::*;

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
