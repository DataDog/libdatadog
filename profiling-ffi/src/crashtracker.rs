// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![cfg(unix)]

use crate::exporter::{self, Endpoint};
use crate::profiles::ProfileResult;
use ddcommon::tag::Tag;
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::Error;
use std::ops::Not;

pub use datadog_crashtracker::{CrashtrackerResolveFrames, ProfilingOpTypes};

#[repr(C)]
pub struct CrashtrackerConfiguration<'a> {
    /// Should the crashtracker attempt to collect a stacktrace for the crash
    pub collect_stacktrace: bool,
    pub create_alt_stack: bool,
    /// The endpoint to send the crash repor to (can be a file://)
    pub endpoint: Endpoint<'a>,
    /// Optional filename to forward stderr to (useful for logging/debugging)
    pub optional_stderr_filename: CharSlice<'a>,
    /// Optional filename to forward stdout to (useful for logging/debugging)
    pub optional_stdout_filename: CharSlice<'a>,
    pub path_to_receiver_binary: CharSlice<'a>,
    /// Whether/when we should attempt to resolve frames
    pub resolve_frames: CrashtrackerResolveFrames,
}

impl<'a> TryFrom<CrashtrackerConfiguration<'a>>
    for datadog_crashtracker::CrashtrackerConfiguration
{
    type Error = anyhow::Error;
    fn try_from(value: CrashtrackerConfiguration<'a>) -> anyhow::Result<Self> {
        fn option_from_char_slice(s: CharSlice) -> anyhow::Result<Option<String>> {
            let s = s.try_to_utf8()?.to_string();
            Ok(s.is_empty().not().then_some(s))
        }
        let collect_stacktrace = value.collect_stacktrace;
        let create_alt_stack = value.create_alt_stack;
        let endpoint = unsafe { Some(exporter::try_to_endpoint(value.endpoint)?) };
        let path_to_receiver_binary = value.path_to_receiver_binary.try_to_utf8()?.to_string();
        let resolve_frames = value.resolve_frames;
        let stderr_filename = option_from_char_slice(value.optional_stderr_filename)?;
        let stdout_filename = option_from_char_slice(value.optional_stdout_filename)?;

        Self::new(
            collect_stacktrace,
            create_alt_stack,
            endpoint,
            path_to_receiver_binary,
            resolve_frames,
            stderr_filename,
            stdout_filename,
        )
    }
}

#[repr(C)]
pub struct CrashtrackerMetadata<'a> {
    pub profiling_library_name: CharSlice<'a>,
    pub profiling_library_version: CharSlice<'a>,
    pub family: CharSlice<'a>,
    /// Should include "service", "environment", etc
    pub tags: Option<&'a ddcommon_ffi::Vec<Tag>>,
}

impl<'a> TryFrom<CrashtrackerMetadata<'a>> for datadog_crashtracker::CrashtrackerMetadata {
    type Error = anyhow::Error;
    fn try_from(value: CrashtrackerMetadata<'a>) -> anyhow::Result<Self> {
        let profiling_library_name = value.profiling_library_name.try_to_utf8()?.to_string();
        let profiling_library_version = value.profiling_library_version.try_to_utf8()?.to_string();
        let family = value.family.try_to_utf8()?.to_string();
        let tags = value
            .tags
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default();
        Ok(Self::new(
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
        ))
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_begin_profiling_op(
    op: ProfilingOpTypes,
) -> ProfileResult {
    match datadog_crashtracker::begin_profiling_op(op) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_begin_profiling_op failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_end_profiling_op(
    op: ProfilingOpTypes,
) -> ProfileResult {
    match datadog_crashtracker::end_profiling_op(op) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_end_profiling_op failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_shutdown() -> ProfileResult {
    match datadog_crashtracker::shutdown_crash_handler() {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_shutdown failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_update_on_fork(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> ProfileResult {
    match ddog_prof_crashtracker_update_on_fork_impl(config, metadata) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_update_on_fork failed"),
        )),
    }
}

unsafe fn ddog_prof_crashtracker_update_on_fork_impl(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    let config = config.try_into()?;
    let metadata = metadata.try_into()?;
    datadog_crashtracker::on_fork(config, metadata)
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_receiver_entry_point() -> ProfileResult {
    match datadog_crashtracker::receiver_entry_point() {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_receiver_entry_point failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_init(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> ProfileResult {
    match ddog_prof_crashtracker_init_impl(config, metadata) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

unsafe fn ddog_prof_crashtracker_init_impl(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    let config = config.try_into()?;
    let metadata = metadata.try_into()?;
    datadog_crashtracker::init(config, metadata)
}
