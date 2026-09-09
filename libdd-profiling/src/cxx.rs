// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CXX bindings for profiling module - provides a safe and idiomatic C++ API

#![allow(clippy::needless_lifetimes)]

use anyhow::Context;

use crate::api;
use crate::api2;
use crate::exporter;
use crate::internal;
use crate::profiles;

pub struct StringId2Opaque {
    _private: [u8; 0],
}

pub struct FunctionId2Opaque {
    _private: [u8; 0],
}

pub struct MappingId2Opaque {
    _private: [u8; 0],
}

// ============================================================================
// CXX Bridge - C++ Bindings
// ============================================================================

/// cbindgen:ignore
#[cxx::bridge(namespace = "datadog::profiling")]
pub mod ffi {
    // Shared types - CXX-friendly types
    //
    // This enum covers the ValueType entries supported by Datadog profilers:
    // - dd-trace-rb (stack_recorder.c `all_value_types`)
    // - dd-trace-py (dd_wrapper/include/types.hpp + dd_wrapper/src/profile.cpp value strings)
    // - dd-trace-php (profiling/src/profiling/samples.rs I/O profiling sample types)
    // - dd-trace-dotnet (sample type definitions for allocations, locks, CPU, walltime, exceptions,
    //   live objects, HTTP requests)
    // - pprof-nodejs (profile-serializer.ts value type functions)
    //
    // To use a type not yet in this enum, use Custom1 through Custom5 and configure
    // the slot on the Profile before serialization.
    //
    // LEGACY VARIANTS (prefer alternatives for consistency):
    // - CpuLegacy (use CpuTime)
    // - CpuSampleLegacy (use CpuSamples)
    // - ExceptionLegacy (use ExceptionSamples)
    // - ObjectsLegacy (use InuseObjects)
    // - SpaceLegacy (use specific variants: InuseSpace, HeapSpace, AllocSpace)
    // - WallLegacy (use WallTime)
    enum SampleType {
        AllocSamples,
        AllocSamplesUnscaled,
        AllocSize,
        AllocSpace,
        CpuTime,
        CpuSamples,
        CpuLegacy,       // LEGACY: Use CpuTime instead
        CpuSampleLegacy, // LEGACY: Use CpuSamples instead
        ExceptionSamples,
        ExceptionLegacy, // LEGACY: Use ExceptionSamples instead
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
        ObjectsLegacy, // LEGACY: Use InuseObjects instead
        RequestTime,
        Sample,
        Tracepoint,
        SocketReadSize,
        SocketReadSizeSamples,
        SocketReadTime,
        SocketReadTimeSamples,
        SocketWriteSize,
        SocketWriteSizeSamples,
        SocketWriteTime,
        SocketWriteTimeSamples,
        SpaceLegacy, // LEGACY: Use specific variants (InuseSpace, HeapSpace, AllocSpace) instead
        Timeline,
        WallSamples,
        WallTime,
        WallLegacy, // LEGACY: Use WallTime instead
        Custom1,
        Custom2,
        Custom3,
        Custom4,
        Custom5,
    }

    struct Period {
        value_type: SampleType,
        value: i64,
    }

    struct Mapping<'a> {
        memory_start: u64,
        memory_limit: u64,
        file_offset: u64,
        filename: &'a str,
        build_id: &'a str,
    }

    struct Function<'a> {
        name: &'a str,
        system_name: &'a str,
        filename: &'a str,
    }

    struct Location<'a> {
        mapping: Mapping<'a>,
        function: Function<'a>,
        address: u64,
        line: i64,
    }

    struct Label<'a> {
        key: &'a str,
        str: &'a str,
        num: i64,
        num_unit: &'a str,
    }

    struct Sample<'a> {
        locations: Vec<Location<'a>>,
        values: Vec<i64>,
        labels: Vec<Label<'a>>,
    }

    /// Opaque pointer-shaped dictionary string id.
    ///
    /// C++ callers must treat handle as opaque. Do not dereference it, invent
    /// non-null values, persist non-null values beyond the ProfilesDictionary
    /// lifetime, or compare values across dictionaries. A null/default handle
    /// represents the empty string. A non-null handle is only valid with the
    /// ProfilesDictionary that produced it, or Profiles created from that
    /// dictionary.
    struct StringId2 {
        handle: *mut StringId2Opaque,
    }

    /// Opaque pointer-shaped dictionary function id.
    ///
    /// C++ callers must treat handle as opaque. Do not dereference it, invent
    /// non-null values, persist non-null values beyond the ProfilesDictionary
    /// lifetime, or compare values across dictionaries. A null/default handle
    /// represents the default/unknown function. A non-null handle is only valid
    /// with the ProfilesDictionary that produced it, or Profiles created from
    /// that dictionary.
    struct FunctionId2 {
        handle: *mut FunctionId2Opaque,
    }

    /// Opaque pointer-shaped dictionary mapping id.
    ///
    /// C++ callers must treat handle as opaque. Do not dereference it, invent
    /// non-null values, persist non-null values beyond the ProfilesDictionary
    /// lifetime, or compare values across dictionaries. A null/default handle
    /// represents an unknown/no mapping. A non-null handle is only valid with
    /// the ProfilesDictionary that produced it, or Profiles created from that
    /// dictionary.
    struct MappingId2 {
        handle: *mut MappingId2Opaque,
    }

    /// Function data for insertion into a ProfilesDictionary.
    ///
    /// String ids may be null/default to represent empty strings. Non-null ids
    /// must come from the same ProfilesDictionary receiving the insertion.
    struct Function2 {
        name: StringId2,
        system_name: StringId2,
        // NOTE: api2's Rust/C FFI datatype uses file_name, while pprof and
        // the string-based CXX Function use filename. Keep file_name for now
        // to match api2; revisit the CXX-facing name during PR review.
        file_name: StringId2,
    }

    /// Mapping data for insertion into a ProfilesDictionary.
    ///
    /// String ids may be null/default to represent empty strings. Non-null ids
    /// must come from the same ProfilesDictionary receiving the insertion.
    struct Mapping2 {
        memory_start: u64,
        memory_limit: u64,
        file_offset: u64,
        filename: StringId2,
        build_id: StringId2,
    }

    /// Dictionary-backed location.
    ///
    /// mapping and function may be null/default to represent unknown values.
    /// Non-null ids must come from the same ProfilesDictionary used to create
    /// the Profile receiving this location.
    struct Location2 {
        mapping: MappingId2,
        function: FunctionId2,
        address: u64,
        line: i64,
    }

    /// Dictionary-backed label.
    ///
    /// key may be null/default to represent an empty key, though callers should
    /// normally use a meaningful dictionary string. Non-null key ids must come
    /// from the same ProfilesDictionary used to create the Profile receiving
    /// this label. str and num_unit are borrowed only for the duration of the
    /// add_sample2 call.
    struct Label2<'a> {
        key: StringId2,
        str: &'a str,
        num: i64,
        num_unit: &'a str,
    }

    /// Dictionary-backed sample.
    ///
    /// locations, values, and labels are borrowed only for the duration of the
    /// add_sample2 call. Null/default ids represent empty or unknown values.
    /// All non-null ids in locations and labels must come from the
    /// ProfilesDictionary used to create the Profile receiving this sample.
    struct Sample2<'a> {
        locations: &'a [Location2],
        values: &'a [i64],
        labels: &'a [Label2<'a>],
    }

    struct Tag<'a> {
        key: &'a str,
        value: &'a str,
    }

    struct AttachmentFile<'a> {
        name: &'a str,
        data: &'a [u8],
    }

    // Opaque Rust types
    extern "Rust" {
        type Profile;
        type ProfileExporter;
        type EncodedProfile;
        type ExporterManager;
        type ProfilesDictionary;
        type StringId2Opaque;
        type FunctionId2Opaque;
        type MappingId2Opaque;
        type CancellationToken;

        // CancellationToken factory and methods
        fn new_cancellation_token() -> Box<CancellationToken>;
        fn clone_token(self: &CancellationToken) -> Box<CancellationToken>;
        fn cancel(self: &CancellationToken);
        fn is_cancelled(self: &CancellationToken) -> bool;

        // Static factory methods for Profile
        #[Self = "Profile"]
        fn create(sample_types: Vec<SampleType>, period: &Period) -> Result<Box<Profile>>;

        /// Create a profile without a sampling period.
        #[Self = "Profile"]
        fn create_no_period(sample_types: Vec<SampleType>) -> Result<Box<Profile>>;

        // Static factory methods for ProfilesDictionary
        #[Self = "ProfilesDictionary"]
        fn create() -> Result<Box<ProfilesDictionary>>;

        /// Inserts value into this dictionary and returns an opaque id.
        ///
        /// The returned id must only be used with this dictionary or Profiles
        /// created from it.
        fn insert_string(self: &ProfilesDictionary, value: &str) -> Result<StringId2>;

        /// Inserts function into this dictionary and returns an opaque id.
        ///
        /// Null/default ids in function represent empty strings. Non-null ids
        /// in function must have been produced by this dictionary. The returned
        /// id must only be used with this dictionary or Profiles created from
        /// it.
        fn insert_function(self: &ProfilesDictionary, function: &Function2) -> Result<FunctionId2>;

        /// Inserts mapping into this dictionary and returns an opaque id.
        ///
        /// Null/default ids in mapping represent empty strings. Non-null ids in
        /// mapping must have been produced by this dictionary. The returned id
        /// must only be used with this dictionary or Profiles created from it.
        fn insert_mapping(self: &ProfilesDictionary, mapping: &Mapping2) -> Result<MappingId2>;

        /// Creates a Profile backed by dictionary.
        ///
        /// The Profile keeps dictionary storage alive internally. Future
        /// add_sample2 calls on the Profile may use null/default ids for empty
        /// or unknown values, but all non-null ids must be produced by this
        /// same dictionary.
        #[Self = "Profile"]
        fn create_with_dictionary(
            sample_types: Vec<SampleType>,
            period: &Period,
            dictionary: &ProfilesDictionary,
        ) -> Result<Box<Profile>>;

        // Profile methods
        fn add_sample(self: &mut Profile, sample: &Sample) -> Result<()>;
        fn add_sample_with_timestamp(
            self: &mut Profile,
            sample: &Sample,
            endtime_ns: i64,
        ) -> Result<()>;

        /// Adds a dictionary-backed sample.
        ///
        /// Null/default ids in sample represent empty or unknown values. All
        /// non-null ids in sample must have been produced by the
        /// ProfilesDictionary used to create this Profile. The sample slices are
        /// borrowed only for the duration of this call. endtime_ns is an
        /// optional end timestamp in nanoseconds; pass 0 to record the sample
        /// without a timestamp.
        fn add_sample2(self: &mut Profile, sample: &Sample2, endtime_ns: i64) -> Result<()>;
        fn set_custom_sample_type(
            self: &mut Profile,
            slot: SampleType,
            type_: &str,
            unit: &str,
        ) -> Result<()>;
        fn add_endpoint(self: &mut Profile, local_root_span_id: u64, endpoint: &str) -> Result<()>;
        fn add_endpoint_count(self: &mut Profile, endpoint: &str, value: i64) -> Result<()>;

        // Upscaling rule methods (one for each variant)
        fn add_upscaling_rule_poisson(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            sum_value_offset: usize,
            count_value_offset: usize,
            sampling_distance: u64,
        ) -> Result<()>;

        fn add_upscaling_rule_poisson_non_sample_type_count(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            sum_value_offset: usize,
            count_value: u64,
            sampling_distance: u64,
        ) -> Result<()>;

        fn add_upscaling_rule_proportional(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            scale: f64,
        ) -> Result<()>;

        fn reset(self: &mut Profile) -> Result<()>;

        /// Serialize and reset the profile, returning the encoded previous
        /// profile data. The returned EncodedProfile includes the compressed
        /// pprof bytes plus metadata needed by exporters, such as endpoint
        /// counts.
        fn serialize(self: &mut Profile) -> Result<Box<EncodedProfile>>;

        /// Serialize and reset the profile, returning only compressed pprof
        /// bytes. This is a convenience/debug API; callers that intend to send
        /// the profile should prefer serialize() plus send_encoded_profile().
        fn serialize_to_vec(self: &mut Profile) -> Result<Vec<u8>>;

        /// Return a copy of the compressed pprof bytes.
        fn bytes(self: &EncodedProfile) -> Vec<u8>;

        // Static factory methods for ProfileExporter
        #[Self = "ProfileExporter"]
        fn create_agent_exporter(
            profiling_library_name: &str,
            profiling_library_version: &str,
            family: &str,
            tags: Vec<Tag>,
            agent_url: &str,
            timeout_ms: u64,
            use_system_resolver: bool,
        ) -> Result<Box<ProfileExporter>>;

        #[Self = "ProfileExporter"]
        #[allow(clippy::too_many_arguments)]
        fn create_agentless_exporter(
            profiling_library_name: &str,
            profiling_library_version: &str,
            family: &str,
            tags: Vec<Tag>,
            site: &str,
            api_key: &str,
            timeout_ms: u64,
            use_system_resolver: bool,
        ) -> Result<Box<ProfileExporter>>;

        #[Self = "ProfileExporter"]
        fn create_file_exporter(
            profiling_library_name: &str,
            profiling_library_version: &str,
            family: &str,
            tags: Vec<Tag>,
            output_path: &str,
        ) -> Result<Box<ProfileExporter>>;

        // ProfileExporter methods
        /// Sends a profile to Datadog.
        ///
        /// **Important**: This method resets the profile and sends the *previous* profile data.
        /// After calling this, the profile will be empty and ready for new samples.
        ///
        /// # Arguments
        /// * `profile` - Profile to send (will be consumed/reset, previous data is sent)
        /// * `files_to_compress` - Additional files to compress and attach (e.g., heap dumps)
        /// * `additional_tags` - Per-profile tags (in addition to exporter-level tags)
        /// * `internal_metadata` - Internal metadata as JSON string (e.g., `{"key": "value"}`) See
        ///   Datadog-internal "RFC: Attaching internal metadata to pprof profiles" Pass empty
        ///   string "" if not needed
        /// * `process_tags` - Process-level tags as comma-separated string (e.g.,
        ///   "runtime:native,profiler_version:1.0") Pass empty string "" if not needed
        /// * `info` - System/environment info as JSON string (e.g., `{"os": "linux", "arch":
        ///   "x86_64"}`) See Datadog-internal "RFC: Pprof System Info Support" Pass empty string ""
        ///   if not needed
        fn send_profile(
            self: &mut ProfileExporter,
            profile: &mut Profile,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
        ) -> Result<()>;

        /// Sends a profile to Datadog with cancellation support.
        ///
        /// This is the same as `send_profile`, but allows cancelling the operation from another
        /// thread using a cancellation token.
        ///
        /// **Important**: This method resets the profile and sends the *previous* profile data.
        /// After calling this, the profile will be empty and ready for new samples.
        ///
        /// # Arguments
        /// * `profile` - Profile to send (will be consumed/reset, previous data is sent)
        /// * `files_to_compress` - Additional files to compress and attach (e.g., heap dumps)
        /// * `additional_tags` - Per-profile tags (in addition to exporter-level tags)
        /// * `process_tags` - Process-level tags as comma-separated string (e.g.,
        ///   "runtime:native,profiler_version:1.0") Pass empty string "" if not needed
        /// * `internal_metadata` - Internal metadata as JSON string (e.g., `{"key": "value"}`) See
        ///   Datadog-internal "RFC: Attaching internal metadata to pprof profiles" Pass empty
        ///   string "" if not needed
        /// * `info` - System/environment info as JSON string (e.g., `{"os": "linux", "arch":
        ///   "x86_64"}`) See Datadog-internal "RFC: Pprof System Info Support" Pass empty string ""
        ///   if not needed
        /// * `cancel` - Cancellation token to cancel the send operation
        #[allow(clippy::too_many_arguments)]
        fn send_profile_with_cancellation(
            self: &mut ProfileExporter,
            profile: &mut Profile,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
            cancel: &CancellationToken,
        ) -> Result<()>;

        /// Sends a previously serialized profile to Datadog.
        ///
        /// This is the split form of send_profile(). It allows callers to
        /// serialize/reset a Profile under their own lock, then release that
        /// lock before performing blocking I/O.
        ///
        /// # Arguments
        /// * `encoded` - EncodedProfile previously returned by Profile::serialize().
        /// * `files_to_compress` - Additional files to compress and attach.
        /// * `additional_tags` - Per-profile tags in addition to exporter-level tags.
        /// * `process_tags` - Process-level tags as comma-separated string; empty if not needed.
        /// * `internal_metadata` - Internal metadata JSON object; empty if not needed.
        /// * `info` - System/environment info JSON object; empty if not needed.
        fn send_encoded_profile(
            self: &mut ProfileExporter,
            encoded: Box<EncodedProfile>,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
        ) -> Result<()>;

        /// Sends a previously serialized profile to Datadog with cancellation
        /// support.
        ///
        /// This is the split form of send_profile_with_cancellation().
        #[allow(clippy::too_many_arguments)]
        fn send_encoded_profile_with_cancellation(
            self: &mut ProfileExporter,
            encoded: Box<EncodedProfile>,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
            cancel: &CancellationToken,
        ) -> Result<()>;

        // ExporterManager methods
        /// Creates a new ExporterManager with a background worker thread
        #[Self = "ExporterManager"]
        fn new_manager(exporter: Box<ProfileExporter>) -> Result<Box<ExporterManager>>;

        /// Queue a profile to be sent asynchronously by the worker thread
        ///
        /// **Important**: This method resets the profile and queues the *previous* profile data.
        /// After calling this, the profile will be empty and ready for new samples.
        #[allow(clippy::too_many_arguments)]
        fn queue_profile(
            self: &ExporterManager,
            profile: &mut Profile,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
        ) -> Result<()>;

        /// Queue a previously serialized profile to be sent asynchronously by
        /// the background worker thread.
        ///
        /// This is the split form of queue_profile().
        #[allow(clippy::too_many_arguments)]
        fn queue_encoded_profile(
            self: &ExporterManager,
            encoded: Box<EncodedProfile>,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
        ) -> Result<()>;

        /// Abort the manager, stopping the worker thread
        /// Transitions the manager from Active to Suspended state
        fn abort(self: &mut ExporterManager) -> Result<()>;

        /// Prefork: suspend the manager before forking
        /// Transitions the manager from Active to Suspended state
        fn prefork(self: &mut ExporterManager) -> Result<()>;

        /// Postfork child: reinitialize manager in child process, discarding inflight requests
        /// Transitions the manager from Suspended to Active state
        fn postfork_child(self: &mut ExporterManager) -> Result<()>;

        /// Postfork parent: reinitialize manager in parent process and re-queue inflight requests
        /// Transitions the manager from Suspended to Active state
        fn postfork_parent(self: &mut ExporterManager) -> Result<()>;
    }
}

// ============================================================================
// From Implementations - Convert CXX types to API types
// ============================================================================

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

impl From<profiles::datatypes::StringId2> for ffi::StringId2 {
    fn from(id: profiles::datatypes::StringId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid StringId2 handle produced by
/// libdatadog. Null/default handles represent the empty string. For non-null
/// handles, the producing ProfilesDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
unsafe fn string_id2_from_cxx(id: &ffi::StringId2) -> profiles::datatypes::StringId2 {
    unsafe { profiles::datatypes::StringId2::from_raw_ptr(id.handle.cast()) }
}

impl From<profiles::datatypes::FunctionId2> for ffi::FunctionId2 {
    fn from(id: profiles::datatypes::FunctionId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid FunctionId2 handle produced by
/// libdatadog. Null/default handles represent the default/unknown function. For
/// non-null handles, the producing ProfilesDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
unsafe fn function_id2_from_cxx(id: &ffi::FunctionId2) -> profiles::datatypes::FunctionId2 {
    unsafe { profiles::datatypes::FunctionId2::from_raw_ptr(id.handle.cast()) }
}

impl From<profiles::datatypes::MappingId2> for ffi::MappingId2 {
    fn from(id: profiles::datatypes::MappingId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid MappingId2 handle produced by
/// libdatadog. Null/default handles represent an unknown/no mapping. For
/// non-null handles, the producing ProfilesDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
unsafe fn mapping_id2_from_cxx(id: &ffi::MappingId2) -> profiles::datatypes::MappingId2 {
    unsafe { profiles::datatypes::MappingId2::from_raw_ptr(id.handle.cast()) }
}

/// # Safety
///
/// All ids in function must be null/default or valid handles produced by the
/// same ProfilesDictionary receiving the function insertion. Null/default ids
/// represent empty strings.
unsafe fn function2_from_cxx(function: &ffi::Function2) -> profiles::datatypes::Function2 {
    profiles::datatypes::Function2 {
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // same ProfilesDictionary receiving the insertion. Null/default ids
        // represent empty strings.
        name: unsafe { string_id2_from_cxx(&function.name) },
        system_name: unsafe { string_id2_from_cxx(&function.system_name) },
        file_name: unsafe { string_id2_from_cxx(&function.file_name) },
    }
}

/// # Safety
///
/// All ids in mapping must be null/default or valid handles produced by the
/// same ProfilesDictionary receiving the mapping insertion. Null/default ids
/// represent empty strings.
unsafe fn mapping2_from_cxx(mapping: &ffi::Mapping2) -> profiles::datatypes::Mapping2 {
    profiles::datatypes::Mapping2 {
        memory_start: mapping.memory_start,
        memory_limit: mapping.memory_limit,
        file_offset: mapping.file_offset,
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // same ProfilesDictionary receiving the insertion. Null/default ids
        // represent empty strings.
        filename: unsafe { string_id2_from_cxx(&mapping.filename) },
        build_id: unsafe { string_id2_from_cxx(&mapping.build_id) },
    }
}

/// # Safety
///
/// All ids in location must be null/default or valid handles produced by the
/// ProfilesDictionary used to create the receiving Profile. Null/default ids
/// represent unknown values.
unsafe fn location2_from_cxx(location: &ffi::Location2) -> api2::Location2 {
    api2::Location2 {
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // ProfilesDictionary used to create the receiving Profile. Null/default
        // ids represent unknown values.
        mapping: unsafe { mapping_id2_from_cxx(&location.mapping) },
        function: unsafe { function_id2_from_cxx(&location.function) },
        address: location.address,
        line: location.line,
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

impl<'a> From<&ffi::AttachmentFile<'a>> for exporter::File<'a> {
    fn from(file: &ffi::AttachmentFile<'a>) -> Self {
        exporter::File {
            name: file.name,
            bytes: file.data,
        }
    }
}

impl<'a> TryFrom<&ffi::Tag<'a>> for libdd_common::tag::Tag {
    type Error = anyhow::Error;

    fn try_from(tag: &ffi::Tag<'a>) -> Result<Self, Self::Error> {
        libdd_common::tag::Tag::new(tag.key, tag.value)
    }
}

// ============================================================================
// CancellationToken - Wrapper around tokio_util::sync::CancellationToken
// ============================================================================

pub struct CancellationToken {
    inner: tokio_util::sync::CancellationToken,
}

/// Creates a new cancellation token.
pub fn new_cancellation_token() -> Box<CancellationToken> {
    Box::new(CancellationToken {
        inner: tokio_util::sync::CancellationToken::new(),
    })
}

impl CancellationToken {
    /// Clones the cancellation token.
    ///
    /// A cloned token is connected to the original token - either can be used
    /// to cancel or check cancellation status. The useful part is that they have
    /// independent lifetimes and can be dropped separately.
    ///
    /// This is useful for multi-threaded scenarios where one thread performs the
    /// send operation while another thread can cancel it.
    pub fn clone_token(&self) -> Box<CancellationToken> {
        Box::new(CancellationToken {
            inner: self.inner.clone(),
        })
    }

    /// Cancels the token.
    ///
    /// Note that cancellation is a terminal state; calling cancel multiple times
    /// has no additional effect.
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Returns true if the token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

// ============================================================================
// Profile - Wrapper around internal::Profile
// ============================================================================

pub struct ProfilesDictionary {
    inner: profiles::collections::Arc<profiles::datatypes::ProfilesDictionary>,
}

impl ProfilesDictionary {
    pub fn create() -> anyhow::Result<Box<ProfilesDictionary>> {
        let dictionary = profiles::datatypes::ProfilesDictionary::try_new()
            .context("ProfilesDictionary::create failed")?;
        let inner = profiles::collections::Arc::try_new(dictionary)
            .map_err(|_| anyhow::anyhow!("failed to allocate ProfilesDictionary"))?;

        Ok(Box::new(ProfilesDictionary { inner }))
    }

    pub fn insert_string(&self, value: &str) -> anyhow::Result<ffi::StringId2> {
        self.inner
            .try_insert_str2(value)
            .map(Into::into)
            .context("ProfilesDictionary::insert_string failed")
    }

    pub fn insert_function(&self, function: &ffi::Function2) -> anyhow::Result<ffi::FunctionId2> {
        // SAFETY: The CXX API contract requires all ids in function to come
        // from this ProfilesDictionary.
        let function = unsafe { function2_from_cxx(function) };
        self.inner
            .try_insert_function2(function)
            .map(Into::into)
            .context("ProfilesDictionary::insert_function failed")
    }

    pub fn insert_mapping(&self, mapping: &ffi::Mapping2) -> anyhow::Result<ffi::MappingId2> {
        // SAFETY: The CXX API contract requires all ids in mapping to come
        // from this ProfilesDictionary.
        let mapping = unsafe { mapping2_from_cxx(mapping) };
        self.inner
            .try_insert_mapping2(mapping)
            .map(Into::into)
            .context("ProfilesDictionary::insert_mapping failed")
    }
}

pub struct Profile {
    inner: internal::Profile,
}

impl Profile {
    pub fn create(
        sample_types: Vec<ffi::SampleType>,
        period: &ffi::Period,
    ) -> anyhow::Result<Box<Profile>> {
        // Convert (fallibly) from CXX types to API types
        let types: Vec<api::SampleType> = sample_types
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;
        let period_value: api::Period = period.try_into()?;

        // Profile::try_new interns the strings
        let inner = internal::Profile::try_new(&types, Some(period_value))?;

        Ok(Box::new(Profile { inner }))
    }

    pub fn create_no_period(sample_types: Vec<ffi::SampleType>) -> anyhow::Result<Box<Profile>> {
        let types: Vec<api::SampleType> = sample_types
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;
        let inner = internal::Profile::try_new(&types, None)?;
        Ok(Box::new(Profile { inner }))
    }

    pub fn create_with_dictionary(
        sample_types: Vec<ffi::SampleType>,
        period: &ffi::Period,
        dictionary: &ProfilesDictionary,
    ) -> anyhow::Result<Box<Profile>> {
        let types: Vec<api::SampleType> = sample_types
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;
        let period_value: api::Period = period.try_into()?;
        let dictionary = dictionary
            .inner
            .try_clone()
            .context("failed to clone ProfilesDictionary for Profile")?;
        let inner =
            internal::Profile::try_new_with_dictionary(&types, Some(period_value), dictionary)
                .context("Profile::create_with_dictionary failed")?;
        Ok(Box::new(Profile { inner }))
    }

    pub fn add_sample(&mut self, sample: &ffi::Sample) -> anyhow::Result<()> {
        let api_sample = api::Sample {
            locations: sample.locations.iter().map(Into::into).collect(),
            values: &sample.values,
            labels: sample.labels.iter().map(Into::into).collect(),
        };

        // Profile interns the strings
        self.inner.try_add_sample(api_sample, None)?;
        Ok(())
    }

    pub fn add_sample_with_timestamp(
        &mut self,
        sample: &ffi::Sample,
        endtime_ns: i64,
    ) -> anyhow::Result<()> {
        let timestamp =
            internal::Timestamp::new(endtime_ns).context("endtime_ns must be non-zero")?;

        let api_sample = api::Sample {
            locations: sample.locations.iter().map(Into::into).collect(),
            values: &sample.values,
            labels: sample.labels.iter().map(Into::into).collect(),
        };

        self.inner
            .try_add_sample(api_sample, Some(timestamp))
            .context("Profile::add_sample_with_timestamp failed")
    }

    /// Adds an api2/dictionary-backed sample.
    ///
    /// Null/default ids in sample represent empty or unknown values. All
    /// non-null ids in sample must have been produced by the same
    /// ProfilesDictionary used to create this Profile. The caller must keep the
    /// provided slices valid for the duration of this call. An endtime_ns value
    /// of 0 records the sample without a timestamp.
    pub fn add_sample2(&mut self, sample: &ffi::Sample2, endtime_ns: i64) -> anyhow::Result<()> {
        let timestamp = internal::Timestamp::new(endtime_ns);
        let locations_iter = sample.locations.iter().map(|location| {
            // SAFETY: The CXX API contract requires all non-null api2 ids in
            // sample to come from the same ProfilesDictionary used to create
            // this Profile. Null/default ids represent unknown values.
            unsafe { location2_from_cxx(location) }
        });
        let labels_iter = sample
            .labels
            .iter()
            .map(|label| -> anyhow::Result<api2::Label<'_>> {
                Ok(api2::Label {
                    // SAFETY: The CXX API contract requires all non-null api2
                    // ids in sample to come from the same ProfilesDictionary
                    // used to create this Profile. Null/default keys represent
                    // the empty string.
                    key: unsafe { string_id2_from_cxx(&label.key) },
                    str: label.str,
                    num: label.num,
                    num_unit: label.num_unit,
                })
            });

        // SAFETY: The CXX API contract requires all non-null api2 ids in sample
        // to come from the same ProfilesDictionary used to create this Profile.
        // Null/default ids represent empty or unknown values.
        unsafe {
            self.inner
                .try_add_sample2(locations_iter, sample.values, labels_iter, timestamp)
                .context("Profile::add_sample2 failed")
        }
    }

    pub fn set_custom_sample_type(
        &mut self,
        slot: ffi::SampleType,
        type_: &str,
        unit: &str,
    ) -> anyhow::Result<()> {
        let slot: api::SampleType = slot.try_into()?;
        self.inner
            .set_custom_sample_type(slot, api::ValueType::new(type_, unit))
    }

    pub fn add_endpoint(&mut self, local_root_span_id: u64, endpoint: &str) -> anyhow::Result<()> {
        self.inner
            .add_endpoint(local_root_span_id, std::borrow::Cow::Borrowed(endpoint))
    }

    pub fn add_endpoint_count(&mut self, endpoint: &str, value: i64) -> anyhow::Result<()> {
        self.inner
            .add_endpoint_count(std::borrow::Cow::Borrowed(endpoint), value)
    }

    pub fn add_upscaling_rule_poisson(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value_offset: usize,
        sampling_distance: u64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::Poisson {
            sum_value_offset,
            count_value_offset,
            sampling_distance,
        };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn add_upscaling_rule_poisson_non_sample_type_count(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value: u64,
        sampling_distance: u64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::PoissonNonSampleTypeCount {
            sum_value_offset,
            count_value,
            sampling_distance,
        };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn add_upscaling_rule_proportional(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        scale: f64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::Proportional { scale };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn reset(&mut self) -> anyhow::Result<()> {
        // Reset and discard the old profile
        self.inner.reset_and_return_previous()?;
        Ok(())
    }

    pub fn serialize(&mut self) -> anyhow::Result<Box<EncodedProfile>> {
        // Reset the profile and get the old one to serialize.
        let old_profile = self.inner.reset_and_return_previous()?;
        let end_time = Some(std::time::SystemTime::now());
        let encoded = old_profile.serialize_into_compressed_pprof(end_time, None)?;
        Ok(Box::new(EncodedProfile { inner: encoded }))
    }

    pub fn serialize_to_vec(&mut self) -> anyhow::Result<Vec<u8>> {
        let EncodedProfile { inner } = *self.serialize()?;
        Ok(inner.buffer)
    }
}

// ============================================================================
// EncodedProfile - Wrapper around internal::EncodedProfile
// ============================================================================

pub struct EncodedProfile {
    inner: internal::EncodedProfile,
}

impl EncodedProfile {
    pub fn bytes(&self) -> Vec<u8> {
        self.inner.buffer.clone()
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

type PreparedExportArgs<'a> = (
    Vec<exporter::File<'a>>,
    Vec<libdd_common::tag::Tag>,
    Option<&'a str>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
);

fn prepare_export_args<'a>(
    files_to_compress: Vec<ffi::AttachmentFile<'a>>,
    additional_tags: Vec<ffi::Tag>,
    process_tags: &'a str,
    internal_metadata: &str,
    info: &str,
) -> anyhow::Result<PreparedExportArgs<'a>> {
    let files_to_compress_vec: Vec<exporter::File> =
        files_to_compress.iter().map(Into::into).collect();

    let additional_tags_vec: Vec<libdd_common::tag::Tag> = additional_tags
        .iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;

    let internal_metadata_json = if internal_metadata.is_empty() {
        None
    } else {
        Some(serde_json::from_str(internal_metadata)?)
    };

    let info_json = if info.is_empty() {
        None
    } else {
        Some(serde_json::from_str(info)?)
    };

    let process_tags_opt = if process_tags.is_empty() {
        None
    } else {
        Some(process_tags)
    };

    Ok((
        files_to_compress_vec,
        additional_tags_vec,
        process_tags_opt,
        internal_metadata_json,
        info_json,
    ))
}

/// Helper to encode a profile and prepare arguments for sending/queuing.
///
/// Resets the profile and returns the encoded previous profile data along with
/// converted arguments ready for the exporter APIs.
#[allow(clippy::type_complexity)]
fn prepare_profile_for_export<'a>(
    profile: &mut Profile,
    files_to_compress: Vec<ffi::AttachmentFile<'a>>,
    additional_tags: Vec<ffi::Tag>,
    process_tags: &'a str,
    internal_metadata: &str,
    info: &str,
) -> anyhow::Result<(
    Box<EncodedProfile>,
    Vec<exporter::File<'a>>,
    Vec<libdd_common::tag::Tag>,
    Option<&'a str>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
)> {
    let encoded = profile.serialize()?;
    let (
        files_to_compress_vec,
        additional_tags_vec,
        process_tags_opt,
        internal_metadata_json,
        info_json,
    ) = prepare_export_args(
        files_to_compress,
        additional_tags,
        process_tags,
        internal_metadata,
        info,
    )?;

    Ok((
        encoded,
        files_to_compress_vec,
        additional_tags_vec,
        process_tags_opt,
        internal_metadata_json,
        info_json,
    ))
}

// ============================================================================
// ProfileExporter - Wrapper around exporter::ProfileExporter
// ============================================================================

pub struct ProfileExporter {
    inner: exporter::ProfileExporter,
}

impl ProfileExporter {
    pub fn create_agent_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        agent_url: &str,
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> anyhow::Result<Box<ProfileExporter>> {
        let mut endpoint = exporter::config::agent(agent_url.parse()?)?;

        // Set timeout if non-zero (0 means use default)
        if timeout_ms > 0 {
            endpoint.timeout_ms = timeout_ms;
        }
        endpoint = endpoint.with_system_resolver(use_system_resolver);

        let tags_vec: Vec<libdd_common::tag::Tag> = tags
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        let inner = exporter::ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            family,
            tags_vec,
            endpoint,
        )?;

        Ok(Box::new(ProfileExporter { inner }))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_agentless_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        site: &str,
        api_key: &str,
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> anyhow::Result<Box<ProfileExporter>> {
        let mut endpoint = exporter::config::agentless(site, api_key.to_string())?;

        // Set timeout if non-zero (0 means use default)
        if timeout_ms > 0 {
            endpoint.timeout_ms = timeout_ms;
        }
        endpoint = endpoint.with_system_resolver(use_system_resolver);

        let tags_vec: Vec<libdd_common::tag::Tag> = tags
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        let inner = exporter::ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            family,
            tags_vec,
            endpoint,
        )?;

        Ok(Box::new(ProfileExporter { inner }))
    }

    pub fn create_file_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        output_path: &str,
    ) -> anyhow::Result<Box<ProfileExporter>> {
        let endpoint = exporter::config::file(output_path)?;

        let tags_vec: Vec<libdd_common::tag::Tag> = tags
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        let inner = exporter::ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            family,
            tags_vec,
            endpoint,
        )?;

        Ok(Box::new(ProfileExporter { inner }))
    }

    /// Sends a profile to Datadog.
    ///
    /// # Arguments
    /// * `profile` - Profile to send (will be reset after sending)
    /// * `files_to_compress` - Additional files to compress and attach
    /// * `additional_tags` - Per-profile tags (in addition to exporter-level tags)
    /// * `internal_metadata` - Internal metadata as JSON string. Empty string if not needed.
    ///   Example: `{"custom_field": "value", "version": "1.0"}`
    /// * `info` - System/environment info as JSON string. Empty string if not needed. Example:
    ///   `{"os": "linux", "arch": "x86_64", "kernel": "5.15.0"}`
    pub fn send_profile(
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> anyhow::Result<()> {
        self.send_profile_impl(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
            None,
        )
    }

    /// Sends a profile to Datadog with cancellation support.
    ///
    /// # Arguments
    /// * `profile` - Profile to send (will be reset after sending)
    /// * `files_to_compress` - Additional files to compress and attach
    /// * `additional_tags` - Per-profile tags (in addition to exporter-level tags)
    /// * `process_tags` - Process-level tags as comma-separated string. Empty string if not needed.
    /// * `internal_metadata` - Internal metadata as JSON string. Empty string if not needed.
    ///   Example: `{"custom_field": "value", "version": "1.0"}`
    /// * `info` - System/environment info as JSON string. Empty string if not needed. Example:
    ///   `{"os": "linux", "arch": "x86_64", "kernel": "5.15.0"}`
    /// * `cancel` - Cancellation token to cancel the send operation
    #[allow(clippy::too_many_arguments)]
    pub fn send_profile_with_cancellation(
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        self.send_profile_impl(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
            Some(&cancel.inner),
        )
    }

    /// Internal implementation shared by send_profile and send_profile_with_cancellation
    ///
    /// Resets the profile and sends the previous profile data. This allows continuous
    /// profiling where you keep adding samples to the current profile while the previous
    /// period's data is being sent.
    #[allow(clippy::too_many_arguments)]
    fn send_profile_impl(
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<()> {
        let (
            encoded,
            files_to_compress_vec,
            additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
        ) = prepare_profile_for_export(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )?;

        let EncodedProfile { inner: encoded } = *encoded;
        self.send_encoded_profile_prepared(
            encoded,
            &files_to_compress_vec,
            &additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
            cancel,
        )
    }

    /// Sends a previously serialized profile to Datadog.
    #[allow(clippy::boxed_local)]
    pub fn send_encoded_profile(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> anyhow::Result<()> {
        self.send_encoded_profile_impl(
            encoded,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
            None,
        )
    }

    /// Sends a previously serialized profile to Datadog with cancellation support.
    #[allow(clippy::boxed_local, clippy::too_many_arguments)]
    pub fn send_encoded_profile_with_cancellation(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        self.send_encoded_profile_impl(
            encoded,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
            Some(&cancel.inner),
        )
    }

    #[allow(clippy::boxed_local, clippy::too_many_arguments)]
    fn send_encoded_profile_impl(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<()> {
        let (
            files_to_compress_vec,
            additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
        ) = prepare_export_args(
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )?;

        let EncodedProfile { inner: encoded } = *encoded;
        self.send_encoded_profile_prepared(
            encoded,
            &files_to_compress_vec,
            &additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
            cancel,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn send_encoded_profile_prepared(
        &mut self,
        encoded: internal::EncodedProfile,
        files_to_compress: &[exporter::File<'_>],
        additional_tags: &[libdd_common::tag::Tag],
        process_tags: Option<&str>,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<()> {
        let status = self.inner.send_blocking(
            encoded,
            files_to_compress,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
            cancel,
        )?;

        anyhow::ensure!(
            status.is_success(),
            "Failed to export profile: HTTP {status}",
        );

        Ok(())
    }
}

// ============================================================================
// ExporterManager - Wrapper around exporter::ExporterManager
// ============================================================================

pub struct ExporterManager {
    inner: exporter::ExporterManager,
}

impl ExporterManager {
    pub fn new_manager(exporter: Box<ProfileExporter>) -> anyhow::Result<Box<ExporterManager>> {
        let inner = exporter::ExporterManager::new(exporter.inner)?;
        Ok(Box::new(ExporterManager { inner }))
    }

    /// Queue a profile to be sent asynchronously by the background worker thread.
    ///
    /// Resets the profile and queues the previous profile data for sending. This allows
    /// continuous profiling where you keep adding samples to the current profile while the
    /// previous period's data is being sent asynchronously.
    pub fn queue_profile(
        &self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> anyhow::Result<()> {
        let (
            encoded,
            files_to_compress_vec,
            additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
        ) = prepare_profile_for_export(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )?;

        let EncodedProfile { inner: encoded } = *encoded;
        self.queue_encoded_profile_prepared(
            encoded,
            &files_to_compress_vec,
            &additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
        )
    }

    #[allow(clippy::boxed_local)]
    pub fn queue_encoded_profile(
        &self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> anyhow::Result<()> {
        let (
            files_to_compress_vec,
            additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
        ) = prepare_export_args(
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )?;

        let EncodedProfile { inner: encoded } = *encoded;
        self.queue_encoded_profile_prepared(
            encoded,
            &files_to_compress_vec,
            &additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
        )
    }

    fn queue_encoded_profile_prepared(
        &self,
        encoded: internal::EncodedProfile,
        files_to_compress: &[exporter::File<'_>],
        additional_tags: &[libdd_common::tag::Tag],
        process_tags: Option<&str>,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        self.inner.queue(
            encoded,
            files_to_compress,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
        )?;

        Ok(())
    }

    pub fn abort(&mut self) -> anyhow::Result<()> {
        self.inner.abort()
    }

    pub fn prefork(&mut self) -> anyhow::Result<()> {
        self.inner.prefork()
    }

    pub fn postfork_child(&mut self) -> anyhow::Result<()> {
        self.inner.postfork_child()
    }

    pub fn postfork_parent(&mut self) -> anyhow::Result<()> {
        self.inner.postfork_parent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pprof::test_utils::{deserialize_compressed_pprof, string_table_fetch};
    use libdd_common::test_utils::{
        create_temp_file_path, parse_http_request_sync, HttpRequest, TempFileGuard,
    };
    use serde_json::json;
    use std::time::{Duration, Instant};

    const TEST_LIB_NAME: &str = "dd-trace-test";
    const TEST_LIB_VERSION: &str = "1.0.0";
    const TEST_FAMILY: &str = "test";

    fn create_test_profile() -> Box<Profile> {
        let wall_time = ffi::SampleType::WallTime;
        let period = ffi::Period {
            value_type: wall_time,
            value: 60,
        };
        Profile::create(vec![ffi::SampleType::WallTime], &period).unwrap()
    }

    fn create_test_dictionary() -> Box<ProfilesDictionary> {
        ProfilesDictionary::create().unwrap()
    }

    fn create_test_profile_with_dictionary(dictionary: &ProfilesDictionary) -> Box<Profile> {
        let wall_time = ffi::SampleType::WallTime;
        let period = ffi::Period {
            value_type: wall_time,
            value: 60,
        };
        Profile::create_with_dictionary(vec![ffi::SampleType::WallTime], &period, dictionary)
            .unwrap()
    }

    fn create_test_sample2_parts(
        dictionary: &ProfilesDictionary,
    ) -> (ffi::Location2, Vec<i64>, Vec<ffi::Label2<'static>>) {
        let filename_mapping = dictionary.insert_string("/usr/lib/libtest.so").unwrap();
        let filename_function = dictionary.insert_string("/src/test.cpp").unwrap();
        let build_id = dictionary.insert_string("abc123").unwrap();
        let name = dictionary.insert_string("test_function").unwrap();
        let system_name = dictionary.insert_string("_Z13test_functionv").unwrap();
        let label_key = dictionary.insert_string("pid").unwrap();
        let mapping = dictionary
            .insert_mapping(&ffi::Mapping2 {
                memory_start: 0x10000000,
                memory_limit: 0x20000000,
                file_offset: 0,
                filename: filename_mapping,
                build_id,
            })
            .unwrap();
        let function = dictionary
            .insert_function(&ffi::Function2 {
                name,
                system_name,
                file_name: filename_function,
            })
            .unwrap();

        (
            ffi::Location2 {
                mapping,
                function,
                address: 0x10003000,
                line: 100,
            },
            vec![1000000],
            vec![ffi::Label2 {
                key: label_key,
                str: "",
                num: 101,
                num_unit: "",
            }],
        )
    }

    fn create_test_location(address: u64, line: i64) -> ffi::Location<'static> {
        ffi::Location {
            mapping: ffi::Mapping {
                memory_start: address & 0xFFFF0000,
                memory_limit: (address & 0xFFFF0000) + 0x10000000,
                file_offset: 0,
                filename: "/usr/lib/libtest.so",
                build_id: "abc123",
            },
            function: ffi::Function {
                name: "test_function",
                system_name: "_Z13test_functionv",
                filename: "/src/test.cpp",
            },
            address,
            line,
        }
    }

    fn create_test_sample() -> ffi::Sample<'static> {
        ffi::Sample {
            locations: vec![create_test_location(0x10003000, 100)],
            values: vec![1000000],
            labels: vec![],
        }
    }

    fn create_test_exporter() -> Box<ProfileExporter> {
        ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![ffi::Tag {
                key: "env",
                value: "test",
            }],
            "http://localhost:1", // Port 1 unlikely to have server
            100,
            false,
        )
        .unwrap()
    }

    fn create_test_file_exporter(test_name: &str) -> (Box<ProfileExporter>, TempFileGuard) {
        let file_path = create_temp_file_path(test_name, "http");
        let exporter = ProfileExporter::create_file_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![ffi::Tag {
                key: "env",
                value: "test",
            }],
            file_path.to_string_lossy().as_ref(),
        )
        .unwrap();

        (exporter, file_path)
    }

    fn read_dumped_request_and_event(
        file_path: &std::path::Path,
    ) -> (HttpRequest, serde_json::Value) {
        let request_bytes = std::fs::read(file_path).expect("read dumped request");
        let request = parse_http_request_sync(&request_bytes).expect("parse dumped request");
        let event_part = request
            .multipart_parts
            .iter()
            .find(|part| part.filename.as_deref() == Some("event.json"))
            .expect("event.json multipart part");
        let event_json = serde_json::from_slice(&event_part.content).expect("parse event.json");

        (request, event_json)
    }

    fn wait_for_request(file_path: &std::path::Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if file_path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        file_path.exists()
    }

    #[test]
    fn test_profile_operations() {
        let mut profile = create_test_profile();

        // Verify profile starts empty
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should start with no samples"
        );

        // Add samples and verify they're tracked
        let sample = create_test_sample();
        profile.add_sample(&sample).unwrap();
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            1,
            "Profile should have 1 sample after adding"
        );

        // Add another sample with different address
        let sample2 = ffi::Sample {
            locations: vec![create_test_location(0x20003000, 200)],
            values: vec![2000000],
            labels: vec![],
        };
        profile.add_sample(&sample2).unwrap();
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            2,
            "Profile should have 2 samples"
        );

        // Test endpoints
        profile.add_endpoint(12345, "/api/test").unwrap();
        profile.add_endpoint(67890, "/api/other").unwrap();
        profile.add_endpoint_count("/api/test", 100).unwrap();

        // Test upscaling rules (verify they don't error)
        profile
            .add_upscaling_rule_poisson(&[0], "thread_id", "0", 0, 0, 1000000)
            .unwrap();
        profile
            .add_upscaling_rule_proportional(&[0], "thread_id", "1", 100.0)
            .unwrap();
        profile
            .add_upscaling_rule_poisson_non_sample_type_count(
                &[0],
                "thread_id",
                "2",
                0,
                50,
                1000000,
            )
            .unwrap();

        // Serialize and verify output
        let serialized = profile.serialize_to_vec().unwrap();
        assert!(
            serialized.len() > 100,
            "Serialized profile should be non-trivial"
        );

        // Verify it's a valid pprof by checking for gzip/zstd magic bytes
        assert!(
            serialized.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) || // zstd magic
            serialized.starts_with(&[0x1f, 0x8b]), // gzip magic
            "Serialized profile should be compressed"
        );

        // After serialization (which resets), profile should be empty
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should be empty after serialize_to_vec"
        );

        // Add sample and test explicit reset
        profile.add_sample(&sample).unwrap();
        assert_eq!(profile.inner.only_for_testing_num_aggregated_samples(), 1);
        profile.reset().unwrap();
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should be empty after reset"
        );
    }

    #[test]
    fn test_profile_timestamped_sample_operations() {
        let mut profile = create_test_profile();
        let sample = create_test_sample();

        profile.add_sample_with_timestamp(&sample, 42).unwrap();

        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Timestamped samples should not be aggregated into the non-timestamped bucket"
        );
        assert_eq!(
            profile.inner.only_for_testing_num_timestamped_samples(),
            1,
            "Profile should have 1 timestamped sample after adding"
        );

        let serialized = profile.serialize_to_vec().unwrap();
        assert!(
            serialized.len() > 100,
            "Serialized timestamped profile should be non-trivial"
        );
        assert!(
            serialized.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
                || serialized.starts_with(&[0x1f, 0x8b]),
            "Serialized timestamped profile should be compressed"
        );
    }

    #[test]
    fn test_profile_timestamped_sample_rejects_zero_timestamp() {
        let mut profile = create_test_profile();
        let sample = create_test_sample();

        let err = profile.add_sample_with_timestamp(&sample, 0).unwrap_err();

        assert!(err.to_string().contains("endtime_ns must be non-zero"));
    }

    #[test]
    fn test_profile_timestamped_sample_serializes_end_timestamp_label() {
        let mut profile = create_test_profile();
        let sample = create_test_sample();

        profile.add_sample_with_timestamp(&sample, 42).unwrap();

        let serialized = profile.serialize_to_vec().unwrap();
        let pprof = deserialize_compressed_pprof(&serialized).unwrap();
        let timestamp_labels = pprof
            .samples
            .iter()
            .flat_map(|sample| sample.labels.iter())
            .filter(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns")
            .collect::<Vec<_>>();

        assert_eq!(timestamp_labels.len(), 1);
        let timestamp_label = timestamp_labels[0];
        assert_eq!(timestamp_label.num, 42);
        assert_eq!(string_table_fetch(&pprof, timestamp_label.str), "");
        assert_eq!(string_table_fetch(&pprof, timestamp_label.num_unit), "");
    }

    #[test]
    fn test_profile_timestamped_identical_samples_remain_distinct() {
        let mut profile = create_test_profile();
        let sample = create_test_sample();

        profile.add_sample_with_timestamp(&sample, 42).unwrap();
        profile.add_sample_with_timestamp(&sample, 43).unwrap();

        let serialized = profile.serialize_to_vec().unwrap();
        let pprof = deserialize_compressed_pprof(&serialized).unwrap();
        let mut timestamps = pprof
            .samples
            .iter()
            .flat_map(|sample| sample.labels.iter())
            .filter(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns")
            .map(|label| label.num)
            .collect::<Vec<_>>();

        timestamps.sort_unstable();

        assert_eq!(timestamps, vec![42, 43]);
    }

    #[test]
    fn test_profiles_dictionary_operations() {
        let dictionary = create_test_dictionary();
        let filename = dictionary.insert_string("example.py").unwrap();
        let build_id = dictionary.insert_string("build-id").unwrap();
        let function_name = dictionary.insert_string("function").unwrap();
        let system_name = dictionary.insert_string("system_function").unwrap();

        let mapping = dictionary
            .insert_mapping(&ffi::Mapping2 {
                memory_start: 1,
                memory_limit: 2,
                file_offset: 3,
                filename,
                build_id,
            })
            .unwrap();
        let function = dictionary
            .insert_function(&ffi::Function2 {
                name: function_name,
                system_name,
                file_name: dictionary.insert_string("example.py").unwrap(),
            })
            .unwrap();

        assert!(!mapping.handle.is_null());
        assert!(!function.handle.is_null());
    }

    #[test]
    fn test_profile_add_sample2_serializes() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_sample2_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::Sample2 {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_sample2(&sample, 42).unwrap();

        let serialized = profile.serialize_to_vec().unwrap();
        assert!(
            serialized.len() > 100,
            "Serialized api2 profile should be non-trivial"
        );
    }

    #[test]
    fn test_profile_add_sample2_profile_holds_dictionary_alive() {
        let dictionary = create_test_dictionary();
        let (location, values, labels) = create_test_sample2_parts(&dictionary);
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        drop(dictionary);

        let locations = vec![location];
        let sample = ffi::Sample2 {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_sample2(&sample, 42).unwrap();

        let serialized = profile.serialize_to_vec().unwrap();
        assert!(serialized.len() > 100);
    }

    #[test]
    fn test_profile_add_sample2_serializes_dictionary_label_and_timestamp() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_sample2_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::Sample2 {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_sample2(&sample, 42).unwrap();

        let serialized = profile.serialize_to_vec().unwrap();
        let pprof = deserialize_compressed_pprof(&serialized).unwrap();

        let sample = pprof.samples.first().expect("serialized sample");
        let pid_label = sample
            .labels
            .iter()
            .find(|label| string_table_fetch(&pprof, label.key) == "pid")
            .expect("pid label");
        assert_eq!(pid_label.num, 101);

        let timestamp_label = sample
            .labels
            .iter()
            .find(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns")
            .expect("end_timestamp_ns label");
        assert_eq!(timestamp_label.num, 42);
    }

    #[test]
    fn test_profile_add_sample2_identical_timestamped_samples_remain_distinct() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_sample2_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::Sample2 {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_sample2(&sample, 42).unwrap();
        profile.add_sample2(&sample, 43).unwrap();

        let serialized = profile.serialize_to_vec().unwrap();
        let pprof = deserialize_compressed_pprof(&serialized).unwrap();
        let mut timestamps = pprof
            .samples
            .iter()
            .flat_map(|sample| sample.labels.iter())
            .filter(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns")
            .map(|label| label.num)
            .collect::<Vec<_>>();

        timestamps.sort_unstable();

        assert_eq!(timestamps, vec![42, 43]);
    }

    #[test]
    fn test_profile_add_sample2_accepts_zero_timestamp_as_none() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_sample2_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::Sample2 {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_sample2(&sample, 0).unwrap();
        assert_eq!(profile.inner.only_for_testing_num_aggregated_samples(), 1);
        assert_eq!(profile.inner.only_for_testing_num_timestamped_samples(), 0);

        let serialized = profile.serialize_to_vec().unwrap();
        let pprof = deserialize_compressed_pprof(&serialized).unwrap();
        let has_timestamp_label = pprof
            .samples
            .iter()
            .flat_map(|sample| sample.labels.iter())
            .any(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns");

        assert!(!has_timestamp_label);
    }

    #[test]
    fn test_profile_serialize_returns_encoded_profile_and_resets() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();

        let encoded = profile.serialize().unwrap();

        assert!(
            encoded.bytes().len() > 100,
            "Encoded profile should contain non-trivial compressed bytes"
        );
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should be empty after serialize"
        );
    }

    #[test]
    fn test_send_encoded_profile_with_attachments() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();
        profile.add_endpoint_count("/api/test", 100).unwrap();

        let encoded = profile.serialize().unwrap();
        let mut exporter = create_test_exporter();
        let attachment_data = br#"{"test": "data", "number": 123}"#.to_vec();

        // Should fail with connection error because test exporter points at localhost:1,
        // but this validates request construction and the encoded-profile API boundary.
        let result = exporter.send_encoded_profile(
            encoded,
            vec![ffi::AttachmentFile {
                name: "metadata.json",
                data: &attachment_data,
            }],
            vec![ffi::Tag {
                key: "profile_type",
                value: "cpu",
            }],
            "language:rust,profiler_version:1.0",
            r#"{"version": "1.0", "profiler": "test"}"#,
            r#"{"os": "linux", "arch": "x86_64", "cores": 8}"#,
        );

        assert!(result.is_err(), "Should fail when no server is available");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_send_encoded_profile_file_export_preserves_metadata() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();
        profile.add_endpoint_count("/api/test", 2).unwrap();
        profile.add_endpoint_count("/api/test", 3).unwrap();
        profile.add_endpoint_count("/api/other", 7).unwrap();

        let encoded = profile.serialize().unwrap();
        let (mut exporter, file_path) = create_test_file_exporter("cxx_send_encoded_profile");
        let attachment_data = br#"{"test": "data", "number": 123}"#.to_vec();

        exporter
            .send_encoded_profile(
                encoded,
                vec![ffi::AttachmentFile {
                    name: "metadata.json",
                    data: &attachment_data,
                }],
                vec![ffi::Tag {
                    key: "profile_type",
                    value: "cpu",
                }],
                "language:rust,profiler_version:1.0",
                r#"{"version": "1.0", "profiler": "test"}"#,
                r#"{"os": "linux", "arch": "x86_64", "cores": 8}"#,
            )
            .unwrap();

        let (request, event_json) = read_dumped_request_and_event(file_path.as_ref());
        assert_eq!(request.method, "POST");
        assert_eq!(
            event_json["attachments"],
            json!(["metadata.json", "profile.pprof"])
        );
        assert_eq!(
            event_json["endpoint_counts"],
            json!({
                "/api/test": 5,
                "/api/other": 7,
            })
        );
        assert_eq!(
            event_json["process_tags"],
            "language:rust,profiler_version:1.0"
        );
        assert_eq!(event_json["internal"]["version"], "1.0");
        assert_eq!(event_json["internal"]["profiler"], "test");
        assert_eq!(
            event_json["internal"]["libdatadog_version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(event_json["info"]["os"], "linux");
        assert_eq!(event_json["info"]["arch"], "x86_64");
        assert_eq!(event_json["info"]["cores"], 8);

        let tags_profiler = event_json["tags_profiler"].as_str().unwrap();
        assert!(tags_profiler.split(',').any(|tag| tag == "env:test"));
        assert!(tags_profiler
            .split(',')
            .any(|tag| tag == "profile_type:cpu"));
        assert!(tags_profiler
            .split(',')
            .any(|tag| tag.starts_with("runtime_platform:")));

        let attachment_part = request
            .multipart_parts
            .iter()
            .find(|part| part.filename.as_deref() == Some("metadata.json"))
            .expect("metadata.json multipart part");
        assert!(!attachment_part.content.is_empty());
        let profile_part = request
            .multipart_parts
            .iter()
            .find(|part| part.name == "profile.pprof")
            .expect("profile.pprof multipart part");
        assert!(!profile_part.content.is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_send_encoded_profile_with_cancelled_token_does_not_send() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();
        let encoded = profile.serialize().unwrap();
        let (mut exporter, file_path) =
            create_test_file_exporter("cxx_send_encoded_profile_cancelled");
        let cancel = new_cancellation_token();
        cancel.cancel();

        let result = exporter.send_encoded_profile_with_cancellation(
            encoded,
            vec![],
            vec![],
            "",
            "",
            "",
            &cancel,
        );

        assert!(result.is_err(), "pre-cancelled upload should fail");
        assert!(
            !file_path.exists(),
            "pre-cancelled upload should not write a request dump"
        );
    }

    #[test]
    fn test_profile_add_sample2_rejects_wrong_value_count() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, _values, labels) = create_test_sample2_parts(&dictionary);
        let locations = vec![location];
        let values = Vec::new();
        let sample = ffi::Sample2 {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        assert!(profile.add_sample2(&sample, 42).is_err());
    }

    #[test]
    fn test_profile_add_sample2_requires_dictionary_profile() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile();
        let (location, values, labels) = create_test_sample2_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::Sample2 {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        let err = profile.add_sample2(&sample, 42).unwrap_err();
        assert!(format!("{err:#}").contains("profiles dictionary not set"));
    }

    #[test]
    fn test_exporter_create() {
        // Test agent exporter with default timeout
        assert!(ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![ffi::Tag {
                key: "service",
                value: "test"
            }],
            "http://localhost:8126",
            0,
            false,
        )
        .is_ok());

        // Test with multiple tags and custom timeout
        assert!(ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![
                ffi::Tag {
                    key: "service",
                    value: "my-service"
                },
                ffi::Tag {
                    key: "env",
                    value: "prod"
                },
                ffi::Tag {
                    key: "version",
                    value: "2.0"
                },
            ],
            "http://localhost:8126",
            10000,
            false,
        )
        .is_ok());

        // Test agentless exporters with different sites
        assert!(ProfileExporter::create_agentless_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![],
            "datadoghq.com",
            "fake-api-key",
            5000,
            false,
        )
        .is_ok());

        assert!(ProfileExporter::create_agentless_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![],
            "datadoghq.eu",
            "fake-api-key",
            0,
            false,
        )
        .is_ok());

        // Test with no tags
        assert!(ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![],
            "http://localhost:8126",
            0,
            false,
        )
        .is_ok());
    }

    #[test]
    fn test_type_conversions() {
        // AttachmentFile conversion
        let data = vec![1u8, 2, 3, 4, 5, 255, 128, 0];
        let file: exporter::File = (&ffi::AttachmentFile {
            name: "test.bin",
            data: &data,
        })
            .into();
        assert_eq!(file.name, "test.bin");
        assert_eq!(file.bytes, data.as_slice());

        // Tag conversion with special characters
        let tag: libdd_common::tag::Tag = (&ffi::Tag {
            key: "test-key.with_special:chars",
            value: "test_value/with@special#chars",
        })
            .try_into()
            .unwrap();
        assert_eq!(
            tag.as_ref(),
            "test-key.with_special:chars:test_value/with@special#chars"
        );

        // Tag validation - empty key should fail
        assert!(TryInto::<libdd_common::tag::Tag>::try_into(&ffi::Tag {
            key: "",
            value: "value"
        })
        .is_err());

        // SampleType conversion
        let st: api::SampleType = ffi::SampleType::CpuSamples.try_into().unwrap();
        let vt: api::ValueType<'static> = st.into();
        assert_eq!(vt.r#type, "cpu-samples");
        assert_eq!(vt.unit, "count");

        // Mapping conversion
        let mapping: api::Mapping = (&ffi::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0x100,
            filename: "/lib/test.so",
            build_id: "build123",
        })
            .into();
        assert_eq!(
            (
                mapping.memory_start,
                mapping.memory_limit,
                mapping.file_offset
            ),
            (0x1000, 0x2000, 0x100)
        );
        assert_eq!(
            (mapping.filename, mapping.build_id),
            ("/lib/test.so", "build123")
        );

        // Function conversion
        let function: api::Function = (&ffi::Function {
            name: "my_func",
            system_name: "_Z7my_funcv",
            filename: "/src/file.cpp",
        })
            .into();
        assert_eq!(
            (function.name, function.system_name, function.filename),
            ("my_func", "_Z7my_funcv", "/src/file.cpp")
        );

        // Label conversion
        let label: api::Label = (&ffi::Label {
            key: "thread_id",
            str: "",
            num: 42,
            num_unit: "thread",
        })
            .into();
        assert_eq!(
            (label.key, label.num, label.num_unit),
            ("thread_id", 42, "thread")
        );
    }

    #[test]
    fn test_send_profile_with_attachments() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();

        let mut exporter = create_test_exporter();
        let attachment_data = br#"{"test": "data", "number": 123}"#.to_vec();

        // Send with full parameters - should fail with connection error but build request correctly
        let result = exporter.send_profile(
            &mut profile,
            vec![ffi::AttachmentFile {
                name: "metadata.json",
                data: &attachment_data,
            }],
            vec![
                ffi::Tag {
                    key: "profile_type",
                    value: "cpu",
                },
                ffi::Tag {
                    key: "runtime",
                    value: "native",
                },
            ],
            "language:rust,profiler_version:1.0",
            r#"{"version": "1.0", "profiler": "test"}"#,
            r#"{"os": "linux", "arch": "x86_64", "cores": 8}"#,
        );

        assert!(result.is_err(), "Should fail when no server available");
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should be reset after send attempt"
        );

        // Test with empty optional parameters
        profile.add_sample(&create_test_sample()).unwrap();
        let result2 = exporter.send_profile(&mut profile, vec![], vec![], "", "", "");
        assert!(
            result2.is_err(),
            "Should fail with empty optional params too"
        );
    }

    #[test]
    fn test_exporter_manager_create_and_abort() {
        let exporter = create_test_exporter();
        let mut manager = ExporterManager::new_manager(exporter).unwrap();

        // Abort immediately
        manager.abort().unwrap();
    }

    #[test]
    fn test_exporter_manager_queue_and_abort() {
        let exporter = create_test_exporter();
        let manager = ExporterManager::new_manager(exporter).unwrap();

        // Queue a profile
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();

        manager
            .queue_profile(&mut profile, vec![], vec![], "", "", "")
            .unwrap();

        // Give worker thread time to process
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Verify profile was reset
        assert_eq!(profile.inner.only_for_testing_num_aggregated_samples(), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_exporter_manager_queue_encoded_profile_writes_file_export() {
        let (exporter, file_path) = create_test_file_exporter("cxx_queue_encoded_profile");
        let manager = ExporterManager::new_manager(exporter).unwrap();

        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();
        profile.add_endpoint_count("/queued", 11).unwrap();
        let encoded = profile.serialize().unwrap();

        manager
            .queue_encoded_profile(
                encoded,
                vec![],
                vec![ffi::Tag {
                    key: "profile_type",
                    value: "wall",
                }],
                "runtime:rust",
                r#"{"queued": true}"#,
                r#"{"worker": "background"}"#,
            )
            .unwrap();

        assert!(
            wait_for_request(file_path.as_ref(), Duration::from_secs(5)),
            "queued encoded profile should be exported"
        );
        let (_request, event_json) = read_dumped_request_and_event(file_path.as_ref());
        assert_eq!(event_json["attachments"], json!(["profile.pprof"]));
        assert_eq!(event_json["endpoint_counts"], json!({ "/queued": 11 }));
        assert_eq!(event_json["process_tags"], "runtime:rust");
        assert_eq!(event_json["internal"]["queued"], true);
        assert_eq!(event_json["info"]["worker"], "background");
        assert!(event_json["tags_profiler"]
            .as_str()
            .unwrap()
            .split(',')
            .any(|tag| tag == "profile_type:wall"));

        let mut manager = manager;
        manager.abort().unwrap();
    }

    #[test]
    fn test_exporter_manager_prefork_and_postfork() {
        let exporter = create_test_exporter();
        let mut manager = ExporterManager::new_manager(exporter).unwrap();

        // Queue some work
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();
        manager
            .queue_profile(&mut profile, vec![], vec![], "", "", "")
            .unwrap();

        // Prefork
        manager.prefork().unwrap();

        // Postfork parent - should re-queue inflight
        manager.postfork_parent().unwrap();

        // Give time for processing
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Abort parent
        manager.abort().unwrap();
    }

    #[test]
    fn test_exporter_manager_postfork_child() {
        let exporter = create_test_exporter();
        let mut manager = ExporterManager::new_manager(exporter).unwrap();

        // Queue some work
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();
        manager
            .queue_profile(&mut profile, vec![], vec![], "", "", "")
            .unwrap();

        // Prefork
        manager.prefork().unwrap();

        // Postfork child - should discard inflight
        manager.postfork_child().unwrap();

        // Child can queue its own work
        let mut child_profile = create_test_profile();
        child_profile.add_sample(&create_test_sample()).unwrap();
        manager
            .queue_profile(&mut child_profile, vec![], vec![], "", "", "")
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));
        manager.abort().unwrap();
    }

    #[test]
    fn test_exporter_manager_cannot_use_after_abort() {
        let exporter = create_test_exporter();
        let mut manager = ExporterManager::new_manager(exporter).unwrap();

        // Abort the manager
        manager.abort().unwrap();

        // Trying to queue after abort should fail
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();

        let result = manager.queue_profile(&mut profile, vec![], vec![], "", "", "");
        assert!(result.is_err(), "Should fail to queue after abort");
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Suspended") || error_msg.contains("state"),
            "Error message should indicate manager is in Suspended state, got: {}",
            error_msg
        );
    }
}
