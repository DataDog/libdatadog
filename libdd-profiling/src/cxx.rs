// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CXX bindings for profiling module - provides a safe and idiomatic C++ API

#![allow(clippy::needless_lifetimes)]

pub struct DictionaryStringIdOpaque {
    _private: [u8; 0],
}

pub struct DictionaryFunctionIdOpaque {
    _private: [u8; 0],
}

pub struct DictionaryMappingIdOpaque {
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
        locations: &'a [Location<'a>],
        values: &'a [i64],
        labels: &'a [Label<'a>],
    }

    /// Opaque pointer-shaped dictionary string id.
    ///
    /// C++ callers must treat handle as opaque. Do not dereference it, invent
    /// non-null values, persist non-null values beyond the ProfileDictionary
    /// lifetime, or compare values across dictionaries. A null/default handle
    /// represents the empty string. A non-null handle is only valid with the
    /// ProfileDictionary that produced it, or Profiles created from that
    /// dictionary.
    struct DictionaryStringId {
        handle: *mut DictionaryStringIdOpaque,
    }

    /// Opaque pointer-shaped dictionary function id.
    ///
    /// C++ callers must treat handle as opaque. Do not dereference it, invent
    /// non-null values, persist non-null values beyond the ProfileDictionary
    /// lifetime, or compare values across dictionaries. A null/default handle
    /// represents the default/unknown function. A non-null handle is only valid
    /// with the ProfileDictionary that produced it, or Profiles created from
    /// that dictionary.
    struct DictionaryFunctionId {
        handle: *mut DictionaryFunctionIdOpaque,
    }

    /// Opaque pointer-shaped dictionary mapping id.
    ///
    /// C++ callers must treat handle as opaque. Do not dereference it, invent
    /// non-null values, persist non-null values beyond the ProfileDictionary
    /// lifetime, or compare values across dictionaries. A null/default handle
    /// represents an unknown/no mapping. A non-null handle is only valid with
    /// the ProfileDictionary that produced it, or Profiles created from that
    /// dictionary.
    struct DictionaryMappingId {
        handle: *mut DictionaryMappingIdOpaque,
    }

    /// Function data for interning into a ProfileDictionary.
    ///
    /// String ids may be null/default to represent empty strings. Non-null ids
    /// must come from the same ProfileDictionary receiving the interning operation.
    struct DictionaryFunction {
        name: DictionaryStringId,
        system_name: DictionaryStringId,
        filename: DictionaryStringId,
    }

    /// Mapping data for interning into a ProfileDictionary.
    ///
    /// String ids may be null/default to represent empty strings. Non-null ids
    /// must come from the same ProfileDictionary receiving the interning operation.
    struct DictionaryMapping {
        memory_start: u64,
        memory_limit: u64,
        file_offset: u64,
        filename: DictionaryStringId,
        build_id: DictionaryStringId,
    }

    /// Dictionary-backed location.
    ///
    /// mapping and function may be null/default to represent unknown values.
    /// Non-null ids must come from the same ProfileDictionary used to create
    /// the Profile receiving this location.
    struct DictionaryLocation {
        mapping: DictionaryMappingId,
        function: DictionaryFunctionId,
        address: u64,
        line: i64,
    }

    /// Dictionary-backed label.
    ///
    /// key may be null/default to represent an empty key, though callers should
    /// normally use a meaningful dictionary string. Non-null key ids must come
    /// from the same ProfileDictionary used to create the Profile receiving
    /// this label. str and num_unit are borrowed only for the duration of the
    /// add_dictionary_sample call.
    struct DictionaryLabel<'a> {
        key: DictionaryStringId,
        str: &'a str,
        num: i64,
        num_unit: &'a str,
    }

    /// Dictionary-backed sample.
    ///
    /// locations, values, and labels are borrowed only for the duration of the
    /// add_dictionary_sample call. Null/default ids represent empty or unknown values.
    /// All non-null ids in locations and labels must come from the
    /// ProfileDictionary used to create the Profile receiving this sample.
    struct DictionarySample<'a> {
        locations: &'a [DictionaryLocation],
        values: &'a [i64],
        labels: &'a [DictionaryLabel<'a>],
    }

    struct Tag<'a> {
        key: &'a str,
        value: &'a str,
    }

    struct AttachmentFile<'a> {
        name: &'a str,
        data: &'a [u8],
    }

    /// Explicit no-exception status for CXX callers.
    ///
    /// These status-returning APIs are intended for runtimes that cannot allow
    /// Rust errors to cross the CXX bridge as C++ exceptions. Success uses an
    /// empty message; failure carries the operation and a human-readable error
    /// string. Prefer ok() or check_and_*() methods over inspecting fields
    /// directly.
    ///
    /// #[must_use] catches ignored statuses in Rust tests/helpers, but cxx does
    /// not currently translate it to C++ [[nodiscard]] in generated headers.
    #[must_use]
    struct Status {
        success: bool,
        operation_name: String,
        details: String,
    }

    enum ErrorPolicy {
        PrintImmediately,
        StoreFirstPerOperation,
        StoreEveryOccurrence,
    }

    struct Error {
        operation: String,
        message: String,
    }

    // Opaque Rust types
    extern "Rust" {
        // CXX exposes Rust Result<T> errors as C++ exceptions. Profiling CXX APIs avoid
        // Result<T> in bridge signatures and report failures through Status or typed
        // result wrappers instead.
        type Profile;
        type ProfileResult;
        type ProfileExporter;
        type ProfileExporterResult;
        type EncodedProfile;
        type EncodedProfileResult;
        type ProfileDictionary;
        type ProfileDictionaryResult;
        type DictionaryStringIdOpaque;
        type DictionaryFunctionIdOpaque;
        type DictionaryMappingIdOpaque;
        type CancellationToken;

        fn ok(self: &Status) -> bool;
        fn operation(self: &Status) -> String;
        fn message(self: &Status) -> String;
        /// Returns true on success. On failure, prints operation and message to stderr.
        fn check_and_print(self: &Status) -> bool;

        fn ok(self: &ProfileResult) -> bool;
        fn message(self: &ProfileResult) -> String;
        fn check_and_print(self: &ProfileResult) -> bool;
        /// Takes the profile from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take_value(self: &mut ProfileResult) -> Box<Profile>;
        fn ok(self: &ProfileExporterResult) -> bool;
        fn message(self: &ProfileExporterResult) -> String;
        fn check_and_print(self: &ProfileExporterResult) -> bool;
        /// Takes the exporter from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take_value(self: &mut ProfileExporterResult) -> Box<ProfileExporter>;
        fn ok(self: &EncodedProfileResult) -> bool;
        fn message(self: &EncodedProfileResult) -> String;
        fn check_and_print(self: &EncodedProfileResult) -> bool;
        /// Takes the encoded profile from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take_value(self: &mut EncodedProfileResult) -> Box<EncodedProfile>;
        fn ok(self: &ProfileDictionaryResult) -> bool;
        fn message(self: &ProfileDictionaryResult) -> String;
        fn check_and_print(self: &ProfileDictionaryResult) -> bool;
        /// Takes the dictionary from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take_value(self: &mut ProfileDictionaryResult) -> Box<ProfileDictionary>;

        fn is_null(self: &DictionaryStringId) -> bool;
        fn is_null(self: &DictionaryFunctionId) -> bool;
        fn is_null(self: &DictionaryMappingId) -> bool;

        // CancellationToken factory and methods
        #[Self = "CancellationToken"]
        fn create() -> Box<CancellationToken>;
        fn clone(self: &CancellationToken) -> Box<CancellationToken>;
        fn cancel(self: &CancellationToken);

        // Static factory methods for Profile
        #[Self = "Profile"]
        fn create(sample_types: Vec<SampleType>, period: &Period) -> Box<ProfileResult>;

        // Static factory methods for ProfileDictionary
        #[Self = "ProfileDictionary"]
        fn create() -> Box<ProfileDictionaryResult>;

        fn set_error_policy(self: &ProfileDictionary, policy: ErrorPolicy);
        fn take_errors(self: &ProfileDictionary) -> Vec<Error>;

        /// Interns value into this dictionary and writes the opaque id to out.
        ///
        /// Returns false on failure. Error details are reported through the
        /// owning ProfileDictionary. The returned id must only be used with
        /// this dictionary or Profiles created from it.
        fn intern_string(
            self: &ProfileDictionary,
            value: &str,
            out: &mut DictionaryStringId,
        ) -> bool;

        /// Interns function into this dictionary and writes the opaque id to out.
        ///
        /// Returns false on failure. Error details are reported through the
        /// owning ProfileDictionary. Null/default ids in function represent
        /// empty strings. Non-null ids in function must have been produced by
        /// this dictionary. The returned id must only be used with this
        /// dictionary or Profiles created from it.
        ///
        /// # Safety
        /// All non-null ids in function must have been produced by this dictionary.
        unsafe fn intern_function(
            self: &ProfileDictionary,
            function: &DictionaryFunction,
            out: &mut DictionaryFunctionId,
        ) -> bool;

        /// Interns mapping into this dictionary and writes the opaque id to out.
        ///
        /// Returns false on failure. Error details are reported through the
        /// owning ProfileDictionary. Null/default ids in mapping represent
        /// empty strings. Non-null ids in mapping must have been produced by
        /// this dictionary. The returned id must only be used with this
        /// dictionary or Profiles created from it.
        ///
        /// # Safety
        /// All non-null ids in mapping must have been produced by this dictionary.
        unsafe fn intern_mapping(
            self: &ProfileDictionary,
            mapping: &DictionaryMapping,
            out: &mut DictionaryMappingId,
        ) -> bool;

        /// Creates a Profile backed by dictionary.
        ///
        /// The Profile keeps dictionary storage alive internally. Future
        /// add_dictionary_sample calls on the Profile may use null/default ids for empty
        /// or unknown values, but all non-null ids must be produced by this
        /// same dictionary.
        #[Self = "Profile"]
        fn create_with_dictionary(
            sample_types: Vec<SampleType>,
            period: &Period,
            dictionary: &ProfileDictionary,
        ) -> Box<ProfileResult>;

        // Profile methods
        fn set_error_policy(self: &mut Profile, policy: ErrorPolicy);
        fn take_errors(self: &mut Profile) -> Vec<Error>;

        /// Adds a sample without an end timestamp.
        fn add_sample(self: &mut Profile, sample: &Sample) -> bool;

        /// Adds a sample with an end timestamp in nanoseconds.
        #[cxx_name = "add_sample"]
        fn add_sample_with_timestamp(self: &mut Profile, sample: &Sample, endtime_ns: i64) -> bool;

        /// Adds a dictionary-backed sample without an end timestamp.
        ///
        /// Null/default ids in sample represent empty or unknown values. All
        /// non-null ids in sample must have been produced by the
        /// ProfileDictionary used to create this Profile. The sample slices are
        /// borrowed only for the duration of this call.
        ///
        /// # Safety
        /// All non-null ids in sample must have been produced by the
        /// ProfileDictionary used to create this Profile.
        unsafe fn add_dictionary_sample(self: &mut Profile, sample: &DictionarySample) -> bool;

        /// Adds a dictionary-backed sample with an end timestamp in nanoseconds.
        ///
        /// # Safety
        /// All non-null ids in sample must have been produced by the
        /// ProfileDictionary used to create this Profile.
        #[cxx_name = "add_dictionary_sample"]
        unsafe fn add_dictionary_sample_with_timestamp(
            self: &mut Profile,
            sample: &DictionarySample,
            endtime_ns: i64,
        ) -> bool;

        fn set_custom_sample_type(
            self: &mut Profile,
            slot: SampleType,
            type_: &str,
            unit: &str,
        ) -> bool;
        fn add_endpoint(self: &mut Profile, local_root_span_id: u64, endpoint: &str) -> bool;
        fn add_endpoint_count(self: &mut Profile, endpoint: &str, value: i64) -> bool;

        // Upscaling rule methods (one for each variant)
        fn add_upscaling_rule_poisson(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            sum_value_offset: usize,
            count_value_offset: usize,
            sampling_distance: u64,
        ) -> bool;

        fn add_upscaling_rule_poisson_non_sample_type_count(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            sum_value_offset: usize,
            count_value: u64,
            sampling_distance: u64,
        ) -> bool;

        fn add_upscaling_rule_proportional(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            scale: f64,
        ) -> bool;

        /// Serialize and reset the profile, returning the encoded previous
        /// profile data. The returned EncodedProfile includes the compressed
        /// pprof bytes plus metadata needed by exporters, such as endpoint
        /// counts.
        fn serialize(self: &mut Profile) -> Box<EncodedProfileResult>;

        /// Return the compressed pprof bytes.
        fn bytes(self: &EncodedProfile) -> &[u8];

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
        ) -> Box<ProfileExporterResult>;

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
        ) -> Box<ProfileExporterResult>;

        #[Self = "ProfileExporter"]
        fn create_file_exporter(
            profiling_library_name: &str,
            profiling_library_version: &str,
            family: &str,
            tags: Vec<Tag>,
            output_path: &str,
        ) -> Box<ProfileExporterResult>;

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
        ) -> Status;

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
        ) -> Status;

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
        ) -> Status;

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
        ) -> Status;

    }
}

mod cancellation;
mod conversions;
mod dictionary;
mod errors;
mod ids;
mod profile;
mod profile_exporter;

pub use self::cancellation::CancellationToken;
pub use self::dictionary::ProfileDictionary;
pub use self::errors::{
    EncodedProfileResult, ProfileDictionaryResult, ProfileExporterResult, ProfileResult,
};
pub use self::profile::{EncodedProfile, Profile};
pub use self::profile_exporter::ProfileExporter;

#[cfg(test)]
mod tests;
