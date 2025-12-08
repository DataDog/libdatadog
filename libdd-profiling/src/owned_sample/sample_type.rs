// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use enum_map::Enum;

/// Types of profiling values that can be collected.
///
/// Each variant corresponds to a specific metric value in a profiling sample.
/// Some high-level sample types (like CPU, Wall, GPU) have multiple associated values.
///
/// Based on the sample types and ValueIndex from [dd-trace-py](https://github.com/DataDog/dd-trace-py/blob/main/ddtrace/internal/datadog/profiling/dd_wrapper/include/types.hpp).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Enum)]
pub enum SampleType {
    // CPU profiling - 2 values
    /// CPU time spent
    CpuTime,
    /// Number of CPU samples
    CpuCount,

    // Wall clock profiling - 2 values
    /// Wall clock time spent
    WallTime,
    /// Number of wall clock samples
    WallCount,

    // Exception tracking - 1 value
    /// Number of exceptions
    ExceptionCount,

    // Lock acquisition profiling - 2 values
    /// Time spent acquiring locks
    LockAcquireTime,
    /// Number of lock acquisitions
    LockAcquireCount,

    // Lock release profiling - 2 values
    /// Time spent releasing locks
    LockReleaseTime,
    /// Number of lock releases
    LockReleaseCount,

    // Memory allocation profiling - 2 values
    /// Allocated space in bytes
    AllocSpace,
    /// Number of allocations
    AllocCount,

    // Heap profiling - 1 value
    /// Heap space in bytes
    HeapSpace,

    // GPU profiling - 6 values
    /// GPU time spent
    GpuTime,
    /// Number of GPU samples
    GpuCount,
    /// GPU allocated space in bytes
    GpuAllocSpace,
    /// Number of GPU allocations
    GpuAllocCount,
    /// GPU floating point operations
    GpuFlops,
    /// Number of GPU FLOPS samples
    GpuFlopsSamples,
}
