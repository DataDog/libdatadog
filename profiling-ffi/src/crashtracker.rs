// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::profiles::ProfileResult;
use datadog_profiling::crashtracker;
use ddcommon_ffi::Error;
use libc::c_char;
use std::ffi::CStr;

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

fn crashtracker_init_impl(path_to_reciever_binary: *const c_char) -> anyhow::Result<()> {
    let path_to_reciever_binary = unsafe { CStr::from_ptr(path_to_reciever_binary) };
    let path_to_reciever_binary = path_to_reciever_binary.to_str()?;
    crashtracker::init(path_to_reciever_binary)
}
