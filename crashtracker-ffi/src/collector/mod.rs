// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
mod counters;
mod datatypes;
mod spans;

use super::crash_info::Metadata;
use crate::Result;
use anyhow::Context;
pub use counters::*;
use datadog_crashtracker::CrashtrackerReceiverConfig;
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
///   This function assumes that the crashtracker has previously been
///   initialized.
/// # Safety
///   Crash-tracking functions are not reentrant.
///   No other crash-handler functions should be called concurrently.
/// # Atomicity
///   This function is not atomic. A crash during its execution may lead to
///   unexpected crash-handling behaviour.
pub unsafe extern "C" fn ddog_crasht_shutdown() -> Result {
    datadog_crashtracker::shutdown_crash_handler()
        .context("ddog_crasht_shutdown failed")
        .into()
}

#[no_mangle]
#[must_use]
/// Reinitialize the crash-tracking infrastructure after a fork.
/// This should be one of the first things done after a fork, to minimize the
/// chance that a crash occurs between the fork, and this call.
/// In particular, reset the counters that track the profiler state machine.
/// NOTE: An alternative design would be to have a 1:many sidecar listening on a
/// socket instead of 1:1 receiver listening on a pipe, but the only real
/// advantage would be to have fewer processes in `ps -a`.
///
/// # Preconditions
///   This function assumes that the crash-tracker has previously been
///   initialized.
/// # Safety
///   Crash-tracking functions are not reentrant.
///   No other crash-handler functions should be called concurrently.
/// # Atomicity
///   This function is not atomic. A crash during its execution may lead to
///   unexpected crash-handling behaviour.
pub unsafe extern "C" fn ddog_crasht_update_on_fork(
    config: Config,
    receiver_config: ReceiverConfig,
    metadata: Metadata,
) -> Result {
    (|| {
        let config = config.try_into()?;
        let receiver_config = receiver_config.try_into()?;
        let metadata = metadata.try_into()?;
        datadog_crashtracker::on_fork(config, receiver_config, metadata)
    })()
    .context("ddog_crasht_update_on_fork failed")
    .into()
}

#[no_mangle]
#[must_use]
/// Initialize the crash-tracking infrastructure.
///
/// # Preconditions
///   None.
/// # Safety
///   Crash-tracking functions are not reentrant.
///   No other crash-handler functions should be called concurrently.
/// # Atomicity
///   This function is not atomic. A crash during its execution may lead to
///   unexpected crash-handling behaviour.
pub unsafe extern "C" fn ddog_crasht_init(
    config: Config,
    receiver_config: ReceiverConfig,
    metadata: Metadata,
) -> Result {
    (|| {
        let config = config.try_into()?;
        let receiver_config = receiver_config.try_into()?;
        let metadata = metadata.try_into()?;
        datadog_crashtracker::init(config, receiver_config, metadata)
    })()
    .context("ddog_crasht_init failed")
    .into()
}

#[no_mangle]
#[must_use]
/// Initialize the crash-tracking infrastructure without launching the receiver.
///
/// # Preconditions
///   Requires `config` to be given with a `unix_socket_path`, which is normally optional.
/// # Safety
///   Crash-tracking functions are not reentrant.
///   No other crash-handler functions should be called concurrently.
/// # Atomicity
///   This function is not atomic. A crash during its execution may lead to
///   unexpected crash-handling behaviour.
pub unsafe extern "C" fn ddog_crasht_init_without_receiver(
    config: Config,
    metadata: Metadata,
) -> Result {
    (|| {
        let config: datadog_crashtracker::CrashtrackerConfiguration = config.try_into()?;
        let metadata = metadata.try_into()?;

        // If the unix domain socket path is not set, then we throw an error--there's currently no
        // other way to specify communication between an async receiver and a collector, so this
        // isn't a valid configuration.
        if config.unix_socket_path.is_none() {
            return Err(anyhow::anyhow!("config.unix_socket_path must be set"));
        }
        if config.unix_socket_path.as_ref().unwrap().is_empty() {
            return Err(anyhow::anyhow!("config.unix_socket_path can't be empty"));
        }

        // Populate an empty receiver config
        let receiver_config = CrashtrackerReceiverConfig {
            args: vec![],
            env: vec![],
            path_to_receiver_binary: "".to_string(),
            stderr_filename: None,
            stdout_filename: None,
        };
        datadog_crashtracker::init(config, receiver_config, metadata)
    })()
    .context("ddog_crasht_init failed")
    .into()
}
