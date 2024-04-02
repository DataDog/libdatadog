// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use crate::crashtracker::datatypes::*;
use anyhow::Context;

#[no_mangle]
#[must_use]
/// Cleans up after the crash-tracker:
/// Unregister the crash handler, restore the previous handler (if any), and
/// shut down the receiver.  Note that the use of this function is optional:
/// the receiver will automatically shutdown when the pipe is closed on program
/// exit.
///
/// # Preconditions
///     This function assumes that the crash-tracker has previously been
///     initialized.
/// # Safety
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// # Atomicity
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub unsafe extern "C" fn ddog_prof_Crashtracker_shutdown() -> CrashtrackerResult {
    datadog_crashtracker::shutdown_crash_handler()
        .context("ddog_prof_Crashtracker_shutdown failed")
        .into()
}

#[no_mangle]
#[must_use]
/// Reinitialize the crash-tracking infrastructure after a fork.
/// This should be one of the first things done after a fork, to minimize the
/// chance that a crash occurs between the fork, and this call.
/// In particular, reset the counters that track the profiler state machine,
/// and start a new receiver to collect data from this fork.
/// NOTE: An alternative design would be to have a 1:many sidecar listening on a
/// socket instead of 1:1 receiver listening on a pipe, but the only real
/// advantage would be to have fewer processes in `ps -a`.
///
/// # Preconditions
///     This function assumes that the crash-tracker has previously been
///     initialized.
/// # Safety
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// # Atomicity
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub unsafe extern "C" fn ddog_prof_Crashtracker_update_on_fork(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> CrashtrackerResult {
    (|| {
        let config = config.try_into()?;
        let metadata = metadata.try_into()?;
        datadog_crashtracker::on_fork(config, metadata)
    })()
    .context("ddog_prof_Crashtracker_update_on_fork failed")
    .into()
}

#[no_mangle]
#[must_use]
/// Receives data from a crash collector via a pipe on `stdin`, formats it into
/// `CrashInfo` json, and emits it to the endpoint/file defined in `config`.
///
/// At a high-level, this exists because doing anything in a
/// signal handler is dangerous, so we fork a sidecar to do the stuff we aren't
/// allowed to do in the handler.
///
/// See comments in [profiling/crashtracker/mod.rs] for a full architecture
/// description.
/// # Safety
/// No safety concerns
pub unsafe extern "C" fn ddog_prof_Crashtracker_receiver_entry_point() -> CrashtrackerResult {
    datadog_crashtracker::receiver_entry_point()
        .context("ddog_prof_Crashtracker_receiver_entry_point failed")
        .into()
}

#[no_mangle]
#[must_use]
/// Initialize the crash-tracking infrastructure.
///
/// # Preconditions
///     None.
/// # Safety
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// # Atomicity
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub unsafe extern "C" fn ddog_prof_Crashtracker_init(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> CrashtrackerResult {
    (|| {
        let config = config.try_into()?;
        let metadata = metadata.try_into()?;
        datadog_crashtracker::init(config, metadata)
    })()
    .context("ddog_prof_Crashtracker_init failed")
    .into()
}
