// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use crate::crashtracker::datatypes::*;
use anyhow::Context;

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_begin_profiling_op(
    op: ProfilingOpTypes,
) -> CrashtrackerResult {
    datadog_crashtracker::begin_profiling_op(op)
        .context("ddog_prof_crashtracker_begin_profiling_op failed")
        .into()
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_end_profiling_op(
    op: ProfilingOpTypes,
) -> CrashtrackerResult {
    datadog_crashtracker::end_profiling_op(op)
        .context("ddog_prof_crashtracker_end_profiling_op failed")
        .into()
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_shutdown() -> CrashtrackerResult {
    datadog_crashtracker::shutdown_crash_handler()
        .context("ddog_prof_crashtracker_shutdown failed")
        .into()
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_update_on_fork(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> CrashtrackerResult {
    (|| {
        let config = config.try_into()?;
        let metadata = metadata.try_into()?;
        datadog_crashtracker::on_fork(config, metadata)
    })()
    .context("ddog_prof_crashtracker_update_on_fork failed")
    .into()
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_receiver_entry_point() -> CrashtrackerResult {
    datadog_crashtracker::receiver_entry_point()
        .context("ddog_prof_crashtracker_receiver_entry_point failed")
        .into()
}

#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_crashtracker_init(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> CrashtrackerResult {
    (|| {
        let config = config.try_into()?;
        let metadata = metadata.try_into()?;
        datadog_crashtracker::init(config, metadata)
    })()
    .context("ddog_prof_crashtracker_init failed")
    .into()
}
