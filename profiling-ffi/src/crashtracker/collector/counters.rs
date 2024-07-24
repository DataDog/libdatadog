// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::datatypes::ProfilingOpTypes;
use crate::crashtracker::datatypes::*;
use anyhow::Context;

/// Resets all counters to 0.
/// Expected to be used after a fork, to reset the counters on the child
/// ATOMICITY:
///     This is NOT ATOMIC.
///     Should only be used when no conflicting updates can occur,
///     e.g. after a fork but before profiling ops start on the child.
/// # Safety
/// No safety concerns.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_Crashtracker_reset_counters() -> CrashtrackerResult {
    datadog_crashtracker::reset_counters()
        .context("ddog_prof_Crashtracker_begin_profiling_op failed")
        .into()
}

#[no_mangle]
#[must_use]
/// Atomically increments the count associated with `op`.
/// Useful for tracking what operations were occuring when a crash occurred.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_prof_Crashtracker_begin_profiling_op(
    op: ProfilingOpTypes,
) -> CrashtrackerResult {
    datadog_crashtracker::begin_profiling_op(op)
        .context("ddog_prof_Crashtracker_begin_profiling_op failed")
        .into()
}

#[no_mangle]
#[must_use]
/// Atomically decrements the count associated with `op`.
/// Useful for tracking what operations were occuring when a crash occurred.
///
/// # Safety
/// No safety concerns.
pub unsafe extern "C" fn ddog_prof_Crashtracker_end_profiling_op(
    op: ProfilingOpTypes,
) -> CrashtrackerResult {
    datadog_crashtracker::end_profiling_op(op)
        .context("ddog_prof_Crashtracker_end_profiling_op failed")
        .into()
}
