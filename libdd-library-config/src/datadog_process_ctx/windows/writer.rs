// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Implements the Datadog publication strategy for Windows.

use core::{
    ptr::{self, NonNull},
    sync::atomic::{fence, AtomicPtr, AtomicU32, AtomicU64, Ordering},
};
use std::{io, sync::OnceLock};

use crate::otel_process_ctx::{
    last_error,
    writer::{HeaderMemoryHolder, MappingHeader, MonotonicTime, WriterBackend},
    UNPUBLISHED_OR_UPDATING,
};

pub(crate) struct WindowsWriterBackend;

impl WriterBackend for WindowsWriterBackend {
    type HeaderMemory = HeapHeader;
    type Clock = MonotonicClock;
}

#[cfg(target_env = "msvc")]
#[used]
#[link_section = ".drectve"]
static EXPORT_DATADOG_PROCESS_CTX_V1: [u8; 31] = *b" /EXPORT:datadog_process_ctx_v1";

#[cfg(target_env = "gnu")]
#[used]
#[link_section = ".drectve"]
static EXPORT_DATADOG_PROCESS_CTX_V1: [u8; 31] = *b" -export:datadog_process_ctx_v1";

#[no_mangle]
#[allow(non_upper_case_globals)]
pub static datadog_process_ctx_v1: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(performance_count: *mut i64) -> i32;
    fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
}

pub(crate) struct HeapHeader {
    header: Box<MappingHeader>,
}

pub(crate) struct MonotonicClock;

impl HeaderMemoryHolder for HeapHeader {
    fn new() -> io::Result<Self> {
        Ok(Self {
            header: Box::new(MappingHeader {
                signature: [0; 8],
                version: 0,
                payload_size: AtomicU32::new(0),
                monotonic_published_at_ns: AtomicU64::new(UNPUBLISHED_OR_UPDATING),
                payload_ptr: AtomicPtr::new(ptr::null_mut()),
            }),
        })
    }

    fn as_ptr(&self) -> Option<NonNull<MappingHeader>> {
        Some(NonNull::from(self.header.as_ref()))
    }

    fn make_discoverable(&mut self) {
        datadog_process_ctx_v1.store(
            ptr::from_ref(self.header.as_ref()).cast_mut().cast(),
            Ordering::Release,
        );
    }

    fn unpublish_and_release(self) -> io::Result<()> {
        datadog_process_ctx_v1.store(ptr::null_mut(), Ordering::Relaxed);
        fence(Ordering::SeqCst);
        drop(self);
        Ok(())
    }
}

impl MonotonicTime for MonotonicClock {
    fn monotonic_time_ns() -> io::Result<u64> {
        let frequency = performance_frequency()?;
        let mut ticks = 0;
        // SAFETY: ticks is a valid writable LARGE_INTEGER-compatible value.
        if unsafe { QueryPerformanceCounter(&mut ticks) } == 0 {
            return Err(last_error("failed to query monotonic process context time"));
        }
        let ticks = u64::try_from(ticks)
            .map_err(|_| io::Error::other("monotonic process context time was negative"))?;
        let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(frequency);
        u64::try_from(nanos)
            .map(|nanos| nanos.max(1))
            .map_err(|_| io::Error::other("monotonic process context timestamp overflowed"))
    }
}

fn performance_frequency() -> io::Result<u64> {
    static FREQUENCY: OnceLock<u64> = OnceLock::new();

    if let Some(frequency) = FREQUENCY.get() {
        return Ok(*frequency);
    }

    let mut frequency = 0;
    // SAFETY: frequency is a valid writable LARGE_INTEGER-compatible value.
    if unsafe { QueryPerformanceFrequency(&mut frequency) } == 0 {
        return Err(last_error(
            "failed to query process context performance-counter frequency",
        ));
    }
    let frequency = u64::try_from(frequency)
        .ok()
        .filter(|frequency| *frequency != 0)
        .ok_or_else(|| io::Error::other("process context performance-counter frequency is zero"))?;

    let _ = FREQUENCY.set(frequency);
    Ok(FREQUENCY.get().copied().unwrap_or(frequency))
}

#[cfg(test)]
mod tests {
    use core::{ffi::c_void, ptr};

    use super::datadog_process_ctx_v1;

    type Handle = *mut c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(module_name: *const u16) -> Handle;
        fn GetProcAddress(module: Handle, proc_name: *const u8) -> *mut c_void;
    }

    #[test]
    fn exports_process_context_global() {
        // SAFETY: a null module name requests the current executable's module handle.
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        assert!(!module.is_null());

        // SAFETY: module is the current executable and the symbol name is NUL-terminated.
        let symbol = unsafe { GetProcAddress(module, c"datadog_process_ctx_v1".as_ptr().cast()) };
        assert_eq!(
            symbol.cast_const(),
            ptr::from_ref(&datadog_process_ctx_v1).cast::<c_void>()
        );
    }
}
