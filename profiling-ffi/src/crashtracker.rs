// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::exporter::{self, Endpoint};
use crate::profiles::ProfileResult;
use datadog_profiling::crashtracker;
use ddcommon::tag::Tag;
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::Error;
use std::ops::Not;

pub use datadog_profiling::crashtracker::ProfilingOpTypes;

#[repr(C)]
pub struct Configuration<'a> {
    pub create_alt_stack: bool,
    pub endpoint: Endpoint<'a>,
    pub output_filename: CharSlice<'a>,
    pub path_to_receiver_binary: CharSlice<'a>,
    pub resolve_frames_in_process: bool,
    pub resolve_frames_in_receiver: bool,
    pub stderr_filename: CharSlice<'a>,
    pub stdout_filename: CharSlice<'a>,
}

impl<'a> TryFrom<Configuration<'a>> for crashtracker::Configuration {
    type Error = anyhow::Error;
    fn try_from(value: Configuration<'a>) -> anyhow::Result<Self> {
        fn option_from_char_slice(s: CharSlice) -> anyhow::Result<Option<String>> {
            let s = unsafe { s.try_to_utf8()?.to_string() };
            Ok(s.is_empty().not().then_some(s))
        }

        let create_alt_stack = value.create_alt_stack;
        let endpoint = unsafe { Some(exporter::try_to_endpoint(value.endpoint)?) };
        let output_filename = option_from_char_slice(value.output_filename)?;
        let path_to_receiver_binary =
            unsafe { value.path_to_receiver_binary.try_to_utf8()?.to_string() };
        let resolve_frames_in_process = value.resolve_frames_in_process;
        let resolve_frames_in_receiver = value.resolve_frames_in_receiver;
        let stderr_filename = option_from_char_slice(value.stderr_filename)?;
        let stdout_filename = option_from_char_slice(value.stdout_filename)?;

        crashtracker::Configuration::new(
            create_alt_stack,
            endpoint,
            output_filename,
            path_to_receiver_binary,
            resolve_frames_in_process,
            resolve_frames_in_receiver,
            stderr_filename,
            stdout_filename,
        )
    }
}

#[repr(C)]
pub struct Metadata<'a> {
    pub profiling_library_name: CharSlice<'a>,
    pub profiling_library_version: CharSlice<'a>,
    pub family: CharSlice<'a>,
    pub tags: Option<&'a ddcommon_ffi::Vec<Tag>>,
}

impl<'a> TryFrom<Metadata<'a>> for crashtracker::Metadata {
    type Error = anyhow::Error;
    fn try_from(value: Metadata<'a>) -> anyhow::Result<Self> {
        let profiling_library_name =
            unsafe { value.profiling_library_name.try_to_utf8()?.to_string() };
        let profiling_library_version =
            unsafe { value.profiling_library_version.try_to_utf8()?.to_string() };
        let family = unsafe { value.family.try_to_utf8()?.to_string() };
        let tags = value.tags.map(|tags| tags.iter().cloned().collect());
        Ok(crashtracker::Metadata::new(
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
    match crashtracker::begin_profiling_op(op) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_end_profiling_op(
    op: ProfilingOpTypes,
) -> ProfileResult {
    match crashtracker::end_profiling_op(op) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_shutdown() -> ProfileResult {
    match crashtracker::shutdown_crash_handler() {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_update_on_fork(
    config: Configuration,
    metadata: Metadata,
) -> ProfileResult {
    match ddog_prof_crashtracker_update_on_fork_impl(config, metadata) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

unsafe fn ddog_prof_crashtracker_update_on_fork_impl(
    config: Configuration,
    metadata: Metadata,
) -> anyhow::Result<()> {
    let config = config.try_into()?;
    let metadata = metadata.try_into()?;
    crashtracker::on_fork(config, metadata)
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_receiver_entry_point() -> ProfileResult {
    match crashtracker::receiver_entry_point() {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_receiver_entry_point failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_init(
    config: Configuration,
    metadata: Metadata,
) -> ProfileResult {
    match ddog_prof_crashtracker_init_impl(config, metadata) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

unsafe fn ddog_prof_crashtracker_init_impl(
    config: Configuration,
    metadata: Metadata,
) -> anyhow::Result<()> {
    let config = config.try_into()?;
    let metadata = metadata.try_into()?;
    crashtracker::init(config, metadata)
}
