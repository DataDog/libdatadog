// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Implements the publication strategy for MacOS.
//! This is not part of the OTEL process context specification, which deals only with Linux.

use core::{
    ffi::c_void,
    mem::forget,
    ptr::{self, NonNull},
    sync::atomic::{fence, Ordering},
};
use std::io;

use super::{HeaderMemoryHolder, MappingHeader, MonotonicTime};
use crate::otel_process_ctx::{
    last_error,
    macos::{AtomicPublishedHeader, PUBLISHER_PID_SHIFT},
};

// A child inherits this global after fork even when minherit excludes the header mapping. Store
// the publisher PID with the pointer so the child cannot discover its parent's unmapped address.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static otel_process_ctx_v2: AtomicPublishedHeader = AtomicPublishedHeader::new(0);

// From <mach/vm_inherit.h>; the libc crate does not expose this constant.
const VM_INHERIT_NONE: libc::c_int = 2;

// SAFETY: these signatures match their documentation.
unsafe extern "C" {
    fn minherit(address: *mut c_void, size: usize, inheritance: libc::c_int) -> libc::c_int;
    fn clock_gettime_nsec_np(clock_id: libc::clockid_t) -> u64;
}

pub(super) struct VmRegion {
    start_addr: NonNull<c_void>,
    /// `Some(pid)` when `VM_INHERIT_NONE` succeeded, otherwise `None`.
    only_for_pid: Option<u32>,
}

pub(super) struct MonotonicClock;

// SAFETY: VmRegion exclusively owns its mapping, which may be unmapped from any thread.
unsafe impl Send for VmRegion {}

impl HeaderMemoryHolder for VmRegion {
    fn new() -> io::Result<Self> {
        let size = super::mapping_size();
        let prot = libc::PROT_READ | libc::PROT_WRITE;
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
        // SAFETY: it's always safe to call mmap with null as it will not
        // override any extant mappings.
        let address = unsafe { libc::mmap(ptr::null_mut(), size, prot, flags, -1, 0) };
        if address == libc::MAP_FAILED {
            return Err(last_error("failed to allocate process context header"));
        }

        // SAFETY: minherit is safe, and the parameters are valid anyhow.
        // Failure is harmless; the mapping then follows the default
        // inheritance behavior.
        let only_for_pid = (unsafe { minherit(address, size, VM_INHERIT_NONE) } == 0)
            .then_some(std::process::id());

        Ok(Self {
            // SAFETY: POSIX guarantees that when not using MAP_FIXED that the
            // returned address will not start at zero.
            start_addr: unsafe { NonNull::new_unchecked(address) },
            only_for_pid,
        })
    }

    fn as_ptr(&self) -> Option<NonNull<MappingHeader>> {
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
        let value = pack_published_header(std::process::id(), self.start_addr.as_ptr().cast());
        otel_process_ctx_v2.store(value, Ordering::Release);
    }

    fn unpublish_and_release(mut self) -> io::Result<()> {
        otel_process_ctx_v2.store(0, Ordering::Relaxed);
        // Make it slightly more likely that a reader will observe the unavailability.
        fence(Ordering::SeqCst);
        self.unmap()?;
        forget(self);
        Ok(())
    }

    fn after_fork(self) {
        drop(self);
    }
}

fn pack_published_header(publisher_pid: u32, header: *mut u8) -> u128 {
    (u128::from(publisher_pid) << PUBLISHER_PID_SHIFT) | header.expose_provenance() as u128
}

impl MonotonicTime for MonotonicClock {
    fn monotonic_time_ns() -> io::Result<u64> {
        // SAFETY: CLOCK_MONOTONIC_RAW is a valid clock ID and this function has no pointer
        // arguments. It returns continuous time directly in nanoseconds.
        Ok(unsafe { clock_gettime_nsec_np(libc::CLOCK_MONOTONIC_RAW) }.max(1))
    }
}

impl VmRegion {
    fn unmap(&mut self) -> io::Result<()> {
        if self
            .only_for_pid
            .is_some_and(|pid| pid != std::process::id())
        {
            return Ok(());
        }

        // SAFETY: start_addr owns the live mapping and this method is called at most once unless
        // munmap fails.
        if unsafe { libc::munmap(self.start_addr.as_ptr(), super::mapping_size()) } != 0 {
            return Err(last_error("failed to free process context header"));
        }
        Ok(())
    }
}

impl Drop for VmRegion {
    fn drop(&mut self) {
        let _ = self.unmap();
    }
}
