// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
mod counters;
mod datatypes;
mod spans;

use super::crash_info::Metadata;
use crate::Result;
use anyhow::Context;
pub use counters::*;
pub use datatypes::*;
pub use spans::*;

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
pub unsafe extern "C" fn ddog_crashtracker_shutdown() -> Result {
    datadog_crashtracker::shutdown_crash_handler()
        .context("ddog_crashtracker_shutdown failed")
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
pub unsafe extern "C" fn ddog_crashtracker_update_on_fork(
    config: Configuration,
    receiver_config: ReceiverConfig,
    metadata: Metadata,
) -> Result {
    (|| {
        let config = config.try_into()?;
        let receiver_config = receiver_config.try_into()?;
        let metadata = metadata.try_into()?;
        datadog_crashtracker::on_fork(config, receiver_config, metadata)
    })()
    .context("ddog_crashtracker_update_on_fork failed")
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
pub unsafe extern "C" fn ddog_crashtracker_init_with_receiver(
    config: Configuration,
    receiver_config: ReceiverConfig,
    metadata: Metadata,
) -> Result {
    (|| {
        let config = config.try_into()?;
        let receiver_config = receiver_config.try_into()?;
        let metadata = metadata.try_into()?;
        datadog_crashtracker::init_with_receiver(config, receiver_config, metadata)
    })()
    .context("ddog_crashtracker_init failed")
    .into()
}
