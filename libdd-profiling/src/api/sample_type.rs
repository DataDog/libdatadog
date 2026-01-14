// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ValueType;

/// Sample types supported by Datadog's profilers.
///
/// Variants are sourced from:
/// - **dd-trace-rb**: `stack_recorder.c` (`all_value_types`)
/// - **dd-trace-py**: `dd_wrapper/include/types.hpp`
/// - **dd-trace-php**: `profiling/src/profiling/samples.rs` (I/O profiling)
/// - **dd-trace-dotnet**: Sample type definitions for allocations, locks, CPU, walltime,
///   exceptions, live objects, HTTP requests
/// - **pprof-nodejs**: `profile-serializer.ts` (value type functions)
#[cfg_attr(test, derive(bolero::generator::TypeGenerator, strum::EnumIter))]
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SampleType {
    AllocSamples,
    AllocSamplesUnscaled,
    AllocSize,
    AllocSpace,
    CpuTime,
    CpuSamples,
    /// Legacy: Use `CpuTime` instead for consistency with naming scheme
    CpuLegacy,
    /// Legacy: Use `CpuSamples` instead for consistency with naming scheme
    CpuSampleLegacy,
    ExceptionSamples,
    /// Legacy: Use `ExceptionSamples` instead for consistency with naming scheme
    ExceptionLegacy,
    FileIoReadSize,
    FileIoReadSizeSamples,
    FileIoReadTime,
    FileIoReadTimeSamples,
    FileIoWriteSize,
    FileIoWriteSizeSamples,
    FileIoWriteTime,
    FileIoWriteTimeSamples,
    GpuAllocSamples,
    GpuFlops,
    GpuFlopsSamples,
    GpuSamples,
    GpuSpace,
    GpuTime,
    HeapLiveSamples,
    HeapLiveSize,
    HeapSpace,
    InuseObjects,
    InuseSpace,
    LockAcquire,
    LockAcquireWait,
    LockCount,
    LockRelease,
    LockReleaseHold,
    LockTime,
    /// Legacy: Use `InuseObjects` instead for consistency with naming scheme
    ObjectsLegacy,
    RequestTime,
    Sample,
    SocketReadSize,
    SocketReadSizeSamples,
    SocketReadTime,
    SocketReadTimeSamples,
    SocketWriteSize,
    SocketWriteSizeSamples,
    SocketWriteTime,
    SocketWriteTimeSamples,
    /// Legacy: Use specific space variants (`InuseSpace`, `HeapSpace`, `AllocSpace`) instead
    SpaceLegacy,
    Timeline,
    WallSamples,
    WallTime,
    /// Legacy: Use `WallTime` instead for consistency with naming scheme
    WallLegacy,

    // Experimental sample types for testing and development.
    ExperimentalCount,
    ExperimentalNanoseconds,
    ExperimentalBytes,
}

impl From<&SampleType> for ValueType<'static> {
    #[inline(always)]
    fn from(sample_type: &SampleType) -> Self {
        (*sample_type).into()
    }
}

impl From<SampleType> for ValueType<'static> {
    #[inline(always)]
    fn from(sample_type: SampleType) -> Self {
        match sample_type {
            SampleType::AllocSamples => ValueType::new("alloc-samples", "count"),
            SampleType::AllocSamplesUnscaled => ValueType::new("alloc-samples-unscaled", "count"),
            SampleType::AllocSize => ValueType::new("alloc-size", "bytes"),
            SampleType::AllocSpace => ValueType::new("alloc-space", "bytes"),
            SampleType::CpuTime => ValueType::new("cpu-time", "nanoseconds"),
            SampleType::CpuSamples => ValueType::new("cpu-samples", "count"),
            SampleType::CpuLegacy => ValueType::new("cpu", "nanoseconds"),
            SampleType::CpuSampleLegacy => ValueType::new("cpu-sample", "count"),
            SampleType::ExceptionSamples => ValueType::new("exception-samples", "count"),
            SampleType::ExceptionLegacy => ValueType::new("exception", "count"),
            SampleType::FileIoReadSize => ValueType::new("file-io-read-size", "bytes"),
            SampleType::FileIoReadSizeSamples => {
                ValueType::new("file-io-read-size-samples", "count")
            }
            SampleType::FileIoReadTime => ValueType::new("file-io-read-time", "nanoseconds"),
            SampleType::FileIoReadTimeSamples => {
                ValueType::new("file-io-read-time-samples", "count")
            }
            SampleType::FileIoWriteSize => ValueType::new("file-io-write-size", "bytes"),
            SampleType::FileIoWriteSizeSamples => {
                ValueType::new("file-io-write-size-samples", "count")
            }
            SampleType::FileIoWriteTime => ValueType::new("file-io-write-time", "nanoseconds"),
            SampleType::FileIoWriteTimeSamples => {
                ValueType::new("file-io-write-time-samples", "count")
            }
            SampleType::GpuAllocSamples => ValueType::new("gpu-alloc-samples", "count"),
            SampleType::GpuFlops => ValueType::new("gpu-flops", "count"),
            SampleType::GpuFlopsSamples => ValueType::new("gpu-flops-samples", "count"),
            SampleType::GpuSamples => ValueType::new("gpu-samples", "count"),
            SampleType::GpuSpace => ValueType::new("gpu-space", "bytes"),
            SampleType::GpuTime => ValueType::new("gpu-time", "nanoseconds"),
            SampleType::HeapLiveSamples => ValueType::new("heap-live-samples", "count"),
            SampleType::HeapLiveSize => ValueType::new("heap-live-size", "bytes"),
            SampleType::HeapSpace => ValueType::new("heap-space", "bytes"),
            SampleType::InuseObjects => ValueType::new("inuse-objects", "count"),
            SampleType::InuseSpace => ValueType::new("inuse-space", "bytes"),
            SampleType::LockAcquire => ValueType::new("lock-acquire", "count"),
            SampleType::LockAcquireWait => ValueType::new("lock-acquire-wait", "nanoseconds"),
            SampleType::LockCount => ValueType::new("lock-count", "count"),
            SampleType::LockRelease => ValueType::new("lock-release", "count"),
            SampleType::LockReleaseHold => ValueType::new("lock-release-hold", "nanoseconds"),
            SampleType::LockTime => ValueType::new("lock-time", "nanoseconds"),
            SampleType::ObjectsLegacy => ValueType::new("objects", "count"),
            SampleType::RequestTime => ValueType::new("request-time", "nanoseconds"),
            SampleType::Sample => ValueType::new("sample", "count"),
            SampleType::SocketReadSize => ValueType::new("socket-read-size", "bytes"),
            SampleType::SocketReadSizeSamples => {
                ValueType::new("socket-read-size-samples", "count")
            }
            SampleType::SocketReadTime => ValueType::new("socket-read-time", "nanoseconds"),
            SampleType::SocketReadTimeSamples => {
                ValueType::new("socket-read-time-samples", "count")
            }
            SampleType::SocketWriteSize => ValueType::new("socket-write-size", "bytes"),
            SampleType::SocketWriteSizeSamples => {
                ValueType::new("socket-write-size-samples", "count")
            }
            SampleType::SocketWriteTime => ValueType::new("socket-write-time", "nanoseconds"),
            SampleType::SocketWriteTimeSamples => {
                ValueType::new("socket-write-time-samples", "count")
            }
            SampleType::SpaceLegacy => ValueType::new("space", "bytes"),
            SampleType::Timeline => ValueType::new("timeline", "nanoseconds"),
            SampleType::WallSamples => ValueType::new("wall-samples", "count"),
            SampleType::WallTime => ValueType::new("wall-time", "nanoseconds"),
            SampleType::WallLegacy => ValueType::new("wall", "nanoseconds"),
            SampleType::ExperimentalCount => ValueType::new("experimental-count", "count"),
            SampleType::ExperimentalNanoseconds => {
                ValueType::new("experimental-nanoseconds", "nanoseconds")
            }
            SampleType::ExperimentalBytes => ValueType::new("experimental-bytes", "bytes"),
        }
    }
}

impl<'a> TryFrom<ValueType<'a>> for SampleType {
    type Error = anyhow::Error;

    fn try_from(vt: ValueType<'a>) -> Result<Self, Self::Error> {
        Ok(match (vt.r#type, vt.unit) {
            ("alloc-samples", "count") => SampleType::AllocSamples,
            ("alloc-samples-unscaled", "count") => SampleType::AllocSamplesUnscaled,
            ("alloc-size", "bytes") => SampleType::AllocSize,
            ("alloc-space", "bytes") => SampleType::AllocSpace,
            ("cpu-time", "nanoseconds") => SampleType::CpuTime,
            ("cpu-samples", "count") => SampleType::CpuSamples,
            ("cpu", "nanoseconds") => SampleType::CpuLegacy,
            ("cpu-sample", "count") => SampleType::CpuSampleLegacy,
            ("exception-samples", "count") => SampleType::ExceptionSamples,
            ("exception", "count") => SampleType::ExceptionLegacy,
            ("file-io-read-size", "bytes") => SampleType::FileIoReadSize,
            ("file-io-read-size-samples", "count") => SampleType::FileIoReadSizeSamples,
            ("file-io-read-time", "nanoseconds") => SampleType::FileIoReadTime,
            ("file-io-read-time-samples", "count") => SampleType::FileIoReadTimeSamples,
            ("file-io-write-size", "bytes") => SampleType::FileIoWriteSize,
            ("file-io-write-size-samples", "count") => SampleType::FileIoWriteSizeSamples,
            ("file-io-write-time", "nanoseconds") => SampleType::FileIoWriteTime,
            ("file-io-write-time-samples", "count") => SampleType::FileIoWriteTimeSamples,
            ("gpu-alloc-samples", "count") => SampleType::GpuAllocSamples,
            ("gpu-flops", "count") => SampleType::GpuFlops,
            ("gpu-flops-samples", "count") => SampleType::GpuFlopsSamples,
            ("gpu-samples", "count") => SampleType::GpuSamples,
            ("gpu-space", "bytes") => SampleType::GpuSpace,
            ("gpu-time", "nanoseconds") => SampleType::GpuTime,
            ("heap-live-samples", "count") => SampleType::HeapLiveSamples,
            ("heap-live-size", "bytes") => SampleType::HeapLiveSize,
            ("heap-space", "bytes") => SampleType::HeapSpace,
            ("inuse-objects", "count") => SampleType::InuseObjects,
            ("inuse-space", "bytes") => SampleType::InuseSpace,
            ("lock-acquire", "count") => SampleType::LockAcquire,
            ("lock-acquire-wait", "nanoseconds") => SampleType::LockAcquireWait,
            ("lock-count", "count") => SampleType::LockCount,
            ("lock-release", "count") => SampleType::LockRelease,
            ("lock-release-hold", "nanoseconds") => SampleType::LockReleaseHold,
            ("lock-time", "nanoseconds") => SampleType::LockTime,
            ("objects", "count") => SampleType::ObjectsLegacy,
            ("request-time", "nanoseconds") => SampleType::RequestTime,
            ("sample", "count") => SampleType::Sample,
            ("socket-read-size", "bytes") => SampleType::SocketReadSize,
            ("socket-read-size-samples", "count") => SampleType::SocketReadSizeSamples,
            ("socket-read-time", "nanoseconds") => SampleType::SocketReadTime,
            ("socket-read-time-samples", "count") => SampleType::SocketReadTimeSamples,
            ("socket-write-size", "bytes") => SampleType::SocketWriteSize,
            ("socket-write-size-samples", "count") => SampleType::SocketWriteSizeSamples,
            ("socket-write-time", "nanoseconds") => SampleType::SocketWriteTime,
            ("socket-write-time-samples", "count") => SampleType::SocketWriteTimeSamples,
            ("space", "bytes") => SampleType::SpaceLegacy,
            ("timeline", "nanoseconds") => SampleType::Timeline,
            ("wall-samples", "count") => SampleType::WallSamples,
            ("wall-time", "nanoseconds") => SampleType::WallTime,
            ("wall", "nanoseconds") => SampleType::WallLegacy,
            ("experimental-count", "count") => SampleType::ExperimentalCount,
            ("experimental-nanoseconds", "nanoseconds") => SampleType::ExperimentalNanoseconds,
            ("experimental-bytes", "bytes") => SampleType::ExperimentalBytes,
            _ => anyhow::bail!("Unknown sample type: ({}, {})", vt.r#type, vt.unit),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn sample_type_round_trip_conversion() {
        // Test that converting SampleType -> ValueType -> SampleType gives the same result
        // Uses strum::EnumIter to automatically iterate over all variants
        for original in SampleType::iter() {
            let value_type: ValueType = original.into();
            let round_trip: SampleType = value_type.try_into().expect("round-trip conversion");
            assert_eq!(original, round_trip);
        }
    }

    #[test]
    fn value_type_to_sample_type_unknown_type() {
        // Test that unknown type/unit combinations fail gracefully
        let unknown = ValueType::new("unknown-type", "count");
        let result: Result<SampleType, _> = unknown.try_into();
        assert!(result.is_err(), "Unknown type should fail to parse");
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown sample type"));

        let invalid_unit = ValueType::new("cpu-time", "count");
        let result: Result<SampleType, _> = invalid_unit.try_into();
        assert!(
            result.is_err(),
            "Invalid unit for known type should fail to parse"
        );
    }
}
