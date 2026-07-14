// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Implements the publication strategy for Windows.
//! This is not part of the OTEL process context specification, which deals only with Linux.

use core::{
    ptr::{self, NonNull},
    sync::atomic::{fence, AtomicPtr, AtomicU32, AtomicU64, Ordering},
};
use std::{io, sync::OnceLock};

use super::super::UNPUBLISHED_OR_UPDATING;
use super::{HeaderMemoryHolder, MappingHeader, MonotonicTime};

#[no_mangle]
#[allow(non_upper_case_globals)]
pub static otel_process_ctx_v2: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(performance_count: *mut i64) -> i32;
    fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
}

pub(super) struct HeapHeader {
    header: Box<MappingHeader>,
}

pub(super) struct MonotonicClock;

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
        otel_process_ctx_v2.store(
            ptr::from_ref(self.header.as_ref()).cast_mut().cast(),
            Ordering::Release,
        );
    }

    fn unpublish_and_release(self) -> io::Result<()> {
        otel_process_ctx_v2.store(ptr::null_mut(), Ordering::Relaxed);
        fence(Ordering::SeqCst);
        drop(self);
        Ok(())
    }

    fn after_fork(self) {}
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

fn last_error(context: &'static str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{context}: {error}"))
}
