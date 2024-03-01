// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![cfg(unix)]

use crate::crashtracker::datatypes::*;
use ddcommon_ffi::Error;

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_begin_profiling_op(
    op: ProfilingOpTypes,
) -> CrashtrackerResult {
    match datadog_crashtracker::begin_profiling_op(op) {
        Ok(_) => CrashtrackerResult::Ok(true),
        Err(err) => CrashtrackerResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_begin_profiling_op failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_end_profiling_op(
    op: ProfilingOpTypes,
) -> CrashtrackerResult {
    match datadog_crashtracker::end_profiling_op(op) {
        Ok(_) => CrashtrackerResult::Ok(true),
        Err(err) => CrashtrackerResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_end_profiling_op failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_shutdown() -> CrashtrackerResult {
    match datadog_crashtracker::shutdown_crash_handler() {
        Ok(_) => CrashtrackerResult::Ok(true),
        Err(err) => CrashtrackerResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_shutdown failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_update_on_fork(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> CrashtrackerResult {
    match ddog_prof_crashtracker_update_on_fork_impl(config, metadata) {
        Ok(_) => CrashtrackerResult::Ok(true),
        Err(err) => CrashtrackerResult::Err(Error::from(
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
pub unsafe extern "C" fn ddog_prof_crashtracker_receiver_entry_point() -> CrashtrackerResult {
    match datadog_crashtracker::receiver_entry_point() {
        Ok(_) => CrashtrackerResult::Ok(true),
        Err(err) => CrashtrackerResult::Err(Error::from(
            err.context("ddog_prof_crashtracker_receiver_entry_point failed"),
        )),
    }
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_init(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> CrashtrackerResult {
    match ddog_prof_crashtracker_init_impl(config, metadata) {
        Ok(_) => CrashtrackerResult::Ok(true),
        Err(err) => CrashtrackerResult::Err(Error::from(
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
