// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::exporter::{self, Endpoint};
use crate::profiles::ProfileResult;
use datadog_profiling::crashtracker;
use datadog_profiling::exporter::config;
use ddcommon::tag::Tag;
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::Error;
use libc::c_char;
use std::borrow::Cow;
use std::ffi::CStr;

pub use datadog_profiling::crashtracker::ProfilingOpTypes;

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
    profiling_library_name: CharSlice,
    profiling_library_version: CharSlice,
    family: CharSlice,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoint: Option<Endpoint>,
    output_filename: Option<String>,
    path_to_reciever_binary: CharSlice,
) -> ProfileResult {
    match ddog_prof_crashtracker_update_on_fork_impl(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
        output_filename,
        path_to_reciever_binary,
    ) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

unsafe fn ddog_prof_crashtracker_update_on_fork_impl(
    profiling_library_name: CharSlice,
    profiling_library_version: CharSlice,
    family: CharSlice,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoint: Option<Endpoint>,
    output_filename: Option<String>,
    path_to_reciever_binary: CharSlice,
) -> anyhow::Result<()> {
    let (config, metadata) = process_args(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
        output_filename,
        path_to_reciever_binary,
    )?;
    crashtracker::on_fork(config, metadata)
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_init_full(
    profiling_library_name: CharSlice,
    profiling_library_version: CharSlice,
    family: CharSlice,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoint: Option<Endpoint>,
    output_filename: Option<String>,
    path_to_reciever_binary: CharSlice,
) -> ProfileResult {
    match ddog_prof_crashtracker_init_full_impl(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
        output_filename,
        path_to_reciever_binary,
    ) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

unsafe fn ddog_prof_crashtracker_init_full_impl(
    profiling_library_name: CharSlice,
    profiling_library_version: CharSlice,
    family: CharSlice,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoint: Option<Endpoint>,
    output_filename: Option<String>,
    path_to_reciever_binary: CharSlice,
) -> anyhow::Result<()> {
    let (config, metadata) = process_args(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
        output_filename,
        path_to_reciever_binary,
    )?;
    crashtracker::init(config, metadata)
}

unsafe fn process_args(
    profiling_library_name: CharSlice,
    profiling_library_version: CharSlice,
    family: CharSlice,
    tags: Option<&ddcommon_ffi::Vec<Tag>>,
    endpoint: Option<Endpoint>,
    output_filename: Option<String>,
    path_to_reciever_binary: CharSlice,
) -> anyhow::Result<(crashtracker::Configuration, crashtracker::Metadata)> {
    let profiling_library_name = profiling_library_name.to_utf8_lossy().into_owned();
    let profiling_library_version = profiling_library_version.to_utf8_lossy().into_owned();
    let family = family.to_utf8_lossy().into_owned();
    let path_to_reciever_binary = path_to_reciever_binary.to_utf8_lossy().into_owned();
    let tags = tags.map(|tags| tags.iter().cloned().collect());
    let endpoint = if let Some(e) = endpoint {
        Some(exporter::try_to_endpoint(e)?)
    } else {
        None
    };
    let config =
        crashtracker::Configuration::new(endpoint, output_filename, path_to_reciever_binary);
    let metadata = crashtracker::Metadata::new(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
    );
    Ok((config, metadata))
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_init(
    path_to_reciever_binary: *const c_char,
    //TODO: key/value pairs to pass to the receiver
) -> ProfileResult {
    match crashtracker_init_impl(path_to_reciever_binary) {
        Ok(_) => ProfileResult::Ok(true),
        Err(err) => ProfileResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_init failed"),
        )),
    }
}

//TODO, for now just hardcoding these values
fn crashtracker_init_impl(path_to_reciever_binary: *const c_char) -> anyhow::Result<()> {
    let path_to_reciever_binary = unsafe { CStr::from_ptr(path_to_reciever_binary) };
    let path_to_reciever_binary = path_to_reciever_binary.to_str()?.to_string();
    let profiling_library_name = "profiling_library_name".to_string();
    let profiling_library_version = "profiling_library_version".to_string();
    let family = "family".to_string();
    let tags = None;

    let api_key = Cow::from(std::env::var("DD_API_KEY")?);
    let endpoint = config::agentless("datad0g.com", api_key)?;

    let config = crashtracker::Configuration::new(Some(endpoint), None, path_to_reciever_binary);
    let metadata = crashtracker::Metadata::new(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
    );
    crashtracker::init(config, metadata)
}
