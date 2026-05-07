// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_agent_client::TelemetryRequest`].
//!
//! Telemetry requests are owned by C as opaque
//! `Box<TelemetryRequest>` pointers, configured at construction time,
//! and consumed by
//! [`crate::ddog_agent_client_send_telemetry_blocking`].

use crate::error::DdogAgentClientError;
use libdd_agent_client::TelemetryRequest;
use libdd_common_ffi::slice::{AsBytes, ByteSlice};
use libdd_common_ffi::CharSlice;
use std::ptr::NonNull;

/// Allocate a new [`TelemetryRequest`].
///
/// `request_type` and `api_version` must be valid UTF-8.
/// `body` is an arbitrary byte payload (the agent expects
/// `application/json`; the caller is responsible for serialising the
/// telemetry event before constructing this).
///
/// # Safety
/// All slice arguments must point to valid memory for their declared
/// lengths. `out_handle` must be a valid, writable pointer to an
/// uninitialised `*mut ddog_TelemetryRequest`.
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_request_new(
    request_type: CharSlice,
    api_version: CharSlice,
    body: ByteSlice,
    debug: bool,
    out_handle: NonNull<Box<TelemetryRequest>>,
) -> Option<Box<DdogAgentClientError>> {
    let request_type = match request_type.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "request_type is not valid UTF-8: {e}"
            ))))
        }
    };
    let api_version = match api_version.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "api_version is not valid UTF-8: {e}"
            ))))
        }
    };
    let body_bytes = bytes::Bytes::copy_from_slice(body.as_bytes());
    let req = TelemetryRequest {
        request_type,
        api_version,
        debug,
        body: body_bytes,
    };
    out_handle.as_ptr().write(Box::new(req));
    None
}

/// Drop a [`TelemetryRequest`] that was not consumed by
/// [`crate::ddog_agent_client_send_telemetry_blocking`].
///
/// # Safety
/// `request` must be `None` or a request produced by
/// [`ddog_telemetry_request_new`] and not yet consumed.
#[no_mangle]
pub unsafe extern "C" fn ddog_telemetry_request_drop(
    request: Option<Box<TelemetryRequest>>,
) {
    drop(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from(s)
    }

    #[test]
    fn new_round_trip() {
        unsafe {
            let mut handle: MaybeUninit<Box<TelemetryRequest>> = MaybeUninit::uninit();
            let body = b"{\"event\":\"app-started\"}";
            let err = ddog_telemetry_request_new(
                cs("app-started"),
                cs("v2"),
                ByteSlice::from(body.as_slice()),
                true,
                NonNull::new_unchecked(handle.as_mut_ptr()),
            );
            assert!(err.is_none());
            let req = handle.assume_init();
            assert_eq!(req.request_type, "app-started");
            assert_eq!(req.api_version, "v2");
            assert!(req.debug);
            assert_eq!(req.body.as_ref(), body.as_slice());
            ddog_telemetry_request_drop(Some(req));
        }
    }

    #[test]
    fn drop_handles_none() {
        unsafe { ddog_telemetry_request_drop(None) };
    }
}
