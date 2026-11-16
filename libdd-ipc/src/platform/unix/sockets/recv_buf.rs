// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Per-thread receive buffer for the server dispatch loop.
//!
//! The buffer must hold any message a client can send inline, i.e. up to [`max_message_size()`]
//! bytes, though almost every message is a few KiB. It is an anonymous mapping: only the pages a
//! message is written to become resident, and those beyond [`KEEP_RESIDENT`] are returned to the
//! kernel after a large message.

use super::max_message_size;
use std::cell::RefCell;
use std::io;
use std::ptr::NonNull;

/// Pages below this offset stay resident between messages, as most messages are smaller.
const KEEP_RESIDENT: usize = 256 * 1024;

/// Calls `f` with this thread's receive buffer, sized to [`max_message_size()`], then releases
/// the pages of a large message.
///
/// `f` returns its result along with the number of bytes it received into the buffer.
pub(super) fn with_recv_buf<R>(f: impl FnOnce(&mut [u8]) -> (R, usize)) -> io::Result<R> {
    thread_local! {
        /// Created on first use; unmapped when the thread exits.
        static RECV_BUF: RefCell<Option<RecvBuf>> = const { RefCell::new(None) };
    }
    RECV_BUF.with_borrow_mut(|slot| {
        let size = max_message_size();
        if slot.as_ref().is_some_and(|buf| buf.len != size) {
            *slot = None;
        }
        let buf = match slot {
            Some(buf) => buf,
            None => slot.insert(RecvBuf::new(size)?),
        };
        let (result, received) = f(buf.as_mut_slice());
        buf.release_above_keep(received);
        Ok(result)
    })
}

/// An anonymous private mapping, unmapped on drop.
struct RecvBuf {
    ptr: NonNull<u8>,
    len: usize,
}

impl RecvBuf {
    fn new(len: usize) -> io::Result<Self> {
        // Linux would otherwise charge the whole mapping against the overcommit limit up front.
        #[cfg(target_os = "linux")]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
        #[cfg(not(target_os = "linux"))]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
        // SAFETY: anonymous mapping with no address hint; the result is checked below.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let ptr: NonNull<u8> =
            NonNull::new(ptr.cast()).ok_or_else(|| io::Error::other("mmap returned null"))?;
        // With transparent huge pages enabled system-wide, the first write would fault in a whole
        // 2 MiB page, and keep it resident. Kernels without THP reject this, which is harmless.
        #[cfg(target_os = "linux")]
        // SAFETY: advises on the mapping just created.
        unsafe {
            libc::madvise(ptr.as_ptr().cast(), len, libc::MADV_NOHUGEPAGE)
        };
        Ok(Self { ptr, len })
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the mapping is `len` bytes, readable and writable, and lives as long as `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Returns the pages beyond [`KEEP_RESIDENT`] which the last `received` bytes touched.
    /// They read as zero afterwards, and become resident again only when written.
    fn release_above_keep(&mut self, received: usize) {
        if received <= KEEP_RESIDENT || self.len <= KEEP_RESIDENT {
            return;
        }
        // KEEP_RESIDENT is a multiple of every page size Linux and macOS use (up to 256 KiB).
        // SAFETY: the range lies within the mapping and starts page-aligned; the kernel rounds
        // the length up to whole pages, which still lie within the (page-granular) mapping.
        let start = unsafe { self.ptr.as_ptr().add(KEEP_RESIDENT) };
        // On macOS, MADV_DONTNEED does not release anything, and MADV_FREE only lets the kernel
        // reclaim pages under memory pressure; both count against the footprint until then.
        // Mapping fresh pages over the range releases them at once. The new mapping extends to
        // the end of the buffer: ending it where the message did would split the mapping at a
        // new boundary with every smaller message, which XNU does not reliably merge back.
        // Replacing untouched pages costs nothing.
        #[cfg(target_os = "macos")]
        // SAFETY: replaces part of our own mapping, which nothing borrows during this call.
        unsafe {
            let ret = libc::mmap(
                start.cast(),
                self.len - KEEP_RESIDENT,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
                -1,
                0,
            );
            // Should MAP_FIXED fail, the old pages are left in place, still mapped.
            debug_assert_ne!(ret, libc::MAP_FAILED);
        }
        // MADV_DONTNEED doesn't split the mapping, so only the pages written are released.
        #[cfg(target_os = "linux")]
        // SAFETY: see above.
        unsafe {
            let end = received.min(self.len);
            libc::madvise(start.cast(), end - KEEP_RESIDENT, libc::MADV_DONTNEED);
        }
    }
}

impl Drop for RecvBuf {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping created in `new`, which nothing borrows anymore.
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resident pages of the `len` bytes at `ptr`, per mincore(2).
    fn resident_pages(ptr: *mut u8, len: usize) -> usize {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let start = ptr as usize & !(page - 1);
        let len = ptr as usize + len - start;
        let mut vec = vec![0u8; len.div_ceil(page)];
        // SAFETY: `start` is page-aligned and the pages up to `ptr + len` are mapped; `vec` has
        // one entry per page.
        assert_eq!(
            unsafe { libc::mincore(start as *mut _, len, vec.as_mut_ptr().cast()) },
            0
        );
        vec.iter().filter(|&&v| v & 1 != 0).count()
    }

    /// Only the pages a message is written to become resident, so a thread that only ever
    /// receives small messages doesn't pay for a buffer sized for the largest one.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn small_messages_leave_the_buffer_unresident() {
        std::thread::spawn(|| {
            let (ptr, len) = with_recv_buf(|buf| {
                buf[..1024].fill(1);
                ((buf.as_mut_ptr(), buf.len()), 1024)
            })
            .unwrap();
            assert_eq!(len, max_message_size());
            // The buffer stays mapped until this thread exits.
            let resident = resident_pages(ptr, len);
            assert!(resident <= 2, "{resident} pages of the buffer are resident");
        })
        .join()
        .unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn large_messages_do_not_stay_resident() {
        let len = 4 * 1024 * 1024;
        let mut buf = RecvBuf::new(len).unwrap();
        let ptr = buf.ptr.as_ptr();
        assert_eq!(resident_pages(ptr, len), 0);

        buf.as_mut_slice()[..len].fill(1);
        let all = resident_pages(ptr, len);
        assert!(all > 0);

        buf.release_above_keep(len);
        let kept = resident_pages(ptr, len);
        assert_eq!(kept, resident_pages(ptr, KEEP_RESIDENT));
        assert!(kept < all);
        assert_eq!(buf.as_mut_slice()[KEEP_RESIDENT], 0);
        assert_eq!(buf.as_mut_slice()[KEEP_RESIDENT - 1], 1);

        // Small messages leave the buffer alone.
        buf.as_mut_slice()[..KEEP_RESIDENT].fill(2);
        buf.release_above_keep(KEEP_RESIDENT);
        assert_eq!(buf.as_mut_slice()[KEEP_RESIDENT - 1], 2);
    }

    /// Messages of decreasing size would each split the mapping at a new boundary, which XNU
    /// does not reliably merge back, if the release ended where the message did.
    #[test]
    #[cfg(target_os = "macos")]
    #[cfg_attr(miri, ignore)]
    fn releases_do_not_fragment_the_mapping() {
        let len = 8 * 1024 * 1024;
        let mut buf = RecvBuf::new(len).unwrap();
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let mut received = len;
        while received > KEEP_RESIDENT + page {
            buf.as_mut_slice()[received - 1] = 1;
            buf.release_above_keep(received);
            received -= page;
        }
        let regions = vm_regions(buf.ptr.as_ptr(), len);
        assert!(regions <= 2, "the buffer spans {regions} VM regions");
    }

    /// Number of VM regions covering `len` bytes at `ptr`.
    #[cfg(target_os = "macos")]
    fn vm_regions(ptr: *mut u8, len: usize) -> usize {
        unsafe extern "C" {
            static mach_task_self_: libc::mach_port_t;
            fn mach_vm_region(
                target_task: libc::mach_port_t,
                address: *mut u64,
                size: *mut u64,
                flavor: libc::c_int,
                info: *mut libc::c_int,
                info_count: *mut libc::c_uint,
                object_name: *mut libc::mach_port_t,
            ) -> libc::kern_return_t;
        }
        const VM_REGION_BASIC_INFO_64: libc::c_int = 9;
        let end = ptr as u64 + len as u64;
        let mut address = ptr as u64;
        let mut count = 0;
        while address < end {
            let mut size = 0u64;
            let mut info = [0 as libc::c_int; 16];
            let mut info_count = info.len() as libc::c_uint;
            let mut object_name: libc::mach_port_t = 0;
            // SAFETY: all out-pointers refer to live locals; `info` is larger than
            // vm_region_basic_info_64.
            let ret = unsafe {
                mach_vm_region(
                    mach_task_self_,
                    &mut address,
                    &mut size,
                    VM_REGION_BASIC_INFO_64,
                    info.as_mut_ptr(),
                    &mut info_count,
                    &mut object_name,
                )
            };
            assert_eq!(ret, 0);
            if address >= end {
                break;
            }
            count += 1;
            address += size;
        }
        count
    }
}
