// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Mirror of the `#[repr(C)]` types and the exported symbols of the profiling FFI. Its modules
//! are private, so callers declare the C ABI they use, like a native caller. Shared by the
//! harness and the `profile_sequence` fuzz target.

#![allow(improper_ctypes, dead_code)]

use libdd_common::tag::Tag;
use libdd_common_ffi::{CharSlice, Error, Handle, Result, Slice, Timespec, VoidResult};
pub use libdd_profiling::api::{ManagedStringId, Period, SampleType};
pub use libdd_profiling::exporter::ProfileExporter;
pub use libdd_profiling::internal::EncodedProfile;
use std::num::NonZeroI64;

#[repr(C)]
pub struct Profile {
    pub inner: *mut std::ffi::c_void,
}

#[repr(C)]
pub enum ProfileNewResult {
    Ok(Profile),
    Err(Error),
}

#[repr(C)]
pub enum ProfileResult {
    Ok(bool),
    Err(Error),
}

#[repr(C)]
pub enum SerializeResult {
    Ok(Handle<EncodedProfile>),
    Err(Error),
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Mapping<'a> {
    pub memory_start: u64,
    pub memory_limit: u64,
    pub file_offset: u64,
    pub filename: CharSlice<'a>,
    pub filename_id: ManagedStringId,
    pub build_id: CharSlice<'a>,
    pub build_id_id: ManagedStringId,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Function<'a> {
    pub name: CharSlice<'a>,
    pub name_id: ManagedStringId,
    pub system_name: CharSlice<'a>,
    pub system_name_id: ManagedStringId,
    pub filename: CharSlice<'a>,
    pub filename_id: ManagedStringId,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Location<'a> {
    pub mapping: Mapping<'a>,
    pub function: Function<'a>,
    pub address: u64,
    pub line: i64,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct Label<'a> {
    pub key: CharSlice<'a>,
    pub key_id: ManagedStringId,
    pub str: CharSlice<'a>,
    pub str_id: ManagedStringId,
    pub num: i64,
    pub num_unit: CharSlice<'a>,
    pub num_unit_id: ManagedStringId,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Sample<'a> {
    pub locations: Slice<'a, Location<'a>>,
    pub values: Slice<'a, i64>,
    pub labels: Slice<'a, Label<'a>>,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum ProfilingEndpoint<'a> {
    Agent(CharSlice<'a>, u64, bool),
    Agentless(CharSlice<'a>, CharSlice<'a>, u64, bool),
    File(CharSlice<'a>),
}

#[repr(C)]
pub struct File<'a> {
    pub name: CharSlice<'a>,
    pub file: Slice<'a, u8>,
}

#[repr(C)]
pub struct HttpStatus(pub u16);

/// Stand-in for `tokio_util::sync::CancellationToken`. `Handle<T>` is one pointer.
pub enum CancellationToken {}

unsafe extern "C" {
    pub fn ddog_prof_Profile_new(
        sample_types: Slice<SampleType>,
        period: Option<&Period>,
    ) -> ProfileNewResult;
    pub fn ddog_prof_Profile_drop(profile: *mut Profile);
    pub fn ddog_prof_Profile_add(
        profile: *mut Profile,
        sample: Sample,
        timestamp: Option<NonZeroI64>,
    ) -> ProfileResult;
    pub fn ddog_prof_Profile_set_endpoint(
        profile: *mut Profile,
        local_root_span_id: u64,
        endpoint: CharSlice,
    ) -> ProfileResult;
    pub fn ddog_prof_Profile_add_endpoint_count(
        profile: *mut Profile,
        endpoint: CharSlice,
        value: i64,
    ) -> ProfileResult;
    pub fn ddog_prof_Profile_add_upscaling_rule_proportional(
        profile: *mut Profile,
        offset_values: Slice<usize>,
        label_name: CharSlice,
        label_value: CharSlice,
        total_sampled: u64,
        total_real: u64,
    ) -> ProfileResult;
    pub fn ddog_prof_Profile_add_upscaling_rule_poisson(
        profile: *mut Profile,
        offset_values: Slice<usize>,
        label_name: CharSlice,
        label_value: CharSlice,
        sum_value_offset: usize,
        count_value_offset: usize,
        sampling_distance: u64,
    ) -> ProfileResult;
    pub fn ddog_prof_Profile_reset(profile: *mut Profile) -> ProfileResult;
    pub fn ddog_prof_Profile_serialize(
        profile: *mut Profile,
        start_time: Option<&Timespec>,
        end_time: Option<&Timespec>,
    ) -> SerializeResult;
    pub fn ddog_prof_EncodedProfile_drop(profile: *mut Handle<EncodedProfile>);
    pub fn ddog_prof_EncodedProfile_bytes(
        encoded_profile: *mut Handle<EncodedProfile>,
    ) -> libdd_common_ffi::Result<Slice<'static, u8>>;
    pub fn ddog_prof_Endpoint_agent(
        base_url: CharSlice,
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> ProfilingEndpoint<'static>;
    pub fn ddog_prof_Endpoint_agentless(
        site: CharSlice,
        api_key: CharSlice,
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> ProfilingEndpoint<'static>;
    pub fn ddog_CancellationToken_new() -> Handle<CancellationToken>;
    pub fn ddog_CancellationToken_drop(token: *mut Handle<CancellationToken>);
    pub fn ddog_prof_Exporter_new(
        profiling_library_name: CharSlice,
        profiling_library_version: CharSlice,
        family: CharSlice,
        tags: Option<&libdd_common_ffi::Vec<Tag>>,
        endpoint: ProfilingEndpoint,
    ) -> Result<Handle<ProfileExporter>>;
    pub fn ddog_prof_Exporter_init_runtime(exporter: *mut Handle<ProfileExporter>) -> VoidResult;
    pub fn ddog_prof_Exporter_send_blocking(
        exporter: *mut Handle<ProfileExporter>,
        profile: *mut Handle<EncodedProfile>,
        files_to_compress_and_export: Slice<File>,
        optional_additional_tags: Option<&libdd_common_ffi::Vec<Tag>>,
        optional_process_tags: Option<&CharSlice>,
        optional_internal_metadata_json: Option<&CharSlice>,
        optional_info_json: Option<&CharSlice>,
        cancel: *mut std::ffi::c_void,
    ) -> Result<HttpStatus>;
    pub fn ddog_prof_Exporter_drop(exporter: *mut Handle<ProfileExporter>);
}
