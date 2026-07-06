// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(dead_code)]

use core::ffi::c_char;

use super::Sink;

const MAX_WRITE_RETRIES: u32 = 10;
const CSTR_MAX_LEN: usize = 4096;

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
            if n == -(libc::EINTR as isize) && retries < MAX_WRITE_RETRIES {
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
    use core::arch::asm;
    use core::ffi::c_void;
    use rustix::fd::{BorrowedFd, IntoRawFd};

    #[inline]
    fn neg_errno(err: rustix::io::Errno) -> isize {
        -(err.raw_os_error() as isize)
    }

    #[inline]
    unsafe fn borrowed_fd(fd: i32) -> BorrowedFd<'static> {
        BorrowedFd::borrow_raw(fd)
    }

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

    pub fn write(fd: i32, bytes: &[u8]) -> isize {
        match rustix::io::write(unsafe { borrowed_fd(fd) }, bytes) {
            Ok(n) => n as isize,
            Err(err) => neg_errno(err),
        }
    }

    pub fn close(fd: i32) {
        unsafe {
            rustix::io::close(fd);
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

    pub fn fd_valid(fd: i32) -> bool {
        fd >= 0 && unsafe { syscall3(libc::SYS_fcntl, fd as usize, libc::F_GETFD as usize, 0) >= 0 }
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

    pub fn access_executable(path: *const u8) -> bool {
        unsafe {
            syscall3(
                libc::SYS_faccessat,
                libc::AT_FDCWD as usize,
                path as usize,
                libc::X_OK as usize,
            ) == 0
        }
    }

    pub fn mprotect_none(addr: *mut u8, len: usize) -> bool {
        unsafe {
            syscall3(
                libc::SYS_mprotect,
                addr as usize,
                len,
                libc::PROT_NONE as usize,
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

    pub fn getpid() -> i32 {
        rustix::process::getpid().as_raw_pid()
    }

    pub fn gettid() -> i32 {
        rustix::thread::gettid().as_raw_pid()
    }

    pub fn kill(pid: i32, sig: i32) -> i32 {
        unsafe { syscall2(libc::SYS_kill, pid as usize, sig as usize) as i32 }
    }

    pub fn waitpid_nohang_status(pid: i32, status: &mut i32) -> i32 {
        unsafe {
            syscall4(
                libc::SYS_wait4,
                pid as usize,
                (status as *mut i32) as usize,
                libc::WNOHANG as usize,
                0,
            ) as i32
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

    #[repr(C)]
    struct KernelTimespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    pub fn monotonic_nanos() -> i64 {
        let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        ts.tv_sec
            .wrapping_mul(1_000_000_000)
            .wrapping_add(ts.tv_nsec as i64)
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
        let ret = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if ret < 0 {
            -(super::errno() as isize)
        } else {
            ret
        }
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

    pub fn fd_valid(fd: i32) -> bool {
        fd >= 0 && unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
    }

    pub fn close_range_from(_first_fd: i32) -> bool {
        false
    }

    pub fn pipe(fds: &mut [i32; 2]) -> bool {
        unsafe { libc::pipe(fds.as_mut_ptr()) == 0 }
    }

    pub fn open_readwrite(path: *const u8) -> i32 {
        unsafe { libc::open(path.cast(), libc::O_RDWR) }
    }

    pub fn access_executable(path: *const u8) -> bool {
        unsafe { libc::access(path.cast(), libc::X_OK) == 0 }
    }

    pub fn mprotect_none(addr: *mut u8, len: usize) -> bool {
        unsafe { libc::mprotect(addr.cast(), len, libc::PROT_NONE) == 0 }
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

    pub fn waitpid_nohang_status(pid: i32, status: &mut i32) -> i32 {
        let ret = unsafe { libc::waitpid(pid, status, libc::WNOHANG) };
        if ret < 0 {
            -super::errno()
        } else {
            ret
        }
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
    access_executable, close, close_range_from, dup2, exit_process, fcntl_dupfd, fd_valid,
    fork_raw, fork_supported, getpid, gettid, kill, monotonic_nanos, mprotect_none, open_readwrite,
    pipe, poll_sleep_ms, read_own_mem, waitpid_nohang_status, write,
};

pub fn waitpid_nohang(pid: i32) -> i32 {
    let mut status = 0i32;
    waitpid_nohang_status(pid, &mut status)
}

pub fn env_get(name_nul: &[u8]) -> Option<&'static [u8]> {
    if name_nul.is_empty() || name_nul[name_nul.len() - 1] != 0 {
        return None;
    }

    let name = &name_nul[..name_nul.len() - 1];
    let env = unsafe { environ };
    if env.is_null() {
        return None;
    }

    unsafe {
        let mut cur = env;
        while !(*cur).is_null() {
            let entry = *cur;
            if let Some(value) = env_entry_value(entry, name) {
                return Some(cstr_bytes_bounded(value));
            }
            cur = cur.add(1);
        }
    }
    None
}

pub fn environ_ptr() -> *mut *mut c_char {
    unsafe { environ }
}

pub unsafe fn cstr_bytes_bounded<'a>(p: *const c_char) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }

    let mut len = 0usize;
    while len < CSTR_MAX_LEN && *p.add(len) != 0 {
        len += 1;
    }
    core::slice::from_raw_parts(p.cast(), len)
}

pub unsafe fn cstr_starts_with(s: *const c_char, prefix: &[u8]) -> bool {
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

unsafe fn env_entry_value(entry: *const c_char, name: &[u8]) -> Option<*const c_char> {
    let mut i = 0usize;
    while i < name.len() {
        let c = *entry.add(i);
        if c == 0 || c as u8 != name[i] {
            return None;
        }
        i += 1;
    }

    if *entry.add(name.len()) as u8 == b'=' {
        Some(entry.add(name.len() + 1))
    } else {
        None
    }
}

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
