// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Scaffolding for memory usage benchmarks.
//!
//! See the `ReportingAllocator` type and `memory_allocated_criterion` for usage.

#![allow(missing_docs)]

use core::{alloc::GlobalAlloc, cell::Cell, time::Duration};
use std::alloc::System;

use criterion::measurement::Measurement;
use criterion::{Criterion, Throughput};

pub trait MeasurementName {
    fn name() -> &'static str;
}

impl MeasurementName for criterion::measurement::WallTime {
    fn name() -> &'static str {
        "wall_time"
    }
}

/// `Criterion` for synchronous workloads that allocate. When the expected result is zero, use
/// [`ReportingAllocator::assert_no_allocations`]: Criterion cannot analyze a zero measurement.
///
/// Not usable as `criterion_group!`'s `config =` -- that macro appends `.configure_from_args()`,
/// which would let command-line flags replace the settings below.
pub fn memory_allocated_criterion(
    global_alloc: &'static ReportingAllocator<System>,
) -> Criterion<AllocatedBytesMeasurement<System>> {
    Criterion::default()
        .with_measurement(AllocatedBytesMeasurement(global_alloc))
        .configure_from_args()
        .measurement_time(Duration::from_millis(1))
        .warm_up_time(Duration::from_millis(1))
        .without_plots()
        .plotting_backend(criterion::PlottingBackend::None)
        .sample_size(10)
}

thread_local! {
    static ALLOCATED_BYTES: Cell<Option<usize>> = const { Cell::new(None) };
}

/// A global allocator that counts requested bytes only on the measuring thread and only while
/// an allocation measurement is active. Timing runs do not update allocation counters.
pub struct ReportingAllocator<T: GlobalAlloc>(T);

impl<T: GlobalAlloc> ReportingAllocator<T> {
    pub const fn new(alloc: T) -> Self {
        Self(alloc)
    }

    fn start(&self) {
        ALLOCATED_BYTES.with(|counter| {
            assert!(counter.get().is_none(), "nested allocation measurement");
            counter.set(Some(0));
        });
    }

    fn end(&self) -> usize {
        ALLOCATED_BYTES.with(|counter| counter.take().expect("allocation measurement not started"))
    }

    pub fn assert_no_allocations(&self, scenario: &str, run: impl FnOnce()) {
        self.start();
        run();
        assert_eq!(self.end(), 0, "{scenario} must not allocate");
    }
}

unsafe impl<T: GlobalAlloc> GlobalAlloc for ReportingAllocator<T> {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let _tls_may_be_destroyed = ALLOCATED_BYTES.try_with(|counter| {
            if let Some(bytes) = counter.get() {
                counter.set(Some(bytes.wrapping_add(layout.size())));
            }
        });
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        unsafe {
            self.0.dealloc(ptr, layout);
        }
    }
}

pub struct AllocatedBytesMeasurement<T: GlobalAlloc + 'static>(&'static ReportingAllocator<T>);

impl<T: GlobalAlloc> MeasurementName for AllocatedBytesMeasurement<T> {
    fn name() -> &'static str {
        "allocated_bytes"
    }
}

impl<T: GlobalAlloc> Measurement for AllocatedBytesMeasurement<T> {
    type Intermediate = ();

    type Value = usize;

    fn start(&self) -> Self::Intermediate {
        self.0.start();
    }

    fn end(&self, (): Self::Intermediate) -> Self::Value {
        self.0.end()
    }

    fn add(&self, v1: &Self::Value, v2: &Self::Value) -> Self::Value {
        *v1 + *v2
    }

    fn zero(&self) -> Self::Value {
        0
    }

    fn to_f64(&self, value: &Self::Value) -> f64 {
        assert_ne!(
            *value, 0,
            "use assert_no_allocations for zero-byte workloads"
        );
        *value as f64
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

    fn allocate(bytes: usize) {
        let layout = Layout::from_size_align(bytes, 8).unwrap();
        let ptr = unsafe { SHARED.alloc(layout) };
        assert!(!ptr.is_null());
        unsafe { SHARED.dealloc(ptr, layout) };
    }

    #[test]
    fn measurement_zero_and_add() {
        let measurement = AllocatedBytesMeasurement(&SHARED);
        assert_eq!(measurement.zero(), 0);
        assert_eq!(measurement.add(&100, &200), 300);
    }

    #[test]
    fn measurement_tracks_only_allocations_in_window_on_current_thread() {
        let measurement = AllocatedBytesMeasurement(&SHARED);
        allocate(32);
        measurement.start();
        std::thread::spawn(|| allocate(128)).join().unwrap();
        allocate(64);
        allocate(256);
        assert_eq!(measurement.end(()), 320);
        SHARED.assert_no_allocations("empty workload", || {});
    }

    #[test]
    fn measurement_reports_exact_bytes() {
        let measurement = AllocatedBytesMeasurement(&SHARED);
        assert_eq!(measurement.to_f64(&1000), 1000.0);
        assert_eq!(measurement.to_f64(&1000), 1000.0);
    }

    #[test]
    #[should_panic(expected = "allocating workload must not allocate")]
    fn zero_allocation_check_detects_allocations() {
        SHARED.assert_no_allocations("allocating workload", || allocate(32));
    }

    #[test]
    fn measurement_name() {
        assert_eq!(
            AllocatedBytesMeasurement::<System>::name(),
            "allocated_bytes"
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
