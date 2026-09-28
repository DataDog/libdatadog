// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Scaffolding for allocation and Linux thread CPU benchmarks.
//!
//! See `ReportingAllocator`, `memory_allocated_criterion`, and `ThreadCpuTime` for usage.

#![allow(missing_docs)]

use core::{alloc::GlobalAlloc, cell::Cell, time::Duration};
use std::alloc::System;

use criterion::{measurement::Measurement, Criterion, Throughput};

pub trait MeasurementName {
    fn name() -> &'static str;
}

impl MeasurementName for criterion::measurement::WallTime {
    fn name() -> &'static str {
        "wall_time"
    }
}

/// Criterion measurement using CPU time consumed by the current thread.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadCpuTime;

#[cfg(target_os = "linux")]
impl MeasurementName for ThreadCpuTime {
    fn name() -> &'static str {
        "thread_cpu"
    }
}

#[cfg(target_os = "linux")]
const WALL_TIME: criterion::measurement::WallTime = criterion::measurement::WallTime;

#[cfg(target_os = "linux")]
#[allow(
    clippy::panic,
    reason = "Criterion measurements cannot return clock errors"
)]
fn thread_cpu_time() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is initialized and its mutable pointer remains valid for the call.
    let result = unsafe {
        libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, core::ptr::addr_of_mut!(time))
    };
    assert_eq!(
        result,
        0,
        "read thread CPU time: {}",
        std::io::Error::last_os_error()
    );

    // A successful `clock_gettime` returns non-negative seconds and nanoseconds.
    Duration::from_secs(time.tv_sec as u64) + Duration::from_nanos(time.tv_nsec as u64)
}

#[cfg(target_os = "linux")]
impl Measurement for ThreadCpuTime {
    type Intermediate = Duration;
    type Value = Duration;

    fn start(&self) -> Self::Intermediate {
        thread_cpu_time()
    }

    fn end(&self, start: Self::Intermediate) -> Self::Value {
        thread_cpu_time() - start
    }

    fn add(&self, v1: &Self::Value, v2: &Self::Value) -> Self::Value {
        WALL_TIME.add(v1, v2)
    }

    fn zero(&self) -> Self::Value {
        WALL_TIME.zero()
    }

    fn to_f64(&self, value: &Self::Value) -> f64 {
        WALL_TIME.to_f64(value)
    }

    fn formatter(&self) -> &dyn criterion::measurement::ValueFormatter {
        WALL_TIME.formatter()
    }
}

/// `Criterion` for the allocation benchmarks. Sampling is deliberately small: allocation is
/// deterministic, so extra samples cannot refine a constant.
///
/// Not usable as `criterion_group!`'s `config =` -- that macro appends `.configure_from_args()`,
/// which would let command-line flags replace the settings below.
pub fn memory_allocated_criterion(
    global_alloc: &'static ReportingAllocator<System>,
) -> Criterion<AllocatedBytesMeasurement<System>> {
    Criterion::default()
        .with_measurement(AllocatedBytesMeasurement(Cell::new(false), global_alloc))
        .configure_from_args()
        .measurement_time(Duration::from_millis(1))
        .warm_up_time(Duration::from_millis(1))
        .without_plots()
        .plotting_backend(criterion::PlottingBackend::None)
        .sample_size(10)
}

#[derive(Debug)]
struct AllocStats {
    allocated_bytes: usize,
    #[allow(dead_code)]
    allocations: usize,
}

pub struct ReportingAllocator<T: GlobalAlloc> {
    alloc: T,
    allocated_bytes: core::sync::atomic::AtomicUsize,
    allocations: core::sync::atomic::AtomicUsize,
}

impl<T: GlobalAlloc> ReportingAllocator<T> {
    pub const fn new(alloc: T) -> Self {
        Self {
            alloc,
            allocated_bytes: core::sync::atomic::AtomicUsize::new(0),
            allocations: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn stats(&self) -> AllocStats {
        AllocStats {
            allocated_bytes: self
                .allocated_bytes
                .load(core::sync::atomic::Ordering::Relaxed),
            allocations: self.allocations.load(core::sync::atomic::Ordering::Relaxed),
        }
    }
}

unsafe impl<T: GlobalAlloc> GlobalAlloc for ReportingAllocator<T> {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        self.allocated_bytes
            .fetch_add(layout.size(), core::sync::atomic::Ordering::Relaxed);
        self.allocations
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        self.alloc.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        self.alloc.dealloc(ptr, layout);
    }
}

pub struct AllocatedBytesMeasurement<T: GlobalAlloc + 'static>(
    Cell<bool>,
    &'static ReportingAllocator<T>,
);

impl<T: GlobalAlloc> MeasurementName for AllocatedBytesMeasurement<T> {
    fn name() -> &'static str {
        "allocated_bytes"
    }
}

impl<T: GlobalAlloc> Measurement for AllocatedBytesMeasurement<T> {
    type Intermediate = usize;

    type Value = usize;

    fn start(&self) -> Self::Intermediate {
        self.1.stats().allocated_bytes
    }

    fn end(&self, i: Self::Intermediate) -> Self::Value {
        self.1.stats().allocated_bytes - i
    }

    fn add(&self, v1: &Self::Value, v2: &Self::Value) -> Self::Value {
        *v1 + *v2
    }

    fn zero(&self) -> Self::Value {
        0
    }

    fn to_f64(&self, value: &Self::Value) -> f64 {
        let b = self.0.get();
        self.0.set(!b);
        // Criterion does not handle all-identical measurement values well, and since
        // allocation is deterministic that tends to happen a lot. Add a small +/- epsilon
        // so each pair of measurements differs slightly without skewing the distribution.
        *value as f64 + if b { 0.01 } else { -0.01 }
    }

    fn formatter(&self) -> &dyn criterion::measurement::ValueFormatter {
        &AllocationFormatter
    }
}

struct AllocationFormatter;

impl criterion::measurement::ValueFormatter for AllocationFormatter {
    fn scale_values(&self, typical_value: f64, values: &mut [f64]) -> &'static str {
        let log_scale: f64 = typical_value.log10().round();
        if log_scale.is_infinite() || log_scale.is_nan() || log_scale < 0.0 {
            return "b";
        }
        let scale = (log_scale as i32 / 3).min(4);
        values.iter_mut().for_each(|v| *v /= 10_f64.powi(scale * 3));
        match scale {
            0 => "b",
            1 => "Kb",
            2 => "Mb",
            3 => "Gb",
            _ => "Tb",
        }
    }

    fn scale_throughputs(
        &self,
        _typical_value: f64,
        throughput: &criterion::Throughput,
        _values: &mut [f64],
    ) -> &'static str {
        match throughput {
            Throughput::Bytes(_) => "B/s",
            Throughput::BytesDecimal(_) => "B/s",
            Throughput::Elements(_) => "elements/s",
        }
    }

    fn scale_for_machines(&self, _values: &mut [f64]) -> &'static str {
        "b"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::alloc::{GlobalAlloc, Layout};
    use criterion::measurement::{Measurement, ValueFormatter};
    use std::alloc::System;

    static SHARED: ReportingAllocator<System> = ReportingAllocator::new(System);

    // --- ReportingAllocator ---

    #[test]
    fn new_starts_at_zero() {
        let a = ReportingAllocator::new(System);
        let s = a.stats();
        assert_eq!(s.allocated_bytes, 0);
        assert_eq!(s.allocations, 0);
    }

    #[test]
    fn alloc_increments_both_counters() {
        let a = ReportingAllocator::new(System);
        let layout = Layout::from_size_align(64, 8).unwrap();
        let ptr = unsafe { a.alloc(layout) };
        assert!(!ptr.is_null());
        assert_eq!(a.stats().allocated_bytes, 64);
        assert_eq!(a.stats().allocations, 1);
        unsafe { a.dealloc(ptr, layout) };
    }

    #[test]
    fn dealloc_does_not_change_counters() {
        let a = ReportingAllocator::new(System);
        let layout = Layout::from_size_align(32, 8).unwrap();
        let ptr = unsafe { a.alloc(layout) };
        let bytes_after_alloc = a.stats().allocated_bytes;
        unsafe { a.dealloc(ptr, layout) };
        assert_eq!(a.stats().allocated_bytes, bytes_after_alloc);
        assert_eq!(a.stats().allocations, 1);
    }

    #[test]
    fn multiple_allocs_accumulate() {
        let a = ReportingAllocator::new(System);
        let l1 = Layout::from_size_align(16, 8).unwrap();
        let l2 = Layout::from_size_align(32, 8).unwrap();
        let p1 = unsafe { a.alloc(l1) };
        let p2 = unsafe { a.alloc(l2) };
        assert_eq!(a.stats().allocated_bytes, 48);
        assert_eq!(a.stats().allocations, 2);
        unsafe {
            a.dealloc(p1, l1);
            a.dealloc(p2, l2);
        }
    }

    // --- AllocatedBytesMeasurement ---

    #[test]
    fn measurement_zero_and_add() {
        let m = AllocatedBytesMeasurement(Cell::new(false), &SHARED);
        assert_eq!(m.zero(), 0);
        assert_eq!(m.add(&100, &200), 300);
    }

    #[test]
    fn measurement_start_end_tracks_delta() {
        let m = AllocatedBytesMeasurement(Cell::new(false), &SHARED);
        let start = m.start();
        let layout = Layout::from_size_align(256, 8).unwrap();
        let ptr = unsafe { SHARED.alloc(layout) };
        // Other tests may also allocate via SHARED concurrently, so allow >= 256.
        assert!(m.end(start) >= 256);
        unsafe { SHARED.dealloc(ptr, layout) };
    }

    #[test]
    fn measurement_to_f64_alternates_epsilon() {
        let m = AllocatedBytesMeasurement(Cell::new(false), &SHARED);
        // Initial state: Cell = false → first result is value - 0.01
        assert!((m.to_f64(&1000) - 999.99).abs() < 1e-9);
        // After first call: Cell = true → result is value + 0.01
        assert!((m.to_f64(&1000) - 1000.01).abs() < 1e-9);
        // Alternates back
        assert!((m.to_f64(&1000) - 999.99).abs() < 1e-9);
    }

    #[test]
    fn measurement_name() {
        assert_eq!(
            AllocatedBytesMeasurement::<System>::name(),
            "allocated_bytes"
        );
    }

    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)] // Miri does not support CLOCK_THREAD_CPUTIME_ID.
    #[test]
    fn thread_cpu_measurement() {
        let measurement = ThreadCpuTime;
        let start = measurement.start();
        let _elapsed = measurement.end(start);

        assert_eq!(ThreadCpuTime::name(), "thread_cpu");
        assert_eq!(measurement.zero(), Duration::ZERO);
        assert_eq!(
            measurement.add(&Duration::from_nanos(2), &Duration::from_nanos(3)),
            Duration::from_nanos(5)
        );
        assert_eq!(measurement.to_f64(&Duration::from_micros(2)), 2_000.0);

        let mut values = [2_000.0];
        assert_eq!(
            measurement.formatter().scale_for_machines(&mut values),
            "ns"
        );
    }

    // --- AllocationFormatter::scale_values ---

    #[test]
    fn scale_values_zero_returns_bytes() {
        let f = AllocationFormatter;
        let mut v = [42.0_f64];
        assert_eq!(f.scale_values(0.0, &mut v), "b");
    }

    #[test]
    fn scale_values_sub_byte_returns_bytes() {
        let f = AllocationFormatter;
        let mut v = [0.5_f64];
        // log10(0.1) = -1 → negative → "b"
        assert_eq!(f.scale_values(0.1, &mut v), "b");
    }

    #[test]
    fn scale_values_bytes() {
        let f = AllocationFormatter;
        let mut v = [1.0_f64];
        assert_eq!(f.scale_values(1.0, &mut v), "b");
        assert!((v[0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn scale_values_kilobytes() {
        let f = AllocationFormatter;
        let mut v = [2000.0_f64];
        assert_eq!(f.scale_values(1000.0, &mut v), "Kb");
        assert!((v[0] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn scale_values_megabytes() {
        let f = AllocationFormatter;
        let mut v = [3_000_000.0_f64];
        assert_eq!(f.scale_values(1_000_000.0, &mut v), "Mb");
        assert!((v[0] - 3.0).abs() < 1e-9);
    }

    #[test]
    fn scale_values_gigabytes() {
        let f = AllocationFormatter;
        let mut v = [4_000_000_000.0_f64];
        assert_eq!(f.scale_values(1_000_000_000.0, &mut v), "Gb");
        assert!((v[0] - 4.0).abs() < 1e-9);
    }

    #[test]
    fn scale_values_terabytes() {
        let f = AllocationFormatter;
        let mut v = [5_000_000_000_000.0_f64];
        assert_eq!(f.scale_values(1_000_000_000_000.0, &mut v), "Tb");
        assert!((v[0] - 5.0).abs() < 1e-9);
    }

    #[test]
    fn scale_values_very_large_clamps_to_terabytes() {
        let f = AllocationFormatter;
        let mut v = [1e18_f64];
        assert_eq!(f.scale_values(1e18, &mut v), "Tb");
    }

    #[test]
    fn scale_for_machines_returns_bytes_unit() {
        let f = AllocationFormatter;
        let mut v = [1000.0_f64];
        assert_eq!(f.scale_for_machines(&mut v), "b");
    }
}
