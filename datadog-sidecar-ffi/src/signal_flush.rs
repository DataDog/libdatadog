// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_sidecar::service::{
    blocking::SidecarTransport, signal_flush::SignalFlush, SidecarFlushOptions,
};
use libdd_common_ffi::{Error, MaybeError};

/// Prepare a flush on this transport with a private completion pipe.
/// Normal thread context only. The returned object owns a duplicate of the transport fd;
/// refresh it after reconnect and drop it before normal connection shutdown.
///
/// # Safety
/// `transport` must be exclusively borrowed and `output` must be writable for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_sidecar_prepare_signal_flush(
    transport: &mut SidecarTransport,
    options: SidecarFlushOptions,
    output: &mut *mut SignalFlush,
) -> MaybeError {
    *output = std::ptr::null_mut();
    match SignalFlush::prepare(transport, options) {
        Ok(flush) => {
            *output = Box::into_raw(Box::new(flush));
            MaybeError::None
        }
        Err(error) => MaybeError::Some(Error::from(error.to_string())),
    }
}

/// Destroy a prepared flush in ordinary thread context.
///
/// # Safety
/// `flush` must be null or an owned pointer returned by prepare. Any raw worker must have exited.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_sidecar_signal_flush_drop(flush: *mut SignalFlush) {
    if !flush.is_null() {
        drop(Box::from_raw(flush));
    }
}

/// Run one bounded flush without TLS access, allocation, unwinding, or process termination.
/// Returns zero when the sidecar closes the pipe (completion or exit), or a negative Linux errno.
///
/// # Safety
/// The object must remain alive through the call, with exclusive one-shot use of this object.
/// The normal transport may continue sending and receiving concurrently.
/// All worker signals must be blocked. Do not use an inherited object after fork.
#[unsafe(no_mangle)]
#[inline(never)]
pub unsafe extern "C-unwind" fn ddog_sidecar_signal_flush_run(flush: &SignalFlush) -> i32 {
    flush.run()
}
