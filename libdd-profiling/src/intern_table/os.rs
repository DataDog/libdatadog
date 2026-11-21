// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Virtual memory for intern tables: one reservation per table.
//!
//! | | Unix | Windows |
//! |---|---|---|
//! | reserve | `mmap(MAP_SHARED \| MAP_ANON)`, plus `MAP_NORESERVE` on Linux | `VirtualAlloc(MEM_RESERVE)` |
//! | commit | nothing: pages fault in on first touch | `VirtualAlloc(MEM_COMMIT)` |
//! | populate | `MADV_POPULATE_WRITE` on Linux, else touch each page | nothing: commit already charged it |
//! | release | `munmap` | `VirtualFree(MEM_RELEASE)` |
//!
//! Unix mappings are shared so that children forked after creation see later
//! inserts. Miri cannot model shared mappings, so it gets a private one.
//!
//! Each table also has one small private, committed mapping for its
//! process-local state ([`map_private`]); a forked child gets a copy-on-write
//! copy of it.

pub use imp::{map_private, page_size, release, reserve};

/// Makes `len` bytes at `start` usable, so writing them cannot fail later.
///
/// # Safety
///
/// The range lies within a live reservation.
#[allow(
    clippy::missing_const_for_fn,
    reason = "const only on Unix; on Windows it is a system call"
)]
pub unsafe fn commit(start: core::ptr::NonNull<u8>, len: usize) -> bool {
    // SAFETY: forwarded.
    len == 0 || unsafe { imp::commit(start, len) }
}

/// Commits `len` bytes at `start` and faults them in now, so the first
/// writes under the lock do not each take a page fault. A failure here means
/// the memory is not available, reported instead of a later fault.
///
/// # Safety
///
/// The range lies within a live reservation, is page-aligned, and holds only
/// zero bytes that no one else is using: populating may write zeros.
pub unsafe fn populate(start: core::ptr::NonNull<u8>, len: usize, page: usize) -> bool {
    // SAFETY: forwarded.
    unsafe { commit(start, len) && imp::populate(start, len, page) }
}

/// Writes a zero to each page in the range.
///
/// # Safety
///
/// As for [`populate`].
#[cfg_attr(windows, allow(dead_code, reason = "commit already charges pages"))]
unsafe fn touch(start: core::ptr::NonNull<u8>, len: usize, page: usize) {
    let mut offset = 0;
    while offset < len {
        // SAFETY: within the range, which holds zeros no one else uses.
        unsafe { start.add(offset).write_volatile(0) };
        offset = offset.saturating_add(page);
    }
}

#[cfg(unix)]
pub fn errno() -> libc::c_int {
    // SAFETY: libc returns a valid pointer to this thread's errno.
    unsafe {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let errno = *libc::__errno_location();
        #[cfg(target_vendor = "apple")]
        let errno = *libc::__error();
        errno
    }
}

#[cfg(unix)]
mod imp {
    use core::ptr::{self, NonNull};

    pub fn page_size() -> Option<usize> {
        // SAFETY: sysconf has no pointer arguments.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(size).ok().filter(|&size| size > 0)
    }

    pub fn reserve(len: usize) -> Option<NonNull<u8>> {
        #[cfg(miri)]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
        // Linux would otherwise charge the whole reservation against the
        // commit limit up front; most of it is address space never touched.
        #[cfg(all(not(miri), any(target_os = "linux", target_os = "android")))]
        let flags = libc::MAP_SHARED | libc::MAP_ANON | libc::MAP_NORESERVE;
        #[cfg(all(not(miri), not(any(target_os = "linux", target_os = "android"))))]
        let flags = libc::MAP_SHARED | libc::MAP_ANON;
        // SAFETY: a new anonymous mapping replaces nothing; the fd is ignored.
        let start = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                -1,
                0,
            )
        };
        if start == libc::MAP_FAILED {
            return None;
        }
        NonNull::new(start.cast())
    }

    pub fn map_private(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: a new anonymous mapping replaces nothing; the fd is ignored.
        let start = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if start == libc::MAP_FAILED {
            return None;
        }
        NonNull::new(start.cast())
    }

    pub const unsafe fn commit(_: NonNull<u8>, _: usize) -> bool {
        true
    }

    pub unsafe fn populate(start: NonNull<u8>, len: usize, page: usize) -> bool {
        #[cfg(all(not(miri), any(target_os = "linux", target_os = "android")))]
        {
            // SAFETY: the range is page-aligned and within the reservation.
            let result =
                unsafe { libc::madvise(start.as_ptr().cast(), len, libc::MADV_POPULATE_WRITE) };
            if result == 0 {
                return true;
            }
            // Kernels before 5.14 do not know the advice: touch instead.
            if super::errno() != libc::EINVAL {
                return false;
            }
        }
        // SAFETY: forwarded from `populate`.
        unsafe { super::touch(start, len, page) };
        true
    }

    /// # Safety
    ///
    /// The reservation is no longer used.
    pub unsafe fn release(start: NonNull<u8>, len: usize) {
        // SAFETY: the caller hands over the whole reservation. Failure leaves
        // address space mapped, which nothing can repair.
        unsafe { libc::munmap(start.as_ptr().cast(), len) };
    }
}

#[cfg(windows)]
mod imp {
    use core::ffi::c_void;
    use core::ptr::{self, NonNull};

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_READWRITE: u32 = 0x04;

    #[repr(C)]
    struct SystemInfo {
        architecture: u32,
        page_size: u32,
        minimum_address: *mut c_void,
        maximum_address: *mut c_void,
        processor_mask: usize,
        processors: u32,
        processor_type: u32,
        allocation_granularity: u32,
        processor_level: u16,
        processor_revision: u16,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemInfo(info: *mut SystemInfo);
        fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32;
    }

    pub fn page_size() -> Option<usize> {
        let mut info = core::mem::MaybeUninit::<SystemInfo>::uninit();
        // SAFETY: GetSystemInfo fills in the whole structure.
        let info = unsafe {
            GetSystemInfo(info.as_mut_ptr());
            info.assume_init()
        };
        usize::try_from(info.page_size)
            .ok()
            .filter(|&size| size > 0)
    }

    pub fn reserve(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: reserves a new range; nothing is replaced.
        let start = unsafe { VirtualAlloc(ptr::null_mut(), len, MEM_RESERVE, PAGE_READWRITE) };
        NonNull::new(start.cast())
    }

    pub fn map_private(len: usize) -> Option<NonNull<u8>> {
        // SAFETY: reserves and commits a new range; nothing is replaced.
        let start = unsafe {
            VirtualAlloc(
                ptr::null_mut(),
                len,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        NonNull::new(start.cast())
    }

    pub unsafe fn commit(start: NonNull<u8>, len: usize) -> bool {
        // SAFETY: the range is within the reservation; recommitting is allowed.
        !unsafe { VirtualAlloc(start.as_ptr().cast(), len, MEM_COMMIT, PAGE_READWRITE) }.is_null()
    }

    pub const unsafe fn populate(_: NonNull<u8>, _: usize, _: usize) -> bool {
        true
    }

    /// # Safety
    ///
    /// The reservation is no longer used.
    pub unsafe fn release(start: NonNull<u8>, _: usize) {
        // SAFETY: the caller hands over the whole reservation; MEM_RELEASE
        // takes size zero.
        unsafe { VirtualFree(start.as_ptr().cast(), 0, MEM_RELEASE) };
    }
}
