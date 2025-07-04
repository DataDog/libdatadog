// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
mod additional_tags;
mod counters;
mod datatypes;
mod spans;

use super::crash_info::Metadata;
pub use additional_tags::*;
pub use counters::*;
use datadog_crashtracker::{CrashtrackerReceiverConfig, DEFAULT_SYMBOLS};
pub use datatypes::*;
use ddcommon_ffi::{wrap_with_void_ffi_result, Slice, VoidResult};
use function_name::named;
pub use spans::*;

#[no_mangle]
#[must_use]
/// Disables the crashtracker.
/// Note that this does not restore the old signal handlers, but rather turns crash-tracking into a
/// no-op, and then chains the old handlers.  This means that handlers registered after the
/// crashtracker will continue to work as expected.
///
/// # Preconditions
///   None
/// # Safety
///   None
/// # Atomicity
///   This function is atomic and idempotent.  Calling it multiple times is allowed.
pub unsafe extern "C" fn ddog_crasht_disable() -> VoidResult {
    datadog_crashtracker::disable();
    VoidResult::Ok
}

#[no_mangle]
#[must_use]
/// Enables the crashtracker, if it had been previously disabled.
/// If crashtracking has not been initialized, this function will have no effect.
///
/// # Preconditions
///   None
/// # Safety
///   None
/// # Atomicity
///   This function is atomic and idempotent.  Calling it multiple times is allowed.
pub unsafe extern "C" fn ddog_crasht_enable() -> VoidResult {
    datadog_crashtracker::enable();
    VoidResult::Ok
}

#[no_mangle]
#[must_use]
#[named]
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
) -> VoidResult {
    wrap_with_void_ffi_result!({
        datadog_crashtracker::on_fork(
            config.try_into()?,
            receiver_config.try_into()?,
            metadata.try_into()?,
        )?;
    })
}

#[no_mangle]
#[must_use]
#[named]
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
) -> VoidResult {
    wrap_with_void_ffi_result!({
        datadog_crashtracker::init(
            config.try_into()?,
            receiver_config.try_into()?,
            metadata.try_into()?,
        )?;
    })
}

#[no_mangle]
#[must_use]
#[named]
/// Reconfigure the crashtracker and re-enables it.
/// If the crashtracker has not been initialized, this function will have no effect.
///
/// # Preconditions
///   None.
/// # Safety
///   Crash-tracking functions are not reentrant.
///   No other crash-handler functions should be called concurrently.
/// # Atomicity
///   This function is not atomic. A crash during its execution may lead to
///   unexpected crash-handling behaviour.
pub unsafe extern "C" fn ddog_crasht_reconfigure(
    config: Config,
    receiver_config: ReceiverConfig,
    metadata: Metadata,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        datadog_crashtracker::reconfigure(
            config.try_into()?,
            receiver_config.try_into()?,
            metadata.try_into()?,
        )?;
    })
}

#[no_mangle]
#[must_use]
#[named]
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
) -> VoidResult {
    wrap_with_void_ffi_result!({
        // If the unix domain socket path is not set, then we throw an error--there's currently no
        // other way to specify communication between an async receiver and a collector, so this
        // isn't a valid configuration.
        anyhow::ensure!(
            !config.optional_unix_socket_filename.is_empty(),
            "config.optional_unix_socket_filename must be set in this configuration"
        );

        // No receiver, use an empty receiver config
        datadog_crashtracker::init(
            config.try_into()?,
            CrashtrackerReceiverConfig::default(),
            metadata.try_into()?,
        )?
    })
}

#[no_mangle]
/// Returns a list of signals suitable for use in a crashtracker config.
pub extern "C" fn ddog_crasht_default_signals() -> Slice<'static, libc::c_int> {
    Slice::new(&DEFAULT_SYMBOLS)
}
