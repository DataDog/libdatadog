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
        GcTime,
        GcSamples,
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
        type ExporterManager;
        type ExporterManagerResult;
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
        /// Returns true on success. On failure, stores this error only if errors
        /// does not already contain an error for the same operation.
        fn check_and_store_first_per_operation(self: &Status, errors: &mut Vec<Error>) -> bool;
        /// Returns true on success. On failure, appends this error to errors.
        fn check_and_store_every_occurrence(self: &Status, errors: &mut Vec<Error>) -> bool;

        fn ok(self: &ProfileResult) -> bool;
        fn message(self: &ProfileResult) -> String;
        fn check_and_print(self: &ProfileResult) -> bool;
        fn check_and_store_first_per_operation(
            self: &ProfileResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        fn check_and_store_every_occurrence(self: &ProfileResult, errors: &mut Vec<Error>) -> bool;
        /// Takes the profile from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take(self: &mut ProfileResult) -> Box<Profile>;
        fn ok(self: &ProfileExporterResult) -> bool;
        fn message(self: &ProfileExporterResult) -> String;
        fn check_and_print(self: &ProfileExporterResult) -> bool;
        fn check_and_store_first_per_operation(
            self: &ProfileExporterResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        fn check_and_store_every_occurrence(
            self: &ProfileExporterResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        /// Takes the exporter from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take(self: &mut ProfileExporterResult) -> Box<ProfileExporter>;
        fn ok(self: &EncodedProfileResult) -> bool;
        fn check_and_print(self: &EncodedProfileResult) -> bool;
        fn check_and_store_first_per_operation(
            self: &EncodedProfileResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        fn check_and_store_every_occurrence(
            self: &EncodedProfileResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        /// Takes the encoded profile from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take(self: &mut EncodedProfileResult) -> Box<EncodedProfile>;
        fn ok(self: &ExporterManagerResult) -> bool;
        fn message(self: &ExporterManagerResult) -> String;
        fn check_and_print(self: &ExporterManagerResult) -> bool;
        fn check_and_store_first_per_operation(
            self: &ExporterManagerResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        fn check_and_store_every_occurrence(
            self: &ExporterManagerResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        /// Takes the manager from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take(self: &mut ExporterManagerResult) -> Box<ExporterManager>;
        fn ok(self: &ProfileDictionaryResult) -> bool;
        fn message(self: &ProfileDictionaryResult) -> String;
        fn check_and_print(self: &ProfileDictionaryResult) -> bool;
        fn check_and_store_first_per_operation(
            self: &ProfileDictionaryResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        fn check_and_store_every_occurrence(
            self: &ProfileDictionaryResult,
            errors: &mut Vec<Error>,
        ) -> bool;
        /// Takes the dictionary from a successful result.
        ///
        /// Call this at most once and only after ok() returns true. Calling it
        /// on a failed result or calling it more than once aborts the process.
        fn take(self: &mut ProfileDictionaryResult) -> Box<ProfileDictionary>;

        fn is_null(self: &DictionaryStringId) -> bool;
        fn is_null(self: &DictionaryFunctionId) -> bool;
        fn is_null(self: &DictionaryMappingId) -> bool;

        // CancellationToken factory and methods
        #[Self = "CancellationToken"]
        fn create() -> Box<CancellationToken>;
        fn clone(self: &CancellationToken) -> Box<CancellationToken>;
        fn cancel(self: &CancellationToken);
        fn is_cancelled(self: &CancellationToken) -> bool;

        // Static factory methods for Profile
        #[Self = "Profile"]
        fn create(sample_types: Vec<SampleType>, period: &Period) -> Box<ProfileResult>;

        /// Create a profile without a sampling period.
        #[Self = "Profile"]
        fn create_no_period(sample_types: Vec<SampleType>) -> Box<ProfileResult>;

        // Static factory methods for ProfileDictionary
        #[Self = "ProfileDictionary"]
        fn create() -> Box<ProfileDictionaryResult>;

        fn set_error_policy(self: &ProfileDictionary, policy: ErrorPolicy);
        fn error_policy(self: &ProfileDictionary) -> ErrorPolicy;
        fn errors(self: &ProfileDictionary) -> Vec<Error>;
        fn take_errors(self: &ProfileDictionary) -> Vec<Error>;
        fn clear_errors(self: &ProfileDictionary);

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
        fn intern_function(
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
        fn intern_mapping(
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
        fn error_policy(self: &Profile) -> ErrorPolicy;
        fn errors(self: &Profile) -> Vec<Error>;
        fn take_errors(self: &mut Profile) -> Vec<Error>;
        fn clear_errors(self: &mut Profile);

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
        fn add_dictionary_sample(self: &mut Profile, sample: &DictionarySample) -> bool;

        /// Adds a dictionary-backed sample with an end timestamp in nanoseconds.
        #[cxx_name = "add_dictionary_sample"]
        fn add_dictionary_sample_with_timestamp(
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

        fn reset(self: &mut Profile) -> bool;

        /// Serialize and reset the profile, returning the encoded previous
        /// profile data. The returned EncodedProfile includes the compressed
        /// pprof bytes plus metadata needed by exporters, such as endpoint
        /// counts.
        fn serialize(self: &mut Profile) -> Box<EncodedProfileResult>;

        /// Serialize and reset the profile, writing only compressed pprof bytes
        /// to out. This is a convenience/debug API; callers that intend to send
        /// the profile should prefer Profile::serialize() plus
        /// ProfileExporter::send_encoded_profile(). Returns false on failure;
        /// error details are reported through the Profile.
        fn serialize_to_vec(self: &mut Profile, out: &mut Vec<u8>) -> bool;

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

        // ExporterManager methods
        /// Creates a new ExporterManager with a background worker thread
        #[Self = "ExporterManager"]
        fn new_manager(exporter: Box<ProfileExporter>) -> Box<ExporterManagerResult>;

        /// Queue a profile to be sent asynchronously by the worker thread
        ///
        /// **Important**: This method resets the profile and queues the *previous* profile data.
        /// After calling this, the profile will be empty and ready for new samples.
        #[allow(clippy::too_many_arguments)]
        fn queue_profile(
            self: &mut ExporterManager,
            profile: &mut Profile,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
        ) -> Status;

        /// Queue a previously serialized profile to be sent asynchronously by
        /// the background worker thread.
        ///
        /// This is the split form of queue_profile().
        #[allow(clippy::too_many_arguments)]
        fn queue_encoded_profile(
            self: &mut ExporterManager,
            encoded: Box<EncodedProfile>,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
        ) -> Status;

        /// Abort the manager, stopping the worker thread
        /// Transitions the manager from Active to Suspended state
        fn abort(self: &mut ExporterManager) -> Status;

        /// Prefork: suspend the manager before forking
        /// Transitions the manager from Active to Suspended state
        fn prefork(self: &mut ExporterManager) -> Status;

        /// Postfork child: reinitialize manager in child process, discarding inflight requests
        /// Transitions the manager from Suspended to Active state
        fn postfork_child(self: &mut ExporterManager) -> Status;

        /// Postfork parent: reinitialize manager in parent process and re-queue inflight requests
        /// Transitions the manager from Suspended to Active state
        fn postfork_parent(self: &mut ExporterManager) -> Status;
    }
}

// ============================================================================
// From Implementations - Convert CXX types to API types
// ============================================================================

impl ffi::Status {
    fn ok_for(operation: &'static str) -> Self {
        Self {
            success: true,
            operation_name: operation.to_string(),
            details: String::new(),
        }
    }

    fn err(operation: &'static str, err: impl std::fmt::Display) -> Self {
        Self {
            success: false,
            operation_name: operation.to_string(),
            details: format!("{err:#}"),
        }
    }

    fn from_result<E>(operation: &'static str, result: std::result::Result<(), E>) -> Self
    where
        E: std::fmt::Display,
    {
        match result {
            Ok(()) => Self::ok_for(operation),
            Err(err) => Self::err(operation, err),
        }
    }

    pub fn ok(&self) -> bool {
        self.success
    }

    pub fn operation(&self) -> String {
        self.operation_name.clone()
    }

    pub fn message(&self) -> String {
        self.details.clone()
    }

    pub fn check_and_print(&self) -> bool {
        if self.success {
            return true;
        }
        eprintln!("{} failed: {}", self.operation_name, self.details);
        false
    }

    pub fn check_and_store_first_per_operation(&self, errors: &mut Vec<ffi::Error>) -> bool {
        if self.success {
            return true;
        }
        if !errors
            .iter()
            .any(|error| error.operation == self.operation_name)
        {
            errors.push(ffi::Error {
                operation: self.operation_name.clone(),
                message: self.details.clone(),
            });
        }
        false
    }

    pub fn check_and_store_every_occurrence(&self, errors: &mut Vec<ffi::Error>) -> bool {
        if self.success {
            return true;
        }
        errors.push(ffi::Error {
            operation: self.operation_name.clone(),
            message: self.details.clone(),
        });
        false
    }

    #[cfg(test)]
    #[track_caller]
    pub fn unwrap(self) {
        assert!(self.success, "{}", self.details);
    }
}

macro_rules! impl_box_result {
    ($result:ident, $value:ty) => {
        #[must_use]
        pub struct $result {
            status: ffi::Status,
            value: Option<Box<$value>>,
        }

        impl $result {
            fn from_result(
                operation: &'static str,
                result: anyhow::Result<Box<$value>>,
            ) -> Box<Self> {
                match result {
                    Ok(value) => Box::new(Self {
                        status: ffi::Status::ok_for(operation),
                        value: Some(value),
                    }),
                    Err(err) => Box::new(Self {
                        status: ffi::Status::err(operation, err),
                        value: None,
                    }),
                }
            }

            pub fn ok(&self) -> bool {
                self.status.ok()
            }

            pub fn check_and_print(&self) -> bool {
                self.status.check_and_print()
            }

            pub fn check_and_store_first_per_operation(
                &self,
                errors: &mut Vec<ffi::Error>,
            ) -> bool {
                self.status.check_and_store_first_per_operation(errors)
            }

            pub fn check_and_store_every_occurrence(&self, errors: &mut Vec<ffi::Error>) -> bool {
                self.status.check_and_store_every_occurrence(errors)
            }

            pub fn take(&mut self) -> Box<$value> {
                match self.value.take() {
                    Some(value) => value,
                    None => std::process::abort(),
                }
            }

            #[cfg(test)]
            #[track_caller]
            pub fn is_ok(&self) -> bool {
                self.status.ok()
            }

            #[cfg(test)]
            #[track_caller]
            pub fn is_err(&self) -> bool {
                !self.status.ok()
            }

            #[cfg(test)]
            #[allow(clippy::boxed_local)]
            #[track_caller]
            pub fn unwrap(mut self: Box<Self>) -> Box<$value> {
                self.status.unwrap();
                self.value.take().expect("successful result has value")
            }
        }
    };
}

macro_rules! impl_box_result_with_message {
    ($result:ident, $value:ty) => {
        impl_box_result!($result, $value);

        impl $result {
            pub fn message(&self) -> String {
                self.status.message()
            }
        }
    };
}

impl_box_result_with_message!(ProfileResult, Profile);
impl_box_result_with_message!(ProfileDictionaryResult, ProfileDictionary);
impl_box_result!(EncodedProfileResult, EncodedProfile);
impl_box_result_with_message!(ProfileExporterResult, ProfileExporter);
impl_box_result_with_message!(ExporterManagerResult, ExporterManager);

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
            ffi::SampleType::GcTime => api::SampleType::GcTime,
            ffi::SampleType::GcSamples => api::SampleType::GcSamples,
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

impl ffi::DictionaryStringId {
    pub fn is_null(&self) -> bool {
        self.handle.is_null()
    }
}

impl ffi::DictionaryFunctionId {
    pub fn is_null(&self) -> bool {
        self.handle.is_null()
    }
}

impl ffi::DictionaryMappingId {
    pub fn is_null(&self) -> bool {
        self.handle.is_null()
    }
}

impl From<profiles::datatypes::StringId2> for ffi::DictionaryStringId {
    fn from(id: profiles::datatypes::StringId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid DictionaryStringId handle produced by
/// libdatadog. Null/default handles represent the empty string. For non-null
/// handles, the producing ProfileDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
unsafe fn dictionary_string_id_from_cxx(
    id: &ffi::DictionaryStringId,
) -> profiles::datatypes::StringId2 {
    unsafe { profiles::datatypes::StringId2::from_raw_ptr(id.handle.cast()) }
}

impl From<profiles::datatypes::FunctionId2> for ffi::DictionaryFunctionId {
    fn from(id: profiles::datatypes::FunctionId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid DictionaryFunctionId handle produced by
/// libdatadog. Null/default handles represent the default/unknown function. For
/// non-null handles, the producing ProfileDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
unsafe fn dictionary_function_id_from_cxx(
    id: &ffi::DictionaryFunctionId,
) -> profiles::datatypes::FunctionId2 {
    unsafe { profiles::datatypes::FunctionId2::from_raw_ptr(id.handle.cast()) }
}

impl From<profiles::datatypes::MappingId2> for ffi::DictionaryMappingId {
    fn from(id: profiles::datatypes::MappingId2) -> Self {
        Self {
            handle: id.into_raw_ptr().cast(),
        }
    }
}

/// # Safety
///
/// id.handle must be null/default or a valid DictionaryMappingId handle produced by
/// libdatadog. Null/default handles represent an unknown/no mapping. For
/// non-null handles, the producing ProfileDictionary must remain alive for any
/// operation that uses the returned id, and callers must only use the id with
/// that dictionary.
unsafe fn dictionary_mapping_id_from_cxx(
    id: &ffi::DictionaryMappingId,
) -> profiles::datatypes::MappingId2 {
    unsafe { profiles::datatypes::MappingId2::from_raw_ptr(id.handle.cast()) }
}

/// # Safety
///
/// All ids in function must be null/default or valid handles produced by the
/// same ProfileDictionary receiving the function interning operation. Null/default ids
/// represent empty strings.
unsafe fn dictionary_function_from_cxx(
    function: &ffi::DictionaryFunction,
) -> profiles::datatypes::Function2 {
    profiles::datatypes::Function2 {
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // same ProfileDictionary receiving the interning operation. Null/default ids
        // represent empty strings.
        name: unsafe { dictionary_string_id_from_cxx(&function.name) },
        system_name: unsafe { dictionary_string_id_from_cxx(&function.system_name) },
        file_name: unsafe { dictionary_string_id_from_cxx(&function.filename) },
    }
}

/// # Safety
///
/// All ids in mapping must be null/default or valid handles produced by the
/// same ProfileDictionary receiving the mapping interning operation. Null/default ids
/// represent empty strings.
unsafe fn dictionary_mapping_from_cxx(
    mapping: &ffi::DictionaryMapping,
) -> profiles::datatypes::Mapping2 {
    profiles::datatypes::Mapping2 {
        memory_start: mapping.memory_start,
        memory_limit: mapping.memory_limit,
        file_offset: mapping.file_offset,
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // same ProfileDictionary receiving the interning operation. Null/default ids
        // represent empty strings.
        filename: unsafe { dictionary_string_id_from_cxx(&mapping.filename) },
        build_id: unsafe { dictionary_string_id_from_cxx(&mapping.build_id) },
    }
}

/// # Safety
///
/// All ids in location must be null/default or valid handles produced by the
/// ProfileDictionary used to create the receiving Profile. Null/default ids
/// represent unknown values.
unsafe fn dictionary_location_from_cxx(location: &ffi::DictionaryLocation) -> api2::Location2 {
    api2::Location2 {
        // SAFETY: The caller guarantees all non-null ids were produced by the
        // ProfileDictionary used to create the receiving Profile. Null/default
        // ids represent unknown values.
        mapping: unsafe { dictionary_mapping_id_from_cxx(&location.mapping) },
        function: unsafe { dictionary_function_id_from_cxx(&location.function) },
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

impl CancellationToken {
    /// Creates a new cancellation token.
    pub fn create() -> Box<CancellationToken> {
        Box::new(CancellationToken {
            inner: tokio_util::sync::CancellationToken::new(),
        })
    }

    /// Clones the cancellation token.
    ///
    /// A cloned token is connected to the original token - either can be used
    /// to cancel or check cancellation status. The useful part is that they have
    /// independent lifetimes and can be dropped separately.
    ///
    /// This is useful for multi-threaded scenarios where one thread performs the
    /// send operation while another thread can cancel it.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Box<CancellationToken> {
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

fn null_dictionary_string_id() -> ffi::DictionaryStringId {
    ffi::DictionaryStringId {
        handle: std::ptr::null_mut(),
    }
}

fn null_dictionary_function_id() -> ffi::DictionaryFunctionId {
    ffi::DictionaryFunctionId {
        handle: std::ptr::null_mut(),
    }
}

fn null_dictionary_mapping_id() -> ffi::DictionaryMappingId {
    ffi::DictionaryMappingId {
        handle: std::ptr::null_mut(),
    }
}

struct ErrorStore {
    policy: ffi::ErrorPolicy,
    errors: Vec<ffi::Error>,
}

impl ErrorStore {
    fn new() -> Self {
        Self {
            policy: ffi::ErrorPolicy::StoreFirstPerOperation,
            errors: Vec::new(),
        }
    }

    fn handle_result(&mut self, operation: &'static str, result: anyhow::Result<()>) -> bool {
        match result {
            Ok(()) => true,
            Err(err) => self.handle_error(operation, &err),
        }
    }

    fn handle_error(&mut self, operation: &'static str, err: impl std::fmt::Display) -> bool {
        match self.policy {
            ffi::ErrorPolicy::PrintImmediately => {
                eprintln!("{operation} failed: {err:#}");
            }
            ffi::ErrorPolicy::StoreFirstPerOperation => {
                if !self.errors.iter().any(|error| error.operation == operation) {
                    self.errors.push(ffi::Error {
                        operation: operation.to_string(),
                        message: format!("{err:#}"),
                    });
                }
            }
            ffi::ErrorPolicy::StoreEveryOccurrence => self.errors.push(ffi::Error {
                operation: operation.to_string(),
                message: format!("{err:#}"),
            }),
            _ => {
                eprintln!("{operation} failed: {err:#}");
            }
        }
        false
    }

    fn set_policy(&mut self, policy: ffi::ErrorPolicy) {
        self.policy = policy;
    }

    fn policy(&self) -> ffi::ErrorPolicy {
        self.policy
    }

    fn errors(&self) -> Vec<ffi::Error> {
        self.errors
            .iter()
            .map(|error| ffi::Error {
                operation: error.operation.clone(),
                message: error.message.clone(),
            })
            .collect()
    }

    fn take_errors(&mut self) -> Vec<ffi::Error> {
        std::mem::take(&mut self.errors)
    }

    fn clear_errors(&mut self) {
        self.errors.clear();
    }
}

pub struct ProfileDictionary {
    inner: profiles::collections::Arc<profiles::datatypes::ProfilesDictionary>,
    errors: std::sync::Mutex<ErrorStore>,
}

impl ProfileDictionary {
    pub fn create() -> Box<ProfileDictionaryResult> {
        ProfileDictionaryResult::from_result(
            "ProfileDictionary::create",
            (|| -> anyhow::Result<Box<ProfileDictionary>> {
                let dictionary = profiles::datatypes::ProfilesDictionary::try_new()
                    .context("ProfileDictionary::create failed")?;
                let inner = profiles::collections::Arc::try_new(dictionary)
                    .map_err(|_| anyhow::anyhow!("failed to allocate ProfileDictionary"))?;

                Ok(Box::new(ProfileDictionary {
                    inner,
                    errors: std::sync::Mutex::new(ErrorStore::new()),
                }))
            })(),
        )
    }

    fn handle_error(&self, operation: &'static str, err: impl std::fmt::Display) {
        match self.errors.lock() {
            Ok(mut errors) => {
                errors.handle_error(operation, err);
            }
            Err(_) => {
                eprintln!("{operation} failed: {err:#}");
            }
        }
    }

    pub fn set_error_policy(&self, policy: ffi::ErrorPolicy) {
        if let Ok(mut errors) = self.errors.lock() {
            errors.set_policy(policy);
        }
    }

    pub fn error_policy(&self) -> ffi::ErrorPolicy {
        match self.errors.lock() {
            Ok(errors) => errors.policy(),
            Err(_) => ffi::ErrorPolicy::StoreFirstPerOperation,
        }
    }

    pub fn errors(&self) -> Vec<ffi::Error> {
        match self.errors.lock() {
            Ok(errors) => errors.errors(),
            Err(_) => Vec::new(),
        }
    }

    pub fn take_errors(&self) -> Vec<ffi::Error> {
        match self.errors.lock() {
            Ok(mut errors) => errors.take_errors(),
            Err(_) => Vec::new(),
        }
    }

    pub fn clear_errors(&self) {
        if let Ok(mut errors) = self.errors.lock() {
            errors.clear_errors();
        }
    }

    pub fn intern_string(&self, value: &str, out: &mut ffi::DictionaryStringId) -> bool {
        match self
            .inner
            .try_insert_str2(value)
            .map(Into::into)
            .context("ProfileDictionary::intern_string failed")
        {
            Ok(id) => {
                *out = id;
                true
            }
            Err(err) => {
                *out = null_dictionary_string_id();
                self.handle_error("ProfileDictionary::intern_string", &err);
                false
            }
        }
    }

    pub fn intern_function(
        &self,
        function: &ffi::DictionaryFunction,
        out: &mut ffi::DictionaryFunctionId,
    ) -> bool {
        // SAFETY: The CXX API contract requires all ids in function to come
        // from this ProfileDictionary.
        let function = unsafe { dictionary_function_from_cxx(function) };
        match self
            .inner
            .try_insert_function2(function)
            .map(Into::into)
            .context("ProfileDictionary::intern_function failed")
        {
            Ok(id) => {
                *out = id;
                true
            }
            Err(err) => {
                *out = null_dictionary_function_id();
                self.handle_error("ProfileDictionary::intern_function", &err);
                false
            }
        }
    }

    pub fn intern_mapping(
        &self,
        mapping: &ffi::DictionaryMapping,
        out: &mut ffi::DictionaryMappingId,
    ) -> bool {
        // SAFETY: The CXX API contract requires all ids in mapping to come
        // from this ProfileDictionary.
        let mapping = unsafe { dictionary_mapping_from_cxx(mapping) };
        match self
            .inner
            .try_insert_mapping2(mapping)
            .map(Into::into)
            .context("ProfileDictionary::intern_mapping failed")
        {
            Ok(id) => {
                *out = id;
                true
            }
            Err(err) => {
                *out = null_dictionary_mapping_id();
                self.handle_error("ProfileDictionary::intern_mapping", &err);
                false
            }
        }
    }
}

pub struct Profile {
    inner: internal::Profile,
    errors: ErrorStore,
}

impl Profile {
    fn new(inner: internal::Profile) -> Self {
        Self {
            inner,
            errors: ErrorStore::new(),
        }
    }

    fn handle_result(&mut self, operation: &'static str, result: anyhow::Result<()>) -> bool {
        self.errors.handle_result(operation, result)
    }

    fn handle_error(&mut self, operation: &'static str, err: impl std::fmt::Display) -> bool {
        self.errors.handle_error(operation, err)
    }

    pub fn set_error_policy(&mut self, policy: ffi::ErrorPolicy) {
        self.errors.set_policy(policy);
    }

    pub fn error_policy(&self) -> ffi::ErrorPolicy {
        self.errors.policy()
    }

    pub fn errors(&self) -> Vec<ffi::Error> {
        self.errors.errors()
    }

    pub fn take_errors(&mut self) -> Vec<ffi::Error> {
        self.errors.take_errors()
    }

    pub fn clear_errors(&mut self) {
        self.errors.clear_errors();
    }

    pub fn create(sample_types: Vec<ffi::SampleType>, period: &ffi::Period) -> Box<ProfileResult> {
        ProfileResult::from_result(
            "Profile::create",
            (|| -> anyhow::Result<Box<Profile>> {
                // Convert (fallibly) from CXX types to API types
                let types: Vec<api::SampleType> = sample_types
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<Vec<_>, _>>()?;
                let period_value: api::Period = period.try_into()?;

                // Profile::try_new interns the strings
                let inner = internal::Profile::try_new(&types, Some(period_value))?;

                Ok(Box::new(Profile::new(inner)))
            })(),
        )
    }

    pub fn create_no_period(sample_types: Vec<ffi::SampleType>) -> Box<ProfileResult> {
        ProfileResult::from_result(
            "Profile::create_no_period",
            (|| -> anyhow::Result<Box<Profile>> {
                let types: Vec<api::SampleType> = sample_types
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<Vec<_>, _>>()?;
                let inner = internal::Profile::try_new(&types, None)?;
                Ok(Box::new(Profile::new(inner)))
            })(),
        )
    }

    pub fn create_with_dictionary(
        sample_types: Vec<ffi::SampleType>,
        period: &ffi::Period,
        dictionary: &ProfileDictionary,
    ) -> Box<ProfileResult> {
        ProfileResult::from_result(
            "Profile::create_with_dictionary",
            (|| -> anyhow::Result<Box<Profile>> {
                let types: Vec<api::SampleType> = sample_types
                    .into_iter()
                    .map(TryInto::try_into)
                    .collect::<Result<Vec<_>, _>>()?;
                let period_value: api::Period = period.try_into()?;
                let dictionary = dictionary
                    .inner
                    .try_clone()
                    .context("failed to clone ProfileDictionary for Profile")?;
                let inner = internal::Profile::try_new_with_dictionary(
                    &types,
                    Some(period_value),
                    dictionary,
                )
                .context("Profile::create_with_dictionary failed")?;
                Ok(Box::new(Profile::new(inner)))
            })(),
        )
    }

    pub fn add_sample(&mut self, sample: &ffi::Sample) -> bool {
        let api_sample = api::Sample {
            locations: sample.locations.iter().map(Into::into).collect(),
            values: sample.values,
            labels: sample.labels.iter().map(Into::into).collect(),
        };

        // Profile interns the strings
        let result = self
            .inner
            .try_add_sample(api_sample, None)
            .context("Profile::add_sample failed");
        self.handle_result("Profile::add_sample", result)
    }

    pub fn add_sample_with_timestamp(&mut self, sample: &ffi::Sample, endtime_ns: i64) -> bool {
        let result =
            match internal::Timestamp::new(endtime_ns).context("endtime_ns must be non-zero") {
                Ok(timestamp) => {
                    let api_sample = api::Sample {
                        locations: sample.locations.iter().map(Into::into).collect(),
                        values: sample.values,
                        labels: sample.labels.iter().map(Into::into).collect(),
                    };

                    self.inner
                        .try_add_sample(api_sample, Some(timestamp))
                        .context("Profile::add_sample_with_timestamp failed")
                }
                Err(err) => Err(err),
            };
        self.handle_result("Profile::add_sample_with_timestamp", result)
    }

    /// Adds a dictionary-backed sample without an end timestamp.
    pub fn add_dictionary_sample(&mut self, sample: &ffi::DictionarySample) -> bool {
        self.add_dictionary_sample_impl(sample, None)
    }

    /// Adds a dictionary-backed sample with an end timestamp in nanoseconds.
    pub fn add_dictionary_sample_with_timestamp(
        &mut self,
        sample: &ffi::DictionarySample,
        endtime_ns: i64,
    ) -> bool {
        let result =
            match internal::Timestamp::new(endtime_ns).context("endtime_ns must be non-zero") {
                Ok(timestamp) => self.add_dictionary_sample_result(sample, Some(timestamp)),
                Err(err) => Err(err),
            };
        self.handle_result("Profile::add_dictionary_sample", result)
    }

    fn add_dictionary_sample_impl(
        &mut self,
        sample: &ffi::DictionarySample,
        timestamp: Option<internal::Timestamp>,
    ) -> bool {
        let result = self.add_dictionary_sample_result(sample, timestamp);
        self.handle_result("Profile::add_dictionary_sample", result)
    }

    fn add_dictionary_sample_result(
        &mut self,
        sample: &ffi::DictionarySample,
        timestamp: Option<internal::Timestamp>,
    ) -> anyhow::Result<()> {
        let locations_iter = sample.locations.iter().map(|location| {
            // SAFETY: The CXX API contract requires all non-null dictionary ids in
            // sample to come from the same ProfileDictionary used to create
            // this Profile. Null/default ids represent unknown values.
            unsafe { dictionary_location_from_cxx(location) }
        });
        let labels_iter = sample
            .labels
            .iter()
            .map(|label| -> anyhow::Result<api2::Label<'_>> {
                Ok(api2::Label {
                    // SAFETY: The CXX API contract requires all non-null dictionary
                    // ids in sample to come from the same ProfileDictionary
                    // used to create this Profile. Null/default keys represent
                    // the empty string.
                    key: unsafe { dictionary_string_id_from_cxx(&label.key) },
                    str: label.str,
                    num: label.num,
                    num_unit: label.num_unit,
                })
            });

        // SAFETY: The CXX API contract requires all non-null dictionary ids in sample
        // to come from the same ProfileDictionary used to create this Profile.
        // Null/default ids represent empty or unknown values.
        unsafe {
            self.inner
                .try_add_sample2(locations_iter, sample.values, labels_iter, timestamp)
                .context("Profile::add_dictionary_sample failed")
        }
    }

    pub fn set_custom_sample_type(
        &mut self,
        slot: ffi::SampleType,
        type_: &str,
        unit: &str,
    ) -> bool {
        let result = match slot.try_into() {
            Ok(slot) => self
                .inner
                .set_custom_sample_type(slot, api::ValueType::new(type_, unit)),
            Err(err) => Err(err),
        };
        self.handle_result("Profile::set_custom_sample_type", result)
    }

    pub fn add_endpoint(&mut self, local_root_span_id: u64, endpoint: &str) -> bool {
        let result = self
            .inner
            .add_endpoint(local_root_span_id, std::borrow::Cow::Borrowed(endpoint));
        self.handle_result("Profile::add_endpoint", result)
    }

    pub fn add_endpoint_count(&mut self, endpoint: &str, value: i64) -> bool {
        let result = self
            .inner
            .add_endpoint_count(std::borrow::Cow::Borrowed(endpoint), value);
        self.handle_result("Profile::add_endpoint_count", result)
    }

    pub fn add_upscaling_rule_poisson(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value_offset: usize,
        sampling_distance: u64,
    ) -> bool {
        let upscaling_info = api::UpscalingInfo::Poisson {
            sum_value_offset,
            count_value_offset,
            sampling_distance,
        };
        let result =
            self.inner
                .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info);
        self.handle_result("Profile::add_upscaling_rule_poisson", result)
    }

    pub fn add_upscaling_rule_poisson_non_sample_type_count(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value: u64,
        sampling_distance: u64,
    ) -> bool {
        let upscaling_info = api::UpscalingInfo::PoissonNonSampleTypeCount {
            sum_value_offset,
            count_value,
            sampling_distance,
        };
        let result =
            self.inner
                .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info);
        self.handle_result(
            "Profile::add_upscaling_rule_poisson_non_sample_type_count",
            result,
        )
    }

    pub fn add_upscaling_rule_proportional(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        scale: f64,
    ) -> bool {
        let upscaling_info = api::UpscalingInfo::Proportional { scale };
        let result =
            self.inner
                .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info);
        self.handle_result("Profile::add_upscaling_rule_proportional", result)
    }

    pub fn reset(&mut self) -> bool {
        // Reset and discard the old profile
        let result = self.inner.reset_and_return_previous().map(|_| ());
        self.handle_result("Profile::reset", result)
    }

    pub fn serialize(&mut self) -> Box<EncodedProfileResult> {
        let result = (|| -> anyhow::Result<Box<EncodedProfile>> {
            // Reset the profile and get the old one to serialize.
            let old_profile = self.inner.reset_and_return_previous()?;
            let end_time = Some(std::time::SystemTime::now());
            let encoded = old_profile.serialize_into_compressed_pprof(end_time, None)?;
            Ok(Box::new(EncodedProfile { inner: encoded }))
        })();
        if let Err(err) = &result {
            self.handle_error("Profile::serialize", err);
        }
        EncodedProfileResult::from_result("Profile::serialize", result)
    }

    pub fn serialize_to_vec(&mut self, out: &mut Vec<u8>) -> bool {
        match (|| -> anyhow::Result<Vec<u8>> {
            // Reset the profile and get the old one to serialize.
            let old_profile = self.inner.reset_and_return_previous()?;
            let end_time = Some(std::time::SystemTime::now());
            Ok(old_profile
                .serialize_into_compressed_pprof(end_time, None)?
                .buffer)
        })() {
            Ok(bytes) => {
                *out = bytes;
                true
            }
            Err(err) => {
                out.clear();
                self.handle_error("Profile::serialize_to_vec", err)
            }
        }
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
    let mut encoded_result = profile.serialize();
    anyhow::ensure!(encoded_result.ok(), encoded_result.status.message());
    let encoded = encoded_result.take();
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
    ) -> Box<ProfileExporterResult> {
        ProfileExporterResult::from_result(
            "ProfileExporter::create_agent_exporter",
            (|| -> anyhow::Result<Box<ProfileExporter>> {
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
            })(),
        )
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
    ) -> Box<ProfileExporterResult> {
        ProfileExporterResult::from_result(
            "ProfileExporter::create_agentless_exporter",
            (|| -> anyhow::Result<Box<ProfileExporter>> {
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
            })(),
        )
    }

    pub fn create_file_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        output_path: &str,
    ) -> Box<ProfileExporterResult> {
        ProfileExporterResult::from_result(
            "ProfileExporter::create_file_exporter",
            (|| -> anyhow::Result<Box<ProfileExporter>> {
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
            })(),
        )
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
    ) -> ffi::Status {
        ffi::Status::from_result(
            "ProfileExporter::send_profile",
            self.send_profile_impl(
                profile,
                files_to_compress,
                additional_tags,
                process_tags,
                internal_metadata,
                info,
                None,
            ),
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
    ) -> ffi::Status {
        ffi::Status::from_result(
            "ProfileExporter::send_profile_with_cancellation",
            self.send_profile_impl(
                profile,
                files_to_compress,
                additional_tags,
                process_tags,
                internal_metadata,
                info,
                Some(&cancel.inner),
            ),
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
    ) -> ffi::Status {
        ffi::Status::from_result(
            "ProfileExporter::send_encoded_profile",
            self.send_encoded_profile_impl(
                encoded,
                files_to_compress,
                additional_tags,
                process_tags,
                internal_metadata,
                info,
                None,
            ),
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
    ) -> ffi::Status {
        ffi::Status::from_result(
            "ProfileExporter::send_encoded_profile_with_cancellation",
            self.send_encoded_profile_impl(
                encoded,
                files_to_compress,
                additional_tags,
                process_tags,
                internal_metadata,
                info,
                Some(&cancel.inner),
            ),
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
    pub fn new_manager(exporter: Box<ProfileExporter>) -> Box<ExporterManagerResult> {
        ExporterManagerResult::from_result(
            "ExporterManager::new_manager",
            (|| -> anyhow::Result<Box<ExporterManager>> {
                let inner = exporter::ExporterManager::new(exporter.inner)?;
                Ok(Box::new(ExporterManager { inner }))
            })(),
        )
    }

    /// Queue a profile to be sent asynchronously by the background worker thread.
    ///
    /// Resets the profile and queues the previous profile data for sending. This allows
    /// continuous profiling where you keep adding samples to the current profile while the
    /// previous period's data is being sent asynchronously.
    pub fn queue_profile(
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> ffi::Status {
        let result = match prepare_profile_for_export(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        ) {
            Ok((
                encoded,
                files_to_compress_vec,
                additional_tags_vec,
                process_tags_opt,
                internal_metadata_json,
                info_json,
            )) => {
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
            Err(err) => Err(err),
        };
        ffi::Status::from_result("ExporterManager::queue_profile", result)
    }

    #[allow(clippy::boxed_local)]
    pub fn queue_encoded_profile(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> ffi::Status {
        let result = match prepare_export_args(
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        ) {
            Ok((
                files_to_compress_vec,
                additional_tags_vec,
                process_tags_opt,
                internal_metadata_json,
                info_json,
            )) => {
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
            Err(err) => Err(err),
        };
        ffi::Status::from_result("ExporterManager::queue_encoded_profile", result)
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

    pub fn abort(&mut self) -> ffi::Status {
        ffi::Status::from_result("ExporterManager::abort", self.inner.abort())
    }

    pub fn prefork(&mut self) -> ffi::Status {
        ffi::Status::from_result("ExporterManager::prefork", self.inner.prefork())
    }

    pub fn postfork_child(&mut self) -> ffi::Status {
        ffi::Status::from_result(
            "ExporterManager::postfork_child",
            self.inner.postfork_child(),
        )
    }

    pub fn postfork_parent(&mut self) -> ffi::Status {
        ffi::Status::from_result(
            "ExporterManager::postfork_parent",
            self.inner.postfork_parent(),
        )
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

    fn serialize_test_profile_to_vec(profile: &mut Profile) -> Vec<u8> {
        let mut serialized = Vec::new();
        assert!(profile.serialize_to_vec(&mut serialized));
        serialized
    }

    fn create_test_dictionary() -> Box<ProfileDictionary> {
        ProfileDictionary::create().unwrap()
    }

    fn create_test_profile_with_dictionary(dictionary: &ProfileDictionary) -> Box<Profile> {
        let wall_time = ffi::SampleType::WallTime;
        let period = ffi::Period {
            value_type: wall_time,
            value: 60,
        };
        Profile::create_with_dictionary(vec![ffi::SampleType::WallTime], &period, dictionary)
            .unwrap()
    }

    fn intern_test_string(dictionary: &ProfileDictionary, value: &str) -> ffi::DictionaryStringId {
        let mut id = null_dictionary_string_id();
        assert!(dictionary.intern_string(value, &mut id));
        id
    }

    fn intern_test_mapping(
        dictionary: &ProfileDictionary,
        mapping: &ffi::DictionaryMapping,
    ) -> ffi::DictionaryMappingId {
        let mut id = null_dictionary_mapping_id();
        assert!(dictionary.intern_mapping(mapping, &mut id));
        id
    }

    fn intern_test_function(
        dictionary: &ProfileDictionary,
        function: &ffi::DictionaryFunction,
    ) -> ffi::DictionaryFunctionId {
        let mut id = null_dictionary_function_id();
        assert!(dictionary.intern_function(function, &mut id));
        id
    }

    fn create_test_dictionary_sample_parts(
        dictionary: &ProfileDictionary,
    ) -> (
        ffi::DictionaryLocation,
        Vec<i64>,
        Vec<ffi::DictionaryLabel<'static>>,
    ) {
        let filename_mapping = intern_test_string(dictionary, "/usr/lib/libtest.so");
        let filename_function = intern_test_string(dictionary, "/src/test.cpp");
        let build_id = intern_test_string(dictionary, "abc123");
        let name = intern_test_string(dictionary, "test_function");
        let system_name = intern_test_string(dictionary, "_Z13test_functionv");
        let label_key = intern_test_string(dictionary, "pid");
        let mapping = intern_test_mapping(
            dictionary,
            &ffi::DictionaryMapping {
                memory_start: 0x10000000,
                memory_limit: 0x20000000,
                file_offset: 0,
                filename: filename_mapping,
                build_id,
            },
        );
        let function = intern_test_function(
            dictionary,
            &ffi::DictionaryFunction {
                name,
                system_name,
                filename: filename_function,
            },
        );

        (
            ffi::DictionaryLocation {
                mapping,
                function,
                address: 0x10003000,
                line: 100,
            },
            vec![1000000],
            vec![ffi::DictionaryLabel {
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
            locations: Box::leak(vec![create_test_location(0x10003000, 100)].into_boxed_slice()),
            values: Box::leak(vec![1000000].into_boxed_slice()),
            labels: &[],
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
        profile.add_sample(&sample);
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            1,
            "Profile should have 1 sample after adding"
        );

        // Add another sample with different address
        let sample2_locations = vec![create_test_location(0x20003000, 200)];
        let sample2_values = vec![2000000];
        let sample2 = ffi::Sample {
            locations: &sample2_locations,
            values: &sample2_values,
            labels: &[],
        };
        profile.add_sample(&sample2);
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            2,
            "Profile should have 2 samples"
        );

        // Test endpoints
        profile.add_endpoint(12345, "/api/test");
        profile.add_endpoint(67890, "/api/other");
        profile.add_endpoint_count("/api/test", 100);

        // Test upscaling rules (verify they don't error)
        assert!(profile.add_upscaling_rule_poisson(&[0], "thread_id", "0", 0, 0, 1000000));
        assert!(profile.add_upscaling_rule_proportional(&[0], "thread_id", "1", 100.0));
        assert!(profile.add_upscaling_rule_poisson_non_sample_type_count(
            &[0],
            "thread_id",
            "2",
            0,
            50,
            1000000,
        ));

        // Serialize and verify output
        let serialized = serialize_test_profile_to_vec(&mut profile);
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
        profile.add_sample(&sample);
        assert_eq!(profile.inner.only_for_testing_num_aggregated_samples(), 1);
        assert!(profile.reset());
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

        profile.add_sample_with_timestamp(&sample, 42);

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

        let serialized = serialize_test_profile_to_vec(&mut profile);
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
    fn test_status_check_helpers() {
        let status = ffi::Status::err("Test::operation", "boom");
        assert!(!status.ok());
        assert_eq!(status.operation(), "Test::operation");
        assert_eq!(status.message(), "boom");

        let mut errors = Vec::new();
        assert!(!status.check_and_store_first_per_operation(&mut errors));
        assert!(!status.check_and_store_first_per_operation(&mut errors));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].operation, "Test::operation");
        assert_eq!(errors[0].message, "boom");

        assert!(!status.check_and_store_every_occurrence(&mut errors));
        assert_eq!(errors.len(), 2);

        let ok_status = ffi::Status::ok_for("Test::ok");
        assert!(ok_status.check_and_store_every_occurrence(&mut errors));
        assert_eq!(errors.len(), 2);

        let result = ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![],
            "not a url",
            0,
            false,
        );
        let mut result_errors = Vec::new();
        assert!(!result.check_and_store_first_per_operation(&mut result_errors));
        assert_eq!(result_errors.len(), 1);
        assert_eq!(
            result_errors[0].operation,
            "ProfileExporter::create_agent_exporter"
        );
        assert!(!result_errors[0].message.is_empty());
    }

    #[test]
    fn test_profile_timestamped_sample_rejects_zero_timestamp() {
        let mut profile = create_test_profile();
        let sample = create_test_sample();

        profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
        assert!(!profile.add_sample_with_timestamp(&sample, 0));
        let errors = profile.errors();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("endtime_ns must be non-zero"));
    }

    #[test]
    fn test_profile_error_storage_modes() {
        let mut profile = create_test_profile();
        assert!(matches!(
            profile.error_policy(),
            ffi::ErrorPolicy::StoreFirstPerOperation
        ));
        let bad_sample = ffi::Sample {
            locations: &[],
            values: &[],
            labels: &[],
        };

        profile.set_error_policy(ffi::ErrorPolicy::StoreFirstPerOperation);
        assert!(!profile.add_sample(&bad_sample));
        assert!(!profile.add_sample(&bad_sample));
        let errors = profile.errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].operation, "Profile::add_sample");
        assert!(!errors[0].message.is_empty());

        let taken_errors = profile.take_errors();
        assert_eq!(taken_errors.len(), 1);
        assert!(profile.errors().is_empty());

        profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
        assert!(!profile.add_sample(&bad_sample));
        assert!(!profile.add_sample(&bad_sample));
        assert_eq!(profile.take_errors().len(), 2);
        assert!(profile.errors().is_empty());

        assert!(!profile.add_sample(&bad_sample));
        profile.clear_errors();
        assert!(profile.errors().is_empty());
    }

    #[test]
    fn test_profile_timestamped_sample_serializes_end_timestamp_label() {
        let mut profile = create_test_profile();
        let sample = create_test_sample();

        profile.add_sample_with_timestamp(&sample, 42);

        let serialized = serialize_test_profile_to_vec(&mut profile);
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

        profile.add_sample_with_timestamp(&sample, 42);
        profile.add_sample_with_timestamp(&sample, 43);

        let serialized = serialize_test_profile_to_vec(&mut profile);
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
    fn test_profiles_dictionary_error_storage_modes() {
        let dictionary = create_test_dictionary();
        assert!(matches!(
            dictionary.error_policy(),
            ffi::ErrorPolicy::StoreFirstPerOperation
        ));

        dictionary.set_error_policy(ffi::ErrorPolicy::StoreFirstPerOperation);
        dictionary.handle_error("ProfileDictionary::intern_string", "first");
        dictionary.handle_error("ProfileDictionary::intern_string", "second");
        let errors = dictionary.errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].operation, "ProfileDictionary::intern_string");
        assert_eq!(errors[0].message, "first");

        let taken_errors = dictionary.take_errors();
        assert_eq!(taken_errors.len(), 1);
        assert!(dictionary.errors().is_empty());

        dictionary.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
        dictionary.handle_error("ProfileDictionary::intern_string", "first");
        dictionary.handle_error("ProfileDictionary::intern_string", "second");
        assert_eq!(dictionary.take_errors().len(), 2);
        assert!(dictionary.errors().is_empty());

        dictionary.handle_error("ProfileDictionary::intern_string", "third");
        dictionary.clear_errors();
        assert!(dictionary.errors().is_empty());
    }

    #[test]
    fn test_profiles_dictionary_operations() {
        let dictionary = create_test_dictionary();
        let filename = intern_test_string(&dictionary, "example.py");
        let build_id = intern_test_string(&dictionary, "build-id");
        let function_name = intern_test_string(&dictionary, "function");
        let system_name = intern_test_string(&dictionary, "system_function");
        let function_file_name = intern_test_string(&dictionary, "example.py");

        let mapping = intern_test_mapping(
            &dictionary,
            &ffi::DictionaryMapping {
                memory_start: 1,
                memory_limit: 2,
                file_offset: 3,
                filename,
                build_id,
            },
        );
        let function = intern_test_function(
            &dictionary,
            &ffi::DictionaryFunction {
                name: function_name,
                system_name,
                filename: function_file_name,
            },
        );

        assert!(!mapping.is_null());
        assert!(!function.is_null());
    }

    #[test]
    fn test_profiles_dictionary_status_operations() {
        let dictionary = create_test_dictionary();

        assert!(null_dictionary_string_id().is_null());
        assert!(null_dictionary_function_id().is_null());
        assert!(null_dictionary_mapping_id().is_null());

        let mut filename = null_dictionary_string_id();
        assert!(dictionary.intern_string("example.py", &mut filename));
        assert!(!filename.is_null());

        let mut build_id = null_dictionary_string_id();
        assert!(dictionary.intern_string("build-id", &mut build_id));
        assert!(!build_id.is_null());

        let mut mapping = null_dictionary_mapping_id();
        assert!(dictionary.intern_mapping(
            &ffi::DictionaryMapping {
                memory_start: 1,
                memory_limit: 2,
                file_offset: 3,
                filename,
                build_id,
            },
            &mut mapping,
        ));
        assert!(!mapping.is_null());

        let mut function_name = null_dictionary_string_id();
        assert!(dictionary.intern_string("function", &mut function_name));
        let mut function_file_name = null_dictionary_string_id();
        assert!(dictionary.intern_string("example.py", &mut function_file_name));
        let mut function = null_dictionary_function_id();
        assert!(dictionary.intern_function(
            &ffi::DictionaryFunction {
                name: function_name,
                system_name: ffi::DictionaryStringId {
                    handle: std::ptr::null_mut(),
                },
                filename: function_file_name,
            },
            &mut function,
        ));
        assert!(!function.is_null());
    }

    #[test]
    fn test_profile_add_dictionary_sample_serializes() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::DictionarySample {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_dictionary_sample_with_timestamp(&sample, 42);

        let serialized = serialize_test_profile_to_vec(&mut profile);
        assert!(
            serialized.len() > 100,
            "Serialized dictionary-backed profile should be non-trivial"
        );
    }

    #[test]
    fn test_profile_add_dictionary_sample_profile_holds_dictionary_alive() {
        let dictionary = create_test_dictionary();
        let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        drop(dictionary);

        let locations = vec![location];
        let sample = ffi::DictionarySample {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_dictionary_sample_with_timestamp(&sample, 42);

        let serialized = serialize_test_profile_to_vec(&mut profile);
        assert!(serialized.len() > 100);
    }

    #[test]
    fn test_profile_add_dictionary_sample_serializes_dictionary_label_and_timestamp() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::DictionarySample {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_dictionary_sample_with_timestamp(&sample, 42);

        let serialized = serialize_test_profile_to_vec(&mut profile);
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
    fn test_profile_add_dictionary_sample_identical_timestamped_samples_remain_distinct() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::DictionarySample {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_dictionary_sample_with_timestamp(&sample, 42);
        profile.add_dictionary_sample_with_timestamp(&sample, 43);

        let serialized = serialize_test_profile_to_vec(&mut profile);
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
    fn test_profile_add_dictionary_sample_without_timestamp() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::DictionarySample {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.add_dictionary_sample(&sample);
        assert_eq!(profile.inner.only_for_testing_num_aggregated_samples(), 1);
        assert_eq!(profile.inner.only_for_testing_num_timestamped_samples(), 0);

        let serialized = serialize_test_profile_to_vec(&mut profile);
        let pprof = deserialize_compressed_pprof(&serialized).unwrap();
        let has_timestamp_label = pprof
            .samples
            .iter()
            .flat_map(|sample| sample.labels.iter())
            .any(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns");

        assert!(!has_timestamp_label);
    }

    #[test]
    fn test_profile_add_dictionary_sample_rejects_zero_timestamp() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::DictionarySample {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
        assert!(!profile.add_dictionary_sample_with_timestamp(&sample, 0));
        let errors = profile.errors();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("endtime_ns must be non-zero"));
    }

    #[test]
    fn test_profile_serialize_returns_encoded_profile_and_resets() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample());

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
        profile.add_sample(&create_test_sample());
        profile.add_endpoint_count("/api/test", 100);

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

        assert!(!result.ok(), "Should fail when no server is available");
        assert_eq!(result.operation(), "ProfileExporter::send_encoded_profile");
        assert!(!result.message().is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_send_encoded_profile_file_export_preserves_metadata() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample());
        profile.add_endpoint_count("/api/test", 2);
        profile.add_endpoint_count("/api/test", 3);
        profile.add_endpoint_count("/api/other", 7);

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
        profile.add_sample(&create_test_sample());
        let encoded = profile.serialize().unwrap();
        let (mut exporter, file_path) =
            create_test_file_exporter("cxx_send_encoded_profile_cancelled");
        let cancel = CancellationToken::create();
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

        assert!(!result.ok(), "pre-cancelled upload should fail");
        assert_eq!(
            result.operation(),
            "ProfileExporter::send_encoded_profile_with_cancellation"
        );
        assert!(
            !file_path.exists(),
            "pre-cancelled upload should not write a request dump"
        );
    }

    #[test]
    fn test_endpoint_operations() {
        let mut profile = create_test_profile();

        profile.add_endpoint(12345, "/api/test");
        profile.add_endpoint_count("/api/test", 100);
    }

    #[test]
    fn test_profile_add_dictionary_sample_rejects_wrong_value_count() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile_with_dictionary(&dictionary);
        let (location, _values, labels) = create_test_dictionary_sample_parts(&dictionary);
        let locations = vec![location];
        let values = Vec::new();
        let sample = ffi::DictionarySample {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
        assert!(!profile.add_dictionary_sample_with_timestamp(&sample, 42));
        assert!(!profile.errors().is_empty());
    }

    #[test]
    fn test_profile_add_dictionary_sample_requires_dictionary_profile() {
        let dictionary = create_test_dictionary();
        let mut profile = create_test_profile();
        let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
        let locations = vec![location];
        let sample = ffi::DictionarySample {
            locations: &locations,
            values: &values,
            labels: &labels,
        };

        profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
        assert!(!profile.add_dictionary_sample_with_timestamp(&sample, 42));
        assert!(profile.errors()[0]
            .message
            .contains("profiles dictionary not set"));
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
        profile.add_sample(&create_test_sample());

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

        assert!(!result.ok(), "Should fail when no server available");
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should be reset after send attempt"
        );

        // Test with empty optional parameters
        profile.add_sample(&create_test_sample());
        let result2 = exporter.send_profile(&mut profile, vec![], vec![], "", "", "");
        assert!(!result2.ok(), "Should fail with empty optional params too");
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
        let mut manager = ExporterManager::new_manager(exporter).unwrap();

        // Queue a profile
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample());

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
        let mut manager = ExporterManager::new_manager(exporter).unwrap();

        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample());
        profile.add_endpoint_count("/queued", 11);
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

        manager.abort().unwrap();
    }

    #[test]
    fn test_exporter_manager_prefork_and_postfork() {
        let exporter = create_test_exporter();
        let mut manager = ExporterManager::new_manager(exporter).unwrap();

        // Queue some work
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample());
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
        profile.add_sample(&create_test_sample());
        manager
            .queue_profile(&mut profile, vec![], vec![], "", "", "")
            .unwrap();

        // Prefork
        manager.prefork().unwrap();

        // Postfork child - should discard inflight
        manager.postfork_child().unwrap();

        // Child can queue its own work
        let mut child_profile = create_test_profile();
        child_profile.add_sample(&create_test_sample());
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
        profile.add_sample(&create_test_sample());

        let result = manager.queue_profile(&mut profile, vec![], vec![], "", "", "");
        assert!(!result.ok(), "Should fail to queue after abort");
        assert_eq!(result.operation(), "ExporterManager::queue_profile");
        assert!(
            result.message().contains("Suspended") || result.message().contains("state"),
            "Error message should indicate manager is in Suspended state, got: {}",
            result.message()
        );
    }
}
