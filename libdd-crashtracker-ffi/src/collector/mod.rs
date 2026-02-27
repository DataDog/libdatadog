// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
mod additional_tags;
mod counters;
mod datatypes;
mod spans;

use super::crash_info::Metadata;
pub use additional_tags::*;
pub use counters::*;
pub use datatypes::*;
use function_name::named;
use libdd_common_ffi::slice::AsBytes;
use libdd_common_ffi::{wrap_with_void_ffi_result, CharSlice, Handle, Slice, ToInner, VoidResult};
use libdd_crashtracker::{CrashtrackerReceiverConfig, StackTrace, DEFAULT_SYMBOLS};
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
    libdd_crashtracker::disable();
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
    libdd_crashtracker::enable();
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
        libdd_crashtracker::on_fork(
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
        libdd_crashtracker::init(
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
        libdd_crashtracker::reconfigure(
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
        libdd_crashtracker::init(
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

#[no_mangle]
#[must_use]
#[named]
/// Report an unhandled exception as a crash event.
///
/// This function sends a crash report for an unhandled exception detected
/// by the runtime. It is intended to be called when the process is in a
/// terminal state due to an unhandled exception.
///
/// # Parameters
/// - `error_type`: Optional type/class of the exception (e.g. "NullPointerException"). Pass empty
///   CharSlice for unknown.
/// - `error_message`: Optional error message. Pass empty CharSlice for no message.
/// - `runtime_stack`: Stack trace from the runtime. Consumed by this call.
///
/// If the crash-tracker has not been initialized, this function is a no-op.
///
/// # Side effects
///   This function disables the signal-based crash handler before performing
///   any work. This means that if the process receives a fatal signal (SIGSEGV)
///   during or after this call, the crashtracker will not produce a
///   second crash report. The previous signal handler (if any) will still be
///   chained.
///
/// # Failure mode
///   If a fatal signal occurs while this function is in progress, the calling
///   process is in an unrecoverable state; the crashtracker cannot report the
///   secondary fault and the caller's own signal handler (if any) will execute
///   in a potentially corrupted context. Callers should treat this function as a
///   terminal operation and exit shortly after it returns.
///
/// # Safety
///   Crash-tracking functions are not reentrant.
///   No other crash-handler functions should be called concurrently.
///   The `runtime_stack` handle must be valid and will be consumed.
pub unsafe extern "C" fn ddog_crasht_report_unhandled_exception(
    error_type: CharSlice,
    error_message: CharSlice,
    mut runtime_stack: *mut Handle<StackTrace>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let error_type_opt = error_type.try_to_string_option()?;
        let error_message_opt = error_message.try_to_string_option()?;
        let stack = *runtime_stack.take()?;

        libdd_crashtracker::report_unhandled_exception(
            error_type_opt.as_deref(),
            error_message_opt.as_deref(),
            stack,
        )?;
    })
}
