// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(target_has_atomic = "64")]

//! Implementation of the publisher (and same-process reader) part of the [OTEL process
//! context](https://github.com/open-telemetry/opentelemetry-specification/pull/4719)
//! specification.
//!
//! # Cross-platform design
//!
//! The header layout, the seqlock publication protocol, the in-page payload placement and the
//! same-process reader are all OS-agnostic and live at module scope. Only the *discoverability*
//! mechanism used by external, out-of-process readers is Linux-specific: a `memfd`-backed mapping
//! named via `prctl`, located by scanning `/proc/<pid>/maps` (see [`ProcessContextSelfReader`],
//! kept as a reference for such readers). macOS and Windows have no external reader; their backing
//! is a plain page from [`libdd_alloc::VirtualAllocator`] and same-process consumers read it
//! through a live pointer via [`read`].
//!
//! # Seqlock algorithm
//!
//! Implements a seqlock-style algorithm, which generally goes like this:
//!
//! atomic<unsigned> seq{0};
//! atomic<int> data1, data2;
//! T reader() {
//!     int r1, r2;
//!     unsigned seq0, seq1;
//!     do {
//!         seq0 = seq.load(m_o_acquire);
//!         r1 = data1.load(m_o_relaxed);
//!         r2 = data2.load(m_o_relaxed);
//!         atomic_thread_fence(m_o_acquire);
//!         seq1 = seq.load(m_o_relaxed);
//!     } while (seq0 & 1 || seq0 != seq1);
//!     ...
//! }
//!
//! void writer(...) {
//!     unsigned seq0 = seq.load(m_o_relaxed);
//!     while (seq0 & 1 ||
//!            !seq.compare_exchange_weak(seq0, seq0 + 1, m_o_acquire)) {}
//!     atomic_thread_fence(m_o_release);
//!     data1.store(..., m_o_relaxed);
//!     data2.store(..., m_o_relaxed);
//!     seq.store(seq0 + 2, m_o_release);
//! }
//!
//! Although we instead use 0 to signal the writer is in progress and a timestamp instead of even
//! numbers. We also forbid concurrent writers, and leave the reader retries to the discretion of
//! the caller. We ignore the corner case where time returns 0.
//!
//! The seqlock algorithm is inherently racy, so header fields that change across updates are
//! atomics (even if accessed relaxed); otherwise we would hit UB. The payload bytes themselves are
//! read either by a syscall (`process_vm_readv` / a pipe, which fall outside the language memory
//! model) or, for a same-process in-mapping payload, by a direct copy guarded by the seqlock
//! timestamp re-check (a torn copy is detected and discarded, matching the spec Reading Protocol).

use std::io;
use std::mem::{size_of, ManuallyDrop};
use std::ptr::{self, NonNull};
use std::sync::atomic::{fence, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;

#[cfg(not(target_os = "linux"))]
use std::alloc::Layout;

#[cfg(unix)]
use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::ffi::CStr;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::time::Duration;

#[cfg(not(target_os = "linux"))]
use libdd_alloc::{Allocator, VirtualAllocator};

use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value, AnyValue, KeyValue, ProcessContext,
};
use prost::Message;

/// Current version of the process context format.
pub const PROCESS_CTX_VERSION: u32 = 2;
/// Signature bytes for identifying process context mappings.
pub const SIGNATURE: &[u8; 8] = b"OTEL_CTX";
/// The discoverable name of the memory mapping (Linux `memfd`/`prctl`).
#[cfg(target_os = "linux")]
pub const MAPPING_NAME: &CStr = c"OTEL_CTX";

/// The header structure written at the start of the mapping. This must match the C layout of the
/// specification.
///
/// The seqlock algorithm is inherently racy, so the fields that change during an update must be
/// atomic (even if accessed relaxed); otherwise we hit UB. `signature`/`version` are immutable once
/// the mapping is published, so they need not be atomic.
#[repr(C)]
struct MappingHeader {
    signature: [u8; 8],
    version: u32,
    payload_size: AtomicU32,
    monotonic_published_at_ns: AtomicU64,
    payload_ptr: AtomicPtr<u8>,
}

// Compile-time verification that MappingHeader matches the field offsets and total size mandated by
// the OTel process context spec:
// https://github.com/open-telemetry/opentelemetry-specification/blob/main/oteps/profiles/4719-process-ctx.md
const _: () = {
    use std::mem::offset_of;
    assert!(offset_of!(MappingHeader, signature) == 0);
    assert!(offset_of!(MappingHeader, version) == 8);
    assert!(offset_of!(MappingHeader, payload_size) == 12);
    assert!(offset_of!(MappingHeader, monotonic_published_at_ns) == 16);
    assert!(offset_of!(MappingHeader, payload_ptr) == 24);
    assert!(size_of::<MappingHeader>() == 32);
    assert!(core::mem::align_of::<MappingHeader>() == 8);
    assert!(core::mem::align_of::<AtomicU32>() == core::mem::align_of::<u32>());
    assert!(core::mem::align_of::<AtomicPtr<u8>>() == core::mem::align_of::<*mut u8>());
};

/// Returns the OS page size (queried once and cached), never smaller than [`MappingHeader`].
///
/// The mapping is exactly one page: the header sits at the start and, when it fits, the payload
/// follows immediately after it in the same page.
fn mapping_size() -> usize {
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        libdd_alloc::os::page_size()
            .unwrap_or(4096)
            .max(size_of::<MappingHeader>())
    })
}

/// The number of payload bytes that fit in the mapping right after the header.
fn inline_capacity() -> usize {
    mapping_size() - size_of::<MappingHeader>()
}

/// Returns a monotonic timestamp in nanoseconds for the publication protocol.
///
/// The exact clock source is unimportant beyond being monotonic; readers compare the value
/// before/after a read and use it as a cache-invalidation key. Publication additionally enforces a
/// strictly increasing value across updates (see [`ProcessContextHandle::update_payload`]).
#[cfg(unix)]
fn monotonic_ns() -> Option<u64> {
    // Linux uses BOOTTIME (includes suspend, never resets); other unixes use MONOTONIC.
    #[cfg(target_os = "linux")]
    let clock = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    let clock = libc::CLOCK_MONOTONIC;

    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid, writable timespec.
    if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
        return None;
    }
    let secs: u64 = ts.tv_sec.try_into().ok()?;
    let nanos: u32 = ts.tv_nsec.try_into().ok()?;
    u64::try_from(Duration::new(secs, nanos).as_nanos()).ok()
}

/// Returns a monotonic timestamp in nanoseconds for the publication protocol.
#[cfg(windows)]
fn monotonic_ns() -> Option<u64> {
    // QueryUnbiasedInterruptTime yields 100ns units, is monotonic and excludes suspend time.
    let mut t: u64 = 0;
    // SAFETY: t is a valid, writable u64.
    if unsafe { windows_sys::Win32::System::SystemInformation::QueryUnbiasedInterruptTime(&mut t) }
        != 0
    {
        Some(t.saturating_mul(100))
    } else {
        None
    }
}

// --------------------------------------------------------------------------------------------
// Backing storage: a single OS page that unmaps on drop. Only its constructor and free path are
// platform-specific; everything above operates on the `(ptr, len)` view it exposes.
// --------------------------------------------------------------------------------------------

/// A page-sized memory mapping owned by a handle; automatically released on drop.
///
/// # Safety
///
/// - `start_addr` is non-null, page-aligned, valid for [`mapping_size`] bytes, zero-initialized on
///   creation, and stays mapped until `self` is dropped or [`MemMapping::free`]d.
/// - After release, no memory access must be performed on the previously mapped region.
#[derive(Debug)]
struct MemMapping {
    start_addr: NonNull<u8>,
    /// Linux only: whether the mapping is discoverable in `/proc/<pid>/maps` without a successful
    /// `prctl` name (true when memfd-backed; false for the anonymous fallback).
    #[cfg(target_os = "linux")]
    discoverable_without_name: bool,
}

// SAFETY: MemMapping represents exclusive ownership over the mapped region. It never leaks or
// shares the internal pointer, and it is safe to release from a different thread.
unsafe impl Send for MemMapping {}

impl MemMapping {
    fn as_ptr(&self) -> *mut u8 {
        self.start_addr.as_ptr()
    }

    /// Creates a page-sized, zero-initialized mapping (Linux: memfd, falling back to anonymous).
    #[cfg(target_os = "linux")]
    fn new() -> io::Result<Self> {
        let size = mapping_size();

        // Prefer memfd: it makes the mapping discoverable via its `/memfd:OTEL_CTX` name even on
        // kernels where `prctl` naming is unavailable (spec Publication Protocol steps 2-3).
        let memfd = try_memfd(
            MAPPING_NAME,
            libc::MFD_CLOEXEC | libc::MFD_NOEXEC_SEAL | libc::MFD_ALLOW_SEALING,
        )
        .or_else(|_| try_memfd(MAPPING_NAME, libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING));

        match memfd {
            Ok(fd) => {
                // SAFETY: fd is a valid open file descriptor.
                check_syscall_retval(
                    unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) },
                    "ftruncate failed",
                )?;
                // SAFETY: null hint is unconditionally valid; fd is valid and sized.
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
                    "mmap failed",
                )?;
                // The fd is implicitly closed here (OwnedFd drop); the mapping keeps the memory.
                Ok(MemMapping {
                    start_addr,
                    discoverable_without_name: true,
                })
            }
            // Fallback: anonymous mapping (spec step 4). Discoverability now depends on `prctl`.
            Err(_) => {
                // SAFETY: null hint is unconditionally valid.
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
                    "mmap failed: couldn't create a memfd or anonymous region for process context",
                )?;
                Ok(MemMapping {
                    start_addr,
                    discoverable_without_name: false,
                })
            }
        }
    }

    /// Creates a page-sized, zero-initialized mapping using the cross-platform virtual allocator.
    #[cfg(not(target_os = "linux"))]
    fn new() -> io::Result<Self> {
        let layout = Layout::from_size_align(mapping_size(), 1)
            .map_err(|_| io::Error::other("invalid process context mapping layout"))?;
        let region = VirtualAllocator {}
            .allocate_zeroed(layout)
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "virtual allocation failed"))?;
        Ok(MemMapping {
            start_addr: region.cast::<u8>(),
        })
    }

    /// Applies fork-safety advice to the mapping (Linux `madvise(MADV_DONTFORK)`). No-op elsewhere.
    #[cfg(target_os = "linux")]
    fn madvise_dontfork(&self) -> io::Result<()> {
        check_syscall_retval(
            // SAFETY: start_addr is valid for mapping_size() bytes per MemMapping invariants.
            unsafe {
                libc::madvise(
                    self.start_addr.as_ptr().cast(),
                    mapping_size(),
                    libc::MADV_DONTFORK,
                )
            },
            "madvise MADV_DONTFORK failed",
        )?;
        Ok(())
    }

    /// Applies fork-safety advice to the mapping. No-op off Linux.
    #[cfg(not(target_os = "linux"))]
    fn madvise_dontfork(&self) -> io::Result<()> {
        Ok(())
    }

    /// Attempts to name the mapping so external readers can discover it (Linux `prctl`).
    #[cfg(target_os = "linux")]
    fn set_name(&self) -> io::Result<()> {
        // SAFETY: start_addr is valid for mapping_size() bytes; MAPPING_NAME is a static NUL-
        // terminated string that outlives the call.
        check_syscall_retval(
            unsafe {
                libc::prctl(
                    libc::PR_SET_VMA,
                    libc::PR_SET_VMA_ANON_NAME as libc::c_ulong,
                    self.start_addr.as_ptr() as libc::c_ulong,
                    mapping_size() as libc::c_ulong,
                    MAPPING_NAME.as_ptr() as libc::c_ulong,
                )
            },
            "prctl PR_SET_VMA_ANON_NAME failed",
        )?;
        Ok(())
    }

    /// Names the mapping after a (re)publication, per Publication Protocol step 10 / Updating
    /// Protocol step 7: attempt unconditionally, tolerate failure *unless* the mapping would then
    /// be undiscoverable (anonymous fallback with no name — the spec treats that as a publish
    /// failure). No-op off Linux (nothing discovers it there).
    #[cfg(target_os = "linux")]
    fn name_after_publish(&self) -> io::Result<()> {
        match self.set_name() {
            Ok(()) => Ok(()),
            // memfd-backed: still discoverable via its `/memfd:` name, naming is best-effort.
            Err(_) if self.discoverable_without_name => Ok(()),
            // anonymous fallback + naming failed => not discoverable => publication failed.
            Err(e) => Err(io::Error::other(format!(
                "process context is not discoverable (memfd unavailable and naming failed): {e}"
            ))),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn name_after_publish(&self) -> io::Result<()> {
        Ok(())
    }

    /// Unmaps the underlying memory region, propagating any error (unlike `Drop`).
    fn free(mut self) -> io::Result<()> {
        // SAFETY: we forget `self` right after, so `unmap` runs exactly once.
        unsafe { self.unmap()? };
        let _ = ManuallyDrop::new(self);
        Ok(())
    }

    /// Releases the underlying region.
    ///
    /// # Safety
    ///
    /// Must be called at most once; afterwards no method of `MemMapping` (including `drop`) may run
    /// on `self`. In practice `self` must be wrapped in `ManuallyDrop` or be dropping.
    #[cfg(target_os = "linux")]
    unsafe fn unmap(&mut self) -> io::Result<()> {
        check_syscall_retval(
            // SAFETY: upheld by the caller.
            unsafe { libc::munmap(self.start_addr.as_ptr().cast(), mapping_size()) },
            "munmap failed when freeing the process context",
        )?;
        Ok(())
    }

    /// Releases the underlying region.
    ///
    /// # Safety
    ///
    /// See the Linux variant: must be called at most once.
    #[cfg(not(target_os = "linux"))]
    unsafe fn unmap(&mut self) -> io::Result<()> {
        let layout = Layout::from_size_align(mapping_size(), 1)
            .map_err(|_| io::Error::other("invalid process context mapping layout"))?;
        // SAFETY: start_addr/layout come from this allocator's `allocate_zeroed`; called once.
        unsafe { VirtualAllocator {}.deallocate(self.start_addr, layout) };
        Ok(())
    }
}

impl Drop for MemMapping {
    fn drop(&mut self) {
        // SAFETY: `self` is being dropped, so `unmap` runs exactly once.
        let _ = unsafe { self.unmap() };
    }
}

#[cfg(target_os = "linux")]
fn check_mapping_addr(addr: *mut c_void, msg: &'static str) -> io::Result<NonNull<u8>> {
    if addr == libc::MAP_FAILED {
        let e = io::Error::last_os_error();
        Err(io::Error::new(e.kind(), format!("{msg}: {e}")))
    } else {
        // SAFETY: mmap returns a non-null pointer on success.
        Ok(unsafe { NonNull::new_unchecked(addr.cast()) })
    }
}

#[cfg(target_os = "linux")]
fn check_syscall_retval(ret: libc::c_int, msg: &'static str) -> io::Result<libc::c_int> {
    if ret < 0 {
        let e = io::Error::last_os_error();
        Err(io::Error::new(e.kind(), format!("{msg}: {e}")))
    } else {
        Ok(ret)
    }
}

/// Creates a `memfd` file descriptor with the given name and flags.
#[cfg(target_os = "linux")]
fn try_memfd(name: &CStr, flags: libc::c_uint) -> io::Result<OwnedFd> {
    // We use the raw syscall rather than `libc::memfd_create` because the latter requires glibc
    // >= 2.27, while `syscall()` + `SYS_memfd_create` works with any glibc version.
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

// --------------------------------------------------------------------------------------------
// Writer: caller-owned handle. No global state; the caller retains the handle (and thus keeps the
// mapping alive) and passes `mapping()` to `read`.
// --------------------------------------------------------------------------------------------

/// Handle for a published process context. Owning it keeps the mapping alive; dropping it (or
/// calling [`ProcessContextHandle::free`]) unpublishes and releases the mapping.
#[derive(Debug)]
pub struct ProcessContextHandle {
    mapping: MemMapping,
    /// Overflow payload kept alive while `payload_ptr` points at it. `None` when the payload fits
    /// inline in the mapping page (the bytes then live in the page and nothing extra is retained).
    payload: Option<Vec<u8>>,
    /// Whether allocating a separate heap buffer is allowed when the payload doesn't fit in-page.
    allow_overflow_alloc: bool,
    /// Timestamp of the most recent publication/update.
    last_published_at_ns: u64,
    /// PID of the publisher, used to detect `fork()` (Linux).
    #[cfg(target_os = "linux")]
    pid: libc::pid_t,
}

impl ProcessContextHandle {
    /// Base pointer and length of the mapping, for the caller to retain and later pass to [`read`].
    #[must_use]
    pub fn mapping(&self) -> (*const u8, usize) {
        (self.mapping.as_ptr().cast_const(), mapping_size())
    }

    /// Like [`mapping`](Self::mapping), but returns `None` when this handle is a stale copy left in
    /// a `fork()`ed child that hasn't republished yet: on Linux the mapping is `MADV_DONTFORK`'d,
    /// so its pages are absent in the child and a reader dereferencing the base would fault.
    /// Same- process readers that don't otherwise coordinate with republication should use
    /// this.
    #[must_use]
    pub fn current_mapping(&self) -> Option<(*const u8, usize)> {
        #[cfg(target_os = "linux")]
        // SAFETY: getpid() is always safe to call.
        if self.pid != unsafe { libc::getpid() } {
            return None;
        }
        Some((self.mapping.as_ptr().cast_const(), mapping_size()))
    }

    /// The timestamp of the most recent publication/update.
    #[must_use]
    pub fn published_at_ns(&self) -> u64 {
        self.last_published_at_ns
    }

    /// Updates the published context. On Linux this transparently republishes into a fresh mapping
    /// if it detects that a `fork()` happened since the last publication.
    pub fn update(&mut self, context: &ProcessContext) -> io::Result<()> {
        self.update_payload(context.encode_to_vec())
    }

    /// Unpublishes and releases the mapping, propagating any release error.
    ///
    /// # Safety
    ///
    /// This may only be called if there are no in-process readers still using this mapping's
    /// pointer, or at least none that will use it after this call.
    pub unsafe fn free(self) -> io::Result<()> {
        let ProcessContextHandle {
            mapping, payload, ..
        } = self;
        // Mark the context as being-updated and order that store before the payload/mapping free,
        // so a concurrent reader observes an in-progress update rather than freed memory.
        let header = mapping.as_ptr().cast::<MappingHeader>();
        // SAFETY: the mapping is still live and valid; the timestamp field is atomic and aligned.
        unsafe {
            (*header)
                .monotonic_published_at_ns
                .store(0, Ordering::Relaxed)
        };
        fence(Ordering::Release);
        mapping.free()?;
        drop(payload);
        Ok(())
    }

    /// Places `payload` either inline (right after the header, when it fits) or in a retained heap
    /// buffer. Returns `(payload_ptr, retained_overflow)`.
    ///
    /// # Safety
    ///
    /// `base` must be the start of a live mapping of at least [`mapping_size`] bytes; the returned
    /// pointer stays valid while the returned overflow buffer (if any) is retained and the mapping
    /// is live.
    unsafe fn place_payload(
        base: *mut u8,
        payload: Vec<u8>,
        allow_overflow_alloc: bool,
    ) -> io::Result<(*mut u8, Option<Vec<u8>>)> {
        if payload.len() <= inline_capacity() {
            // Inline: copy the bytes into the page immediately after the header (spec step 6,
            // "storing it ... following the header").
            let dst = unsafe { base.add(size_of::<MappingHeader>()) };
            // SAFETY: dst points to `inline_capacity()` writable bytes disjoint from the header,
            // and payload is a distinct allocation.
            unsafe { ptr::copy_nonoverlapping(payload.as_ptr(), dst, payload.len()) };
            Ok((dst, None))
        } else if allow_overflow_alloc {
            // Overflow: keep the heap buffer alive and point at it (spec step 6, "or in a separate
            // memory allocation").
            let payload_ptr = payload.as_ptr().cast_mut();
            Ok((payload_ptr, Some(payload)))
        } else {
            Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "process context payload does not fit in one page and overflow allocation is disabled",
            ))
        }
    }

    /// Initial publication following the OTEL Publication Protocol.
    fn publish_payload(payload: Vec<u8>, allow_overflow_alloc: bool) -> io::Result<Self> {
        let payload_size: u32 = payload
            .len()
            .try_into()
            .map_err(|_| io::Error::other("payload size overflowed"))?;

        let mapping = MemMapping::new()?;
        mapping.madvise_dontfork()?;

        let published_at_ns = monotonic_ns()
            .ok_or_else(|| io::Error::other("failed to get monotonic time for publication"))?;

        let base = mapping.as_ptr();
        // SAFETY: base is a fresh, live, zero-initialized mapping of mapping_size() bytes.
        let (payload_ptr, overflow) =
            unsafe { Self::place_payload(base, payload, allow_overflow_alloc)? };

        let header = base.cast::<MappingHeader>();
        // SAFETY: header points to a zero-filled, page-aligned mapping of at least
        // mapping_size() bytes; the field projections are in-bounds and aligned.
        unsafe {
            ptr::addr_of_mut!((*header).signature).write(*SIGNATURE);
            ptr::addr_of_mut!((*header).version).write(PROCESS_CTX_VERSION);
            (*header)
                .payload_size
                .store(payload_size, Ordering::Relaxed);
            (*header).payload_ptr.store(payload_ptr, Ordering::Relaxed);
            // Written last, with Release ordering, so a reader that acquires a non-zero timestamp
            // also observes the header/payload writes above (spec steps 8-9).
            (*header)
                .monotonic_published_at_ns
                .store(published_at_ns, Ordering::Release);
        }

        let handle = ProcessContextHandle {
            mapping,
            payload: overflow,
            allow_overflow_alloc,
            last_published_at_ns: published_at_ns,
            // SAFETY: getpid() is always safe to call.
            #[cfg(target_os = "linux")]
            pid: unsafe { libc::getpid() },
        };

        // Spec step 10: name the mapping (best-effort, but required if not otherwise discoverable).
        handle.mapping.name_after_publish()?;

        Ok(handle)
    }

    /// Updates the context in place following the OTEL Updating Protocol. On Linux, if a `fork()`
    /// is detected, republishes into a fresh mapping and leaks the old (possibly `MADV_DONTFORK`'d
    /// or remapped) region instead of touching it.
    fn update_payload(&mut self, payload: Vec<u8>) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: getpid() is always safe to call.
            if self.pid != unsafe { libc::getpid() } {
                let fresh = Self::publish_payload(payload, self.allow_overflow_alloc)?;
                let old = std::mem::replace(self, fresh);
                // Don't unmap the parent's mapping from the child: it was `MADV_DONTFORK`'d (so it
                // isn't mapped here) or could have been remapped to something else.
                let _ = ManuallyDrop::new(old.mapping);
                return Ok(());
            }
        }

        let payload_size: u32 = payload.len().try_into().map_err(|_| {
            io::Error::other("couldn't update process context: new payload too large")
        })?;

        let now = monotonic_ns()
            .ok_or_else(|| io::Error::other("could not get the current timestamp"))?;

        let base = self.mapping.as_ptr();
        let header = base.cast::<MappingHeader>();
        // SAFETY: the mapping is live and valid; the timestamp field is atomic and aligned.
        let published_at_atomic = unsafe { &(*header).monotonic_published_at_ns };

        // Spec step 2: signal update-in-progress by writing 0. A process must not concurrently
        // update its own context.
        //
        // Note: avoid early returns while the timestamp is still 0, as that would "lock" the
        // context for future readers; restore it before bailing out.
        let previous_published_at_ns = published_at_atomic.swap(0, Ordering::Acquire);
        if previous_published_at_ns == 0 {
            return Err(io::Error::other(
                "concurrent update of the process context is not supported",
            ));
        }
        // The timestamp doubles as the seqlock version, so it must strictly advance even if the
        // clock returns the same value for two rapid updates.
        let published_at_ns = now.max(previous_published_at_ns.saturating_add(1));

        fence(Ordering::Release);

        // SAFETY: base is a live mapping of mapping_size() bytes.
        let (payload_ptr, new_overflow) =
            match unsafe { Self::place_payload(base, payload, self.allow_overflow_alloc) } {
                Ok(placed) => placed,
                Err(e) => {
                    // Restore the previous timestamp so we don't leave readers blocked.
                    published_at_atomic.store(previous_published_at_ns, Ordering::Release);
                    return Err(e);
                }
            };

        // SAFETY: the mapping is live and valid; these fields are atomic and aligned.
        unsafe {
            (*header).payload_ptr.store(payload_ptr, Ordering::Relaxed);
            (*header)
                .payload_size
                .store(payload_size, Ordering::Relaxed);
        }
        published_at_atomic.store(published_at_ns, Ordering::Release);
        self.last_published_at_ns = published_at_ns;

        // Only now drop the previous overflow buffer (if any): a concurrent same-process reader on
        // the pipe-safe-read path can't race a free before the update is fully published.
        let _old = std::mem::replace(&mut self.payload, new_overflow);
        drop(_old);

        // Spec step 7: re-issue naming unconditionally (best-effort; the mapping keeps its stable
        // address, this re-signals eBPF `prctl`-hook readers of the update).
        #[cfg(target_os = "linux")]
        let _ = self.mapping.set_name();

        Ok(())
    }
}

/// Publishes the process context and returns an owning handle. The caller MUST retain the handle
/// for as long as the context should stay visible, and store `handle.mapping()` where its
/// same-process readers can find it. Overflow allocation (for payloads larger than one page) is
/// allowed; use [`publish_with`] to opt out.
pub fn publish(context: &ProcessContext) -> io::Result<ProcessContextHandle> {
    ProcessContextHandle::publish_payload(context.encode_to_vec(), true)
}

/// Like [`publish`], but lets the caller decide whether an over-a-page payload may spill into a
/// separate heap allocation (`true`) or should fail publication instead (`false`).
pub fn publish_with(
    context: &ProcessContext,
    allow_overflow_alloc: bool,
) -> io::Result<ProcessContextHandle> {
    ProcessContextHandle::publish_payload(context.encode_to_vec(), allow_overflow_alloc)
}

// --------------------------------------------------------------------------------------------
// Same-process reader: takes the mapping pointer as an argument (no discovery). Works identically
// on every platform, and never uses `process_vm_readv` (which hardened seccomp profiles block).
// --------------------------------------------------------------------------------------------

/// Copies `len` bytes from `ptr` through a pipe so the kernel validates the source range and
/// returns an error (instead of crashing) if it is invalid or has been unmapped.
///
/// Used for the rare case where the payload lives outside the mapping (overflow) and could race a
/// concurrent update that frees it. Returns [`io::ErrorKind::WouldBlock`] when the source became
/// invalid mid-read (retryable), distinct from decode errors on the copied bytes.
#[cfg(unix)]
fn safe_read(ptr: *const u8, len: usize) -> io::Result<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }

    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: fds is a valid array of two ints.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe() just returned two valid, owned file descriptors.
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    // Non-blocking so a full pipe surfaces as EAGAIN (drain then retry) rather than deadlocking
    // this single thread that both writes and reads.
    set_nonblocking(&read_fd)?;
    set_nonblocking(&write_fd)?;

    let mut out = Vec::with_capacity(len);
    let mut written = 0usize;
    let mut buf = [0u8; 8192];

    while out.len() < len {
        if written < len {
            // SAFETY: we pass the raw pointer directly (never a Rust slice, which would be UB over
            // an invalid pointer). The kernel validates the range while copying from userspace.
            let n = unsafe {
                libc::write(
                    write_fd.as_raw_fd(),
                    ptr.add(written).cast::<c_void>(),
                    len - written,
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(libc::EAGAIN) => { /* pipe full: drain below */ }
                    Some(libc::EFAULT) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "process context payload pointer became invalid while reading",
                        ))
                    }
                    _ => return Err(err),
                }
            } else {
                written += n as usize;
            }
        }

        // SAFETY: buf is a valid writable buffer of buf.len() bytes.
        let n = unsafe {
            libc::read(
                read_fd.as_raw_fd(),
                buf.as_mut_ptr().cast::<c_void>(),
                buf.len(),
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EINTR) => continue,
                // Nothing buffered yet but we still owe writes: loop to write more.
                Some(libc::EAGAIN) if written < len => continue,
                _ => return Err(err),
            }
        } else if n == 0 {
            break;
        } else {
            out.extend_from_slice(&buf[..n as usize]);
        }
    }

    Ok(out)
}

#[cfg(unix)]
fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fd is a valid open file descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is valid; setting O_NONBLOCK is well-defined.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Windows counterpart of [`safe_read`]. `WriteFile` probes the source buffer in kernel mode and
/// fails with `ERROR_NOACCESS` for an invalid pointer instead of raising a hardware exception.
///
/// NOTE: only reached for overflow payloads (larger than one page), expected to be vanishingly
/// rare; the in-page common case never calls it. The behavior still needs verification on real
/// Windows.
#[cfg(windows)]
fn safe_read(ptr: *const u8, len: usize) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NOACCESS, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows_sys::Win32::System::Pipes::CreatePipe;

    if len == 0 {
        return Ok(Vec::new());
    }

    let mut read_h: HANDLE = ptr::null_mut();
    let mut write_h: HANDLE = ptr::null_mut();
    // Size the pipe buffer to hold the whole payload so a single write/read pair can't deadlock.
    let nsize = u32::try_from(len).unwrap_or(u32::MAX);
    // SAFETY: read_h/write_h are valid out-params.
    if unsafe { CreatePipe(&mut read_h, &mut write_h, ptr::null_mut(), nsize) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let result = (|| {
        let mut written = 0usize;
        while written < len {
            let mut wrote: u32 = 0;
            let chunk = u32::try_from(len - written).unwrap_or(u32::MAX);
            // SAFETY: raw source pointer passed directly; the kernel validates it while copying.
            let ok = unsafe {
                WriteFile(
                    write_h,
                    ptr.add(written),
                    chunk,
                    &mut wrote,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_NOACCESS as i32) {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "process context payload pointer became invalid while reading",
                    ));
                }
                return Err(err);
            }
            written += wrote as usize;
        }

        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let mut buf = [0u8; 8192];
            let want = u32::try_from((len - out.len()).min(buf.len())).unwrap_or(u32::MAX);
            let mut got: u32 = 0;
            // SAFETY: buf is a valid writable buffer.
            let ok = unsafe { ReadFile(read_h, buf.as_mut_ptr(), want, &mut got, ptr::null_mut()) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if got == 0 {
                break;
            }
            out.extend_from_slice(&buf[..got as usize]);
        }
        Ok(out)
    })();

    // SAFETY: both handles were created by CreatePipe above.
    unsafe {
        CloseHandle(read_h);
        CloseHandle(write_h);
    }
    result
}

/// Decodes an already-located OTel process context mapping. The caller supplies the mapping base
/// pointer and length (from [`ProcessContextHandle::mapping`], retained by the runtime) — there is
/// no discovery step, so this behaves identically on Linux, macOS and Windows.
///
/// Intended for same-process (SDK-internal) consumers. A genuine cross-process reader must discover
/// and copy the mapping itself (see [`ProcessContextSelfReader`], Linux-only).
///
/// Returns the decoded context together with the `monotonic_published_at_ns` it was read at, which
/// callers use as a cache-invalidation key.
pub fn read(mapping_base: *const u8, mapping_len: usize) -> io::Result<(ProcessContext, u64)> {
    if mapping_base.is_null() || mapping_len < size_of::<MappingHeader>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid process context mapping base/length",
        ));
    }
    // SAFETY: the caller guarantees `mapping_base` is a live mapping of at least `mapping_len`
    // bytes (>= size_of::<MappingHeader>()), so the header is readable.
    let header = unsafe { &*mapping_base.cast::<MappingHeader>() };

    let published_at = header.monotonic_published_at_ns.load(Ordering::Acquire);
    if published_at == 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "process context is currently being updated",
        ));
    }

    // `signature`/`version` are immutable once published; the seqlock fields are loaded atomically.
    if header.signature != *SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid signature in process context mapping",
        ));
    }
    if header.version != PROCESS_CTX_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported process context version {}", header.version),
        ));
    }

    let payload_size = header.payload_size.load(Ordering::Relaxed) as usize;
    let payload_ptr = header.payload_ptr.load(Ordering::Relaxed).cast_const();
    if payload_ptr.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process context payload pointer is null",
        ));
    }

    // Is the payload inside the mapping (inline) or in a separate allocation (overflow)?
    let base_addr = mapping_base as usize;
    let payload_addr = payload_ptr as usize;
    let within = payload_addr >= base_addr
        && payload_addr
            .checked_add(payload_size)
            .zip(base_addr.checked_add(mapping_len))
            .is_some_and(|(payload_end, mapping_end)| payload_end <= mapping_end);

    let payload: Vec<u8> = if within {
        // Inline: same-process memory that is part of the live mapping. Copy directly (the spec's
        // seqlock read); a torn copy is detected by the timestamp re-check below and discarded.
        let mut buf = vec![0u8; payload_size];
        // SAFETY: the payload lies fully within the live mapping we were handed; buf holds
        // payload_size bytes; the regions are distinct.
        unsafe { ptr::copy_nonoverlapping(payload_ptr, buf.as_mut_ptr(), payload_size) };
        buf
    } else {
        // Overflow: the pointer may have been freed by a concurrent update; copy it safely.
        safe_read(payload_ptr, payload_size)?
    };

    // Pairs with the writer's Release store on the timestamp: if the payload changed under us we
    // must observe a different (or zero) timestamp here.
    fence(Ordering::Acquire);
    let published_at_after = header.monotonic_published_at_ns.load(Ordering::Relaxed);
    if published_at != published_at_after {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "process context changed while being read",
        ));
    }

    let context = ProcessContext::decode(payload.as_slice())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((context, published_at))
}

fn string_array(value: &AnyValue) -> Option<Vec<String>> {
    let any_value::Value::ArrayValue(array) = value.value.as_ref()? else {
        return None;
    };

    array
        .values
        .iter()
        .map(|value| match value.value.as_ref()? {
            any_value::Value::StringValue(value) => Some(value.clone()),
            _ => None,
        })
        .collect()
}

fn find_attr<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
    attrs
        .iter()
        .find(|attr| attr.key == key)
        .and_then(|attr| attr.value.as_ref())
}

/// Returns the thread-local attribute key map from a decoded process context.
pub fn threadlocal_attribute_key_map(context: &ProcessContext) -> Option<Vec<String>> {
    let key = "threadlocal.attribute_key_map";

    context
        .resource
        .as_ref()
        .and_then(|resource| find_attr(&resource.attributes, key))
        .or_else(|| find_attr(&context.extra_attributes, key))
        .and_then(string_array)
}

/// Reads the process context at `mapping_base`/`mapping_len` and returns its thread-local attribute
/// key map along with the `monotonic_published_at_ns` it was read at (for cache invalidation).
pub fn read_threadlocal_attribute_key_map(
    mapping_base: *const u8,
    mapping_len: usize,
) -> io::Result<(Option<Vec<String>>, u64)> {
    let (context, published_at) = read(mapping_base, mapping_len)?;
    Ok((threadlocal_attribute_key_map(&context), published_at))
}

// --------------------------------------------------------------------------------------------
// Discovery (Linux only): reference implementation of the spec's Reading Protocol for genuine
// out-of-process readers (e.g. a future eBPF profiler), which must locate the mapping via
// `/proc/<pid>/maps` and copy it with `process_vm_readv`. Same-process consumers use `read`
// instead (a retained pointer, no discovery, no `process_vm_readv`).
// --------------------------------------------------------------------------------------------

/// Reader for the current process's OTel process context mapping, via `/proc/self/maps` discovery
/// and `process_vm_readv`.
///
/// This is the reference implementation of the spec's Reading Protocol for external readers.
/// **In-process consumers should prefer [`read`] with a retained [`ProcessContextHandle::mapping`]
/// pointer**, which avoids both `/proc` parsing and `process_vm_readv` (commonly blocked by
/// hardened seccomp profiles).
///
/// Locates the OTEL_CTX mapping at construction; call [`read`](Self::read) repeatedly to fetch
/// updated context data without re-parsing `/proc/self/maps`, as long as the process has not
/// forked. After a `fork()`, reads fail and a new reader must be constructed.
#[cfg(target_os = "linux")]
pub struct ProcessContextSelfReader {
    pid: libc::pid_t,
    header_ptr: NonNull<MappingHeader>,
}

// SAFETY: ProcessContextSelfReader doesn't rely on thread local state and only references static
// memory -- owns nothing.
#[cfg(target_os = "linux")]
unsafe impl Send for ProcessContextSelfReader {}
// SAFETY: ProcessContextSelfReader doesn't modify anything.
#[cfg(target_os = "linux")]
unsafe impl Sync for ProcessContextSelfReader {}

#[cfg(target_os = "linux")]
impl ProcessContextSelfReader {
    /// Locates the OTEL_CTX mapping in `/proc/self/maps`.
    pub fn new() -> io::Result<Self> {
        let mapping_addr = Self::find_otel_mapping()?;
        // SAFETY: getpid() is always safe to call.
        let pid = unsafe { libc::getpid() };
        Ok(Self {
            pid,
            header_ptr: Self::header_ptr_from_addr(mapping_addr)?,
        })
    }

    /// Reads and decodes the current process's OTel process context.
    pub fn read(&self) -> io::Result<ProcessContext> {
        // SAFETY: getpid() is always safe to call.
        let current_pid = unsafe { libc::getpid() };
        if current_pid != self.pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process context reader is stale after fork; construct a new reader",
            ));
        }

        // SAFETY: `header_ptr` is non-null and points to our own process memory at an address we
        // found in /proc/self/maps for `self.pid`. The mapping must be readable if it is listed as
        // the OTel context.
        let header = unsafe { self.header_ptr.as_ref() };

        let published_at = header.monotonic_published_at_ns.load(Ordering::Acquire);
        if published_at == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context is currently being updated",
            ));
        }

        let signature = header.signature;
        let version = header.version;

        if signature != *SIGNATURE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid signature in process context mapping",
            ));
        }
        if version != PROCESS_CTX_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported process context version {version}"),
            ));
        }

        let payload_size = header.payload_size.load(Ordering::Relaxed);
        let payload_ptr = header.payload_ptr.load(Ordering::Relaxed).cast_const();

        if payload_ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process context payload pointer is null",
            ));
        }

        let payload_bytes =
            Self::read_process_memory(self.pid, payload_ptr, payload_size as usize)?;

        // pairs with the first release fence on update() to ensure that, if we read data updated
        // after the initial published time, we at least see the published time being set to 0 in
        // the next load of the published time (or we could see a later time rather than 0).
        fence(Ordering::Acquire);

        let published_at_after = header.monotonic_published_at_ns.load(Ordering::Relaxed);
        if published_at != published_at_after {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "process context changed while being read",
            ));
        }

        let context = ProcessContext::decode(payload_bytes.as_slice())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        Ok(context)
    }

    fn header_ptr_from_addr(mapping_addr: usize) -> io::Result<NonNull<MappingHeader>> {
        NonNull::new(ptr::with_exposed_provenance::<MappingHeader>(mapping_addr).cast_mut())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "null process context header")
            })
    }

    /// Find the OTEL_CTX mapping in /proc/self/maps.
    fn find_otel_mapping() -> io::Result<usize> {
        let file = File::open("/proc/self/maps")?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line?;

            if Self::is_named_otel_mapping(&line) {
                if let Some(addr) = Self::parse_mapping_start(&line) {
                    return Ok(addr);
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "couldn't find the mapping of the process context",
        ))
    }

    /// Parses the start address from a /proc/self/maps line.
    fn parse_mapping_start(line: &str) -> Option<usize> {
        usize::from_str_radix(line.split('-').next()?, 16).ok()
    }

    /// Checks if a mapping line refers to the OTEL_CTX mapping.
    fn is_named_otel_mapping(line: &str) -> bool {
        let trimmed = line.trim_end();

        // The name of the mapping is the 6th column. The separator changes (both ' ' and '\t') but
        // `split_whitespace()` takes care of that.
        let Some(name) = trimmed.split_whitespace().nth(5) else {
            return false;
        };

        // The spec says to search for entries whose name **starts with** `[anon_shmem:OTEL_CTX]`,
        // `[anon:OTEL_CTX]` or `/memfd:OTEL_CTX`.
        name.starts_with("/memfd:OTEL_CTX")
            || name.starts_with("[anon_shmem:OTEL_CTX]")
            || name.starts_with("[anon:OTEL_CTX]")
    }

    /// Reads `len` bytes from `addr` in the address space of `pid` via `process_vm_readv(2)`.
    ///
    /// Returns [`io::ErrorKind::WouldBlock`] when the remote memory is no longer mapped or only
    /// partially readable.
    fn read_process_memory(pid: libc::pid_t, addr: *const u8, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }

        let mut buf = vec![0u8; len];
        let local_iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: len,
        };
        let remote_iov = libc::iovec {
            iov_base: addr.cast_mut().cast(),
            iov_len: len,
        };

        // SAFETY: `buf` and `addr` each span `len` bytes for the duration of the syscall.
        let nbytes = unsafe { libc::process_vm_readv(pid, &local_iov, 1, &remote_iov, 1, 0) };

        if nbytes < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EFAULT) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "process context payload was unmapped during read",
                ));
            }
            return Err(io::Error::new(
                err.kind(),
                format!("failed to read process context payload: {err}"),
            ));
        }

        if nbytes as usize != len {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "incomplete read of process context payload",
            ));
        }

        Ok(buf)
    }
}

#[cfg(test)]
#[serial_test::serial]
mod tests {
    use super::*;
    use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource;

    fn ctx_with_payload(bytes: &str) -> ProcessContext {
        ProcessContext {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "datadog.process_tags".to_owned(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(bytes.to_owned())),
                    }),
                    key_ref: 0,
                }],
                dropped_attributes_count: 0,
                entity_refs: vec![],
            }),
            extra_attributes: vec![],
        }
    }

    /// Builds a context carrying a `threadlocal.attribute_key_map` array attribute.
    fn ctx_with_key_map(keys: &[&str]) -> ProcessContext {
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::ArrayValue;
        ProcessContext {
            resource: None,
            extra_attributes: vec![KeyValue {
                key: "threadlocal.attribute_key_map".to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::ArrayValue(ArrayValue {
                        values: keys
                            .iter()
                            .map(|k| AnyValue {
                                value: Some(any_value::Value::StringValue((*k).to_owned())),
                            })
                            .collect(),
                    })),
                }),
                key_ref: 0,
            }],
        }
    }

    /// The contract dd-trace-php's profiler relies on for cache invalidation (B2): when the
    /// append-only key map grows, `read_threadlocal_attribute_key_map` returns the longer map and a
    /// strictly greater timestamp, so a cached map that doesn't cover a new key index is refreshed.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn key_map_grows_bumps_timestamp() {
        let mut handle = publish(&ctx_with_key_map(&[
            "datadog.local_root_span_id",
            "service.name",
        ]))
        .expect("publish initial key map");
        let (base, len) = handle.mapping();

        let (keys_v1, ts_v1) =
            read_threadlocal_attribute_key_map(base, len).expect("read initial key map");
        assert_eq!(
            keys_v1.as_deref(),
            Some(
                [
                    "datadog.local_root_span_id".to_owned(),
                    "service.name".to_owned()
                ]
                .as_slice()
            )
        );

        // Append a key (index 2 was previously unknown — the bug was caching v1 forever).
        handle
            .update(&ctx_with_key_map(&[
                "datadog.local_root_span_id",
                "service.name",
                "service.version",
            ]))
            .expect("grow key map");

        let (keys_v2, ts_v2) =
            read_threadlocal_attribute_key_map(base, len).expect("read grown key map");
        assert_eq!(
            keys_v2.map(|k| k.len()),
            Some(3),
            "grown key map should have 3 keys"
        );
        assert!(
            ts_v2 > ts_v1,
            "timestamp must strictly increase so caches invalidate"
        );

        // SAFETY: no other reader is using the mapping in this test.
        unsafe { handle.free().expect("free") };
    }

    /// Whether the handle's `payload_ptr` currently points inside its mapping page (inline).
    fn payload_is_inline(handle: &ProcessContextHandle) -> bool {
        let (base, len) = handle.mapping();
        // SAFETY: base is a live mapping of len bytes.
        let header = unsafe { &*base.cast::<MappingHeader>() };
        let payload_addr = header.payload_ptr.load(Ordering::Relaxed) as usize;
        let base_addr = base as usize;
        payload_addr >= base_addr && payload_addr < base_addr + len
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn publish_then_update_reads_back_inline() {
        let ctx_v1 = ctx_with_payload("example process context payload");
        let ctx_v2 = ctx_with_payload("another example process context payload of different size");

        let mut handle = publish(&ctx_v1).expect("couldn't publish the process context");
        assert!(payload_is_inline(&handle), "small payload should be inline");
        let (base, len) = handle.mapping();

        let (read_v1, ts_v1) = read(base, len).expect("couldn't read back the process context");
        assert_eq!(read_v1, ctx_v1, "payload mismatch after publish");
        assert!(ts_v1 > 0, "monotonic_published_at_ns is zero");

        handle.update(&ctx_v2).expect("couldn't update the context");
        let (read_v2, ts_v2) = read(base, len).expect("couldn't read back after update");
        assert_eq!(read_v2, ctx_v2, "payload mismatch after update");
        assert!(
            ts_v2 > ts_v1,
            "published_at_ns should be strictly greater after update"
        );

        // SAFETY: no other reader is using the mapping in this test.
        unsafe { handle.free().expect("couldn't free the context") };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn overflow_payload_is_read_back() {
        // A payload larger than a page forces the overflow (separate allocation) path.
        let big = "x".repeat(mapping_size() * 2);
        let ctx = ctx_with_payload(&big);

        let handle = publish(&ctx).expect("couldn't publish an overflow context");
        assert!(!payload_is_inline(&handle), "large payload should overflow");

        let (base, len) = handle.mapping();
        let (read_back, _ts) = read(base, len).expect("couldn't read back overflow context");
        assert_eq!(read_back, ctx, "overflow payload mismatch");

        // SAFETY: no other reader is using the mapping in this test.
        unsafe { handle.free().expect("couldn't free the context") };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn overflow_disabled_fails_when_too_large() {
        let big = "y".repeat(mapping_size() * 2);
        let ctx = ctx_with_payload(&big);
        let err = publish_with(&ctx, false).expect_err("should refuse to overflow when disabled");
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn inline_to_overflow_transition() {
        let small = ctx_with_payload("small");
        let big = ctx_with_payload(&"z".repeat(mapping_size() * 2));

        let mut handle = publish(&small).expect("publish small");
        assert!(payload_is_inline(&handle));
        let (base, len) = handle.mapping();

        handle.update(&big).expect("grow to overflow");
        assert!(!payload_is_inline(&handle));
        let (read_big, _) = read(base, len).expect("read overflow");
        assert_eq!(read_big, big);

        handle.update(&small).expect("shrink back to inline");
        assert!(payload_is_inline(&handle));
        let (read_small, _) = read(base, len).expect("read inline");
        assert_eq!(read_small, small);

        // SAFETY: no other reader is using the mapping in this test.
        unsafe { handle.free().expect("free") };
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn safe_read_valid_and_invalid() {
        // Valid buffer round-trips.
        let data = b"hello safe_read".to_vec();
        let copied = safe_read(data.as_ptr(), data.len()).expect("safe_read of valid buffer");
        assert_eq!(copied, data);

        // A deliberately-unmapped page must yield an error, not a crash.
        let page = mapping_size();
        // SAFETY: map then unmap a page to obtain a known-invalid address.
        let addr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(addr, libc::MAP_FAILED, "mmap for test failed");
        // SAFETY: addr/page came from mmap just above.
        assert_eq!(unsafe { libc::munmap(addr, page) }, 0, "munmap failed");
        let err = safe_read(addr.cast::<u8>().cast_const(), page)
            .expect_err("safe_read of unmapped memory should error");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn discoverable_and_readable_via_self_reader() {
        let ctx = ctx_with_payload("discover me");
        let handle = publish(&ctx).expect("publish");

        // The Linux discovery-based reader (reference impl) must find and decode it too.
        let reader = ProcessContextSelfReader::new().expect("mapping must be discoverable");
        let read_back = reader.read().expect("self reader read");
        assert_eq!(read_back, ctx);

        // SAFETY: the self reader above is dropped; no reader outlives this free.
        unsafe { handle.free().expect("free") };
        assert!(
            ProcessContextSelfReader::new().is_err(),
            "mapping should be gone after free"
        );
    }
}
