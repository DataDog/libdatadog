// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    convert::TryInto,
    ffi::{c_void, CStr},
    mem::ManuallyDrop,
    ptr::{self, NonNull},
    time::Duration,
};
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

/// The shared memory mapped area to publish the context to. The memory region is owned by a
/// [MemMapping] instance and is automatically unmapped upon drop.
///
/// # Safety
///
/// The following invariants MUST always hold for safety and are guaranteed by [MemMapping]:
/// - when `only_for_pid` allows access from the current process, `start_addr` points to a live
///   mapping of `mapping_size()` bytes created by `mmap`.
/// - once `self` has been dropped, no memory access must be performed on the memory previously
///   pointed to by `start_addr`.
pub(super) struct MemMapping {
    start_addr: NonNull<c_void>,
    /// `Some(pid)` when `MADV_DONTFORK` succeeded, otherwise `None`.
    only_for_pid: Option<u32>,
}

// SAFETY: MemMapping represents ownership over the mapped region. It never leaks or
// share the internal pointer. It's also safe to drop (`munmap`) from a different thread.
unsafe impl Send for MemMapping {}

impl super::HeaderMemoryHolder for MemMapping {
    fn new() -> io::Result<Self> {
        Self::new()
    }

    fn as_ptr(&self) -> Option<NonNull<super::MappingHeader>> {
        if self
            .only_for_pid
            .is_some_and(|pid| pid != std::process::id())
        {
            None
        } else {
            Some(self.start_addr.cast())
        }
    }

    fn make_discoverable(&mut self) {
        // Naming must be attempted even when it is expected to fail. A naming failure does not
        // invalidate the publication protocol.
        let _ = self.set_name();
    }

    fn unpublish_and_release(self) -> io::Result<()> {
        self.free()
    }

    fn after_fork(self) {
        drop(self);
    }
}

pub(super) struct MonotonicClock;

impl super::MonotonicTime for MonotonicClock {
    fn monotonic_time_ns() -> io::Result<u64> {
        since_boottime_ns().ok_or_else(|| {
            io::Error::other("failed to get current time for process context publication")
        })
    }
}

impl MemMapping {
    /// Creates a suitable memory mapping for the context protocol to be published.
    ///
    /// `memfd` is the preferred method, but this function fallbacks to an anonymous mapping if
    /// `memfd` failed for any reason.
    ///
    /// Both allocation paths produce zero-filled memory: `MAP_ANONYMOUS` mappings are
    /// initialized to zero, and the memfd path maps a newly-created file extended by
    /// `ftruncate()`, whose extended bytes read as `\0`. This matters because a memfd-backed
    /// mapping is discoverable before `set_name()` runs, so early readers may race with header
    /// initialization. They must observe the unpublished/updating sentinel (0) and stop until the
    /// final timestamp store publishes the initialized header.
    fn new() -> io::Result<Self> {
        let size = super::mapping_size();

        let mut mapping = try_memfd(crate::otel_process_ctx::linux::MAPPING_NAME, libc::MFD_CLOEXEC | libc::MFD_NOEXEC_SEAL | libc::MFD_ALLOW_SEALING)
            .or_else(|_| try_memfd(crate::otel_process_ctx::linux::MAPPING_NAME, libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING))
            .and_then(|fd| {
                // SAFETY: fd is a valid open file descriptor.
                check_syscall_retval(
                    unsafe {
                        libc::ftruncate(fd.as_raw_fd(), super::mapping_size() as libc::off_t)
                    },
                    "ftruncate failed"
                )?;
                // SAFETY: we pass a null pointer to mmap which is unconditionally ok
                let start_addr = check_mapping_addr(
                    unsafe {
                        libc::mmap(
                            ptr::null_mut(),
                            size,
                            libc::PROT_WRITE | libc::PROT_READ,
                            libc::MAP_PRIVATE,
                            fd.as_raw_fd(),
                            0,
                        )
                    },
                    "mmap failed"
                )?;

                // We (implicitly) close the file descriptor right away, but this ok
                Ok(MemMapping {
                    start_addr,
                    only_for_pid: None,
                })
            })
            // If any previous step failed, we fallback to an anonymous mapping
            .or_else(|_| {
                // SAFETY: we pass a null pointer to mmap, no precondition to uphold
                let start_addr = check_mapping_addr(
                    unsafe {
                        libc::mmap(
                            ptr::null_mut(),
                            size,
                            libc::PROT_WRITE | libc::PROT_READ,
                            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                            -1,
                            0,
                        )
                    },
                    "mmap failed: couldn't create a memfd or anonymous mmapped region for process context publication"
                )?;

                Ok::<_, io::Error>(MemMapping {
                    start_addr,
                    only_for_pid: None,
                })
            })?;

        // SAFETY: MemMapping owns a live mapping of mapping_size() bytes. Failure is harmless;
        // the mapping then follows the default inheritance behavior.
        mapping.only_for_pid = (unsafe {
            libc::madvise(
                mapping.start_addr.as_ptr(),
                super::mapping_size(),
                libc::MADV_DONTFORK,
            )
        } == 0)
            .then_some(std::process::id());

        Ok(mapping)
    }

    /// Makes this mapping discoverable by giving it a name.
    fn set_name(&mut self) -> io::Result<()> {
        // SAFETY: self.start_addr is valid for mapping_size() bytes as per MemMapping
        // invariants. name is a valid NUL-terminated string that outlives the prctl call.
        check_syscall_retval(
            unsafe {
                // int prctl(PR_SET_VMA, long attr, unsigned long addr, unsigned long size,
                // const char *_Nullable val);
                libc::prctl(
                    libc::PR_SET_VMA,
                    libc::PR_SET_VMA_ANON_NAME as libc::c_ulong,
                    TryInto::<libc::c_ulong>::try_into(self.start_addr.as_ptr() as usize)
                        .expect("start addr overflowed"),
                    TryInto::<libc::c_ulong>::try_into(super::mapping_size())
                        .expect("mapping size overflowed"),
                    crate::otel_process_ctx::linux::MAPPING_NAME.as_ptr(),
                )
            },
            "prctl PR_SET_VMA_ANON_NAME failed",
        )?;

        Ok(())
    }

    /// Unmaps the underlying memory region. This has same effect as dropping `self`, but
    /// propagates potential errors.
    fn free(mut self) -> io::Result<()> {
        // SAFETY: We put `self` in a `ManuallyDrop`, which prevents drop and future calls to
        // `free()`.
        unsafe {
            self.unmap()?;
        }

        // Prevent `Self::drop` from being called
        let _ = ManuallyDrop::new(self);

        Ok(())
    }

    /// Unmaps the underlying memory region. For internal use only; prefer `free()` or `drop()`.
    ///
    /// # Safety
    ///
    /// This method must only be called once. After calling `unmap()`, no other method of
    /// `MemMapping` must be ever called on `self` again, including `unmap()` and `drop()`.
    ///
    /// Practically, `self` must be put in a `ManuallyDrop` wrapper and forgotten, or being in
    /// the process of being dropped.
    unsafe fn unmap(&mut self) -> io::Result<()> {
        if self
            .only_for_pid
            .is_some_and(|pid| pid != std::process::id())
        {
            return Ok(());
        }

        check_syscall_retval(
            // SAFETY: upheld by the caller.
            unsafe { libc::munmap(self.start_addr.as_ptr(), super::mapping_size()) },
            "munmap failed when freeing the process context",
        )?;

        Ok(())
    }
}

impl Drop for MemMapping {
    fn drop(&mut self) {
        // SAFETY: `self` is being dropped
        let _ = unsafe { self.unmap() };
    }
}

/// Returns `Err` wrapping the current `errno` with `msg` as context if `ret` is negative,
/// `Ok(ret)` otherwise.
fn check_syscall_retval(ret: libc::c_int, msg: &'static str) -> io::Result<libc::c_int> {
    if ret < 0 {
        let e = io::Error::last_os_error();
        Err(io::Error::new(e.kind(), format!("{msg}: {e}")))
    } else {
        Ok(ret)
    }
}

/// Returns `Err` wrapping the current `errno` with `msg` as context if `addr` equals
/// `MAP_FAILED`, `Ok(addr)` otherwise.
fn check_mapping_addr(addr: *mut c_void, msg: &'static str) -> io::Result<NonNull<c_void>> {
    if addr == libc::MAP_FAILED {
        let e = io::Error::last_os_error();
        Err(io::Error::new(e.kind(), format!("{msg}: {e}")))
    } else {
        // SAFETY: mmap returns a non-null pointer on success.
        Ok(unsafe { NonNull::new_unchecked(addr) })
    }
}

/// Creates a `memfd` file descriptor with the given name and flags.
fn try_memfd(name: &CStr, flags: libc::c_uint) -> io::Result<OwnedFd> {
    // We use the raw syscall rather than `libc::memfd_create` because the latter requires
    // glibc >= 2.27, while `syscall()` + `SYS_memfd_create` works with any glibc version.
    check_syscall_retval(
        // SAFETY: name is a valid NUL-terminated string; flags are constant bit flags.
        unsafe {
            libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags as libc::c_long)
                as libc::c_int
        },
        "memfd_create failed",
    )
    // SAFETY: fd is a valid file descriptor just returned by memfd_create.
    .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Returns the value of the monotonic BOOTTIME clock in nanoseconds.
fn since_boottime_ns() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid, writable timespec.
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    if ret != 0 {
        return None;
    }
    let secs: u64 = ts.tv_sec.try_into().ok()?;
    let nanos: u32 = ts.tv_nsec.try_into().ok()?;
    let duration = Duration::new(secs, nanos);
    u64::try_from(duration.as_nanos()).ok()
}
