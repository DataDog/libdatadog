// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ffi;
use crate::api;

impl TryFrom<ffi::SampleType> for api::SampleType {
    type Error = anyhow::Error;

    fn try_from(st: ffi::SampleType) -> Result<Self, Self::Error> {
        Ok(match st {
            ffi::SampleType::AllocSamples => api::SampleType::AllocSamples,
            ffi::SampleType::AllocSamplesUnscaled => api::SampleType::AllocSamplesUnscaled,
            ffi::SampleType::AllocSize => api::SampleType::AllocSize,
            ffi::SampleType::AllocSpace => api::SampleType::AllocSpace,
            ffi::SampleType::CpuTime => api::SampleType::CpuTime,
            ffi::SampleType::CpuSamples => api::SampleType::CpuSamples,
            ffi::SampleType::CpuLegacy => api::SampleType::CpuLegacy,
            ffi::SampleType::CpuSampleLegacy => api::SampleType::CpuSampleLegacy,
            ffi::SampleType::ExceptionSamples => api::SampleType::ExceptionSamples,
            ffi::SampleType::ExceptionLegacy => api::SampleType::ExceptionLegacy,
            ffi::SampleType::FileIoReadSize => api::SampleType::FileIoReadSize,
            ffi::SampleType::FileIoReadSizeSamples => api::SampleType::FileIoReadSizeSamples,
            ffi::SampleType::FileIoReadTime => api::SampleType::FileIoReadTime,
            ffi::SampleType::FileIoReadTimeSamples => api::SampleType::FileIoReadTimeSamples,
            ffi::SampleType::FileIoWriteSize => api::SampleType::FileIoWriteSize,
            ffi::SampleType::FileIoWriteSizeSamples => api::SampleType::FileIoWriteSizeSamples,
            ffi::SampleType::FileIoWriteTime => api::SampleType::FileIoWriteTime,
            ffi::SampleType::FileIoWriteTimeSamples => api::SampleType::FileIoWriteTimeSamples,
            ffi::SampleType::GpuAllocSamples => api::SampleType::GpuAllocSamples,
            ffi::SampleType::GpuFlops => api::SampleType::GpuFlops,
            ffi::SampleType::GpuFlopsSamples => api::SampleType::GpuFlopsSamples,
            ffi::SampleType::GpuSamples => api::SampleType::GpuSamples,
            ffi::SampleType::GpuSpace => api::SampleType::GpuSpace,
            ffi::SampleType::GpuTime => api::SampleType::GpuTime,
            ffi::SampleType::HeapLiveSamples => api::SampleType::HeapLiveSamples,
            ffi::SampleType::HeapLiveSize => api::SampleType::HeapLiveSize,
            ffi::SampleType::HeapSpace => api::SampleType::HeapSpace,
            ffi::SampleType::InuseObjects => api::SampleType::InuseObjects,
            ffi::SampleType::InuseSpace => api::SampleType::InuseSpace,
            ffi::SampleType::LockAcquire => api::SampleType::LockAcquire,
            ffi::SampleType::LockAcquireWait => api::SampleType::LockAcquireWait,
            ffi::SampleType::LockCount => api::SampleType::LockCount,
            ffi::SampleType::LockRelease => api::SampleType::LockRelease,
            ffi::SampleType::LockReleaseHold => api::SampleType::LockReleaseHold,
            ffi::SampleType::LockTime => api::SampleType::LockTime,
            ffi::SampleType::ObjectsLegacy => api::SampleType::ObjectsLegacy,
            ffi::SampleType::RequestTime => api::SampleType::RequestTime,
            ffi::SampleType::Sample => api::SampleType::Sample,
            ffi::SampleType::Tracepoint => api::SampleType::Tracepoint,
            ffi::SampleType::SocketReadSize => api::SampleType::SocketReadSize,
            ffi::SampleType::SocketReadSizeSamples => api::SampleType::SocketReadSizeSamples,
            ffi::SampleType::SocketReadTime => api::SampleType::SocketReadTime,
            ffi::SampleType::SocketReadTimeSamples => api::SampleType::SocketReadTimeSamples,
            ffi::SampleType::SocketWriteSize => api::SampleType::SocketWriteSize,
            ffi::SampleType::SocketWriteSizeSamples => api::SampleType::SocketWriteSizeSamples,
            ffi::SampleType::SocketWriteTime => api::SampleType::SocketWriteTime,
            ffi::SampleType::SocketWriteTimeSamples => api::SampleType::SocketWriteTimeSamples,
            ffi::SampleType::SpaceLegacy => api::SampleType::SpaceLegacy,
            ffi::SampleType::Timeline => api::SampleType::Timeline,
            ffi::SampleType::WallSamples => api::SampleType::WallSamples,
            ffi::SampleType::WallTime => api::SampleType::WallTime,
            ffi::SampleType::WallLegacy => api::SampleType::WallLegacy,
            ffi::SampleType::Custom1 => api::SampleType::Custom1,
            ffi::SampleType::Custom2 => api::SampleType::Custom2,
            ffi::SampleType::Custom3 => api::SampleType::Custom3,
            ffi::SampleType::Custom4 => api::SampleType::Custom4,
            ffi::SampleType::Custom5 => api::SampleType::Custom5,
            _ => anyhow::bail!("invalid SampleType discriminant from C++"),
        })
    }
}

impl TryFrom<&ffi::Period> for api::Period {
    type Error = anyhow::Error;

    fn try_from(period: &ffi::Period) -> Result<Self, Self::Error> {
        Ok(api::Period {
            sample_type: period.value_type.try_into()?,
            value: period.value,
        })
    }
}

impl<'a> From<&ffi::Mapping<'a>> for api::Mapping<'a> {
    fn from(mapping: &ffi::Mapping<'a>) -> Self {
        api::Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: mapping.filename,
            build_id: mapping.build_id,
        }
    }
}

impl<'a> From<&ffi::Function<'a>> for api::Function<'a> {
    fn from(func: &ffi::Function<'a>) -> Self {
        api::Function {
            name: func.name,
            system_name: func.system_name,
            filename: func.filename,
        }
    }
}

impl<'a> From<&ffi::Location<'a>> for api::Location<'a> {
    fn from(loc: &ffi::Location<'a>) -> Self {
        api::Location {
            mapping: (&loc.mapping).into(),
            function: (&loc.function).into(),
            address: loc.address,
            line: loc.line,
        }
    }
}

impl<'a> From<&ffi::Label<'a>> for api::Label<'a> {
    fn from(label: &ffi::Label<'a>) -> Self {
        api::Label {
            key: label.key,
            str: label.str,
            num: label.num,
            num_unit: label.num_unit,
        }
    }
}

// AttachmentFile → exporter::File conversion is handled inline in
// prepare_export_args (profile_exporter.rs) to keep lossy-converted
// filenames alive for the borrow.
