// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Blocking send entry points for [`libdd_agent_client::AgentClient`].
//!
//! Mirrors the precedent in
//! `libdd-data-pipeline-ffi/src/trace_exporter.rs:457` for
//! `Arc<SharedRuntime>` handling: the caller passes a non-null
//! `*const SharedRuntime` (obtained from `ddog_shared_runtime_new` in
//! `libdd-shared-runtime-ffi`); we increment the strong count and
//! reconstruct an independent `Arc` via `Arc::from_raw`, then drive
//! each blocking send through `SharedRuntime::block_on`.

use crate::error::DdogAgentClientError;
use libdd_agent_client::{
    AgentClient, AgentInfo, AgentResponse, TelemetryRequest, TraceFormat, TraceSendOptions,
};
use libdd_common_ffi::slice::{AsBytes, ByteSlice};
use libdd_common_ffi::CharSlice;
use libdd_shared_runtime::SharedRuntime;
use std::ptr::NonNull;
use std::sync::Arc;

/// Wire format for a serialised trace payload.
///
/// Mirrors [`libdd_agent_client::TraceFormat`]. Determines both the
/// `Content-Type` header and the target endpoint:
/// `MsgpackV5` -> `/v0.5/traces`, `MsgpackV4` -> `/v0.4/traces`.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DdogTraceFormat {
    /// `application/msgpack` to `/v0.5/traces`. Preferred.
    MsgpackV5,
    /// `application/msgpack` to `/v0.4/traces`. Fallback for Windows /
    /// AppSec.
    MsgpackV4,
}

impl From<DdogTraceFormat> for TraceFormat {
    fn from(value: DdogTraceFormat) -> Self {
        match value {
            DdogTraceFormat::MsgpackV5 => TraceFormat::MsgpackV5,
            DdogTraceFormat::MsgpackV4 => TraceFormat::MsgpackV4,
        }
    }
}

/// Per-request options for trace sends.
///
/// FFI mirror of [`libdd_agent_client::TraceSendOptions`].
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct DdogTraceSendOptions {
    /// When `true`, appends `Datadog-Client-Computed-Top-Level: yes`.
    pub computed_top_level: bool,
}

impl From<DdogTraceSendOptions> for TraceSendOptions {
    fn from(value: DdogTraceSendOptions) -> Self {
        TraceSendOptions {
            computed_top_level: value.computed_top_level,
        }
    }
}

/// Bump the runtime strong count and reconstruct an independent
/// `Arc<SharedRuntime>` from a raw pointer. Mirrors the pattern in
/// `libdd-data-pipeline-ffi/src/trace_exporter.rs:457` so the caller's
/// handle remains valid and is still freed by them.
///
/// # Safety
/// `handle` must be a non-null pointer produced by
/// `ddog_shared_runtime_new` whose underlying `Arc` is still alive.
unsafe fn clone_runtime(handle: NonNull<SharedRuntime>) -> Arc<SharedRuntime> {
    Arc::increment_strong_count(handle.as_ptr());
    Arc::from_raw(handle.as_ptr())
}

/// Send a serialised trace payload synchronously.
///
/// On success writes a `Box<AgentResponse>` into `*out_response` and
/// returns `None`. On failure returns the error and leaves
/// `*out_response` unchanged.
///
/// # Parameters
/// - `client`: a client produced by
///   [`crate::ddog_agent_client_builder_build`].
/// - `payload`: a borrowed byte buffer with the serialised traces. The
///   bytes are copied into an owned `Bytes` for the duration of the
///   request — the caller may free `payload` after the call returns.
/// - `trace_count`: number of traces in the payload (sets
///   `X-Datadog-Trace-Count`).
/// - `format`: msgpack v5 or v4.
/// - `options`: per-request options (e.g. computed-top-level).
/// - `shared_runtime`: a runtime handle from `ddog_shared_runtime_new`.
///   This call does not take ownership of the handle.
/// - `out_response`: where to write the resulting
///   `Box<AgentResponse>`.
///
/// # Safety
/// - `client` must be valid (non-null) and not dropped.
/// - `payload` must point to valid memory for its declared length.
/// - `shared_runtime` must be non-null and produced by
///   `ddog_shared_runtime_new`.
/// - `out_response` must be a valid, writable pointer to an
///   uninitialised `*mut ddog_AgentResponse`.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_send_traces_blocking(
    client: Option<&AgentClient>,
    payload: ByteSlice,
    trace_count: usize,
    format: DdogTraceFormat,
    options: DdogTraceSendOptions,
    shared_runtime: Option<NonNull<SharedRuntime>>,
    out_response: NonNull<Box<AgentResponse>>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(client) = client else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "client is null",
        )));
    };
    let Some(handle) = shared_runtime else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "shared_runtime is null",
        )));
    };
    let bytes = bytes::Bytes::copy_from_slice(payload.as_bytes());
    let runtime = clone_runtime(handle);
    let result = client.send_traces_blocking(
        bytes,
        trace_count,
        format.into(),
        options.into(),
        runtime.as_ref(),
    );
    drop(runtime);
    match result {
        Ok(resp) => {
            out_response.as_ptr().write(Box::new(resp));
            None
        }
        Err(err) => Some(Box::new(DdogAgentClientError::from(err))),
    }
}

/// Send span stats (APM concentrator buckets) synchronously to
/// `/v0.6/stats`.
///
/// # Safety
/// See [`ddog_agent_client_send_traces_blocking`] — same contract.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_send_stats_blocking(
    client: Option<&AgentClient>,
    payload: ByteSlice,
    shared_runtime: Option<NonNull<SharedRuntime>>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(client) = client else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "client is null",
        )));
    };
    let Some(handle) = shared_runtime else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "shared_runtime is null",
        )));
    };
    let bytes = bytes::Bytes::copy_from_slice(payload.as_bytes());
    let runtime = clone_runtime(handle);
    let result = client.send_stats_blocking(bytes, runtime.as_ref());
    drop(runtime);
    match result {
        Ok(()) => None,
        Err(err) => Some(Box::new(DdogAgentClientError::from(err))),
    }
}

/// Send data-streams pipeline stats synchronously to
/// `/v0.1/pipeline_stats`. The payload is gzip-compressed by the
/// underlying client regardless of any client-level compression
/// setting.
///
/// # Safety
/// See [`ddog_agent_client_send_traces_blocking`] — same contract.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_send_pipeline_stats_blocking(
    client: Option<&AgentClient>,
    payload: ByteSlice,
    shared_runtime: Option<NonNull<SharedRuntime>>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(client) = client else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "client is null",
        )));
    };
    let Some(handle) = shared_runtime else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "shared_runtime is null",
        )));
    };
    let bytes = bytes::Bytes::copy_from_slice(payload.as_bytes());
    let runtime = clone_runtime(handle);
    let result = client.send_pipeline_stats_blocking(bytes, runtime.as_ref());
    drop(runtime);
    match result {
        Ok(()) => None,
        Err(err) => Some(Box::new(DdogAgentClientError::from(err))),
    }
}

/// Send a telemetry event synchronously to the agent's telemetry
/// proxy (`telemetry/proxy/api/v2/apmtelemetry`). Consumes the
/// telemetry request handle.
///
/// # Safety
/// `client` and `shared_runtime` follow the standard contract;
/// `request` must be `None` or a request produced by
/// [`crate::ddog_telemetry_request_new`] and not yet consumed.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_send_telemetry_blocking(
    client: Option<&AgentClient>,
    request: Option<Box<TelemetryRequest>>,
    shared_runtime: Option<NonNull<SharedRuntime>>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(client) = client else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "client is null",
        )));
    };
    let Some(request) = request else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "request is null",
        )));
    };
    let Some(handle) = shared_runtime else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "shared_runtime is null",
        )));
    };
    let runtime = clone_runtime(handle);
    let result = client.send_telemetry_blocking(*request, runtime.as_ref());
    drop(runtime);
    match result {
        Ok(()) => None,
        Err(err) => Some(Box::new(DdogAgentClientError::from(err))),
    }
}

/// Send an event via the agent's EVP (Event Platform) proxy.
///
/// `subdomain` is the target intake (injected as
/// `X-Datadog-EVP-Subdomain`) and `path` is the endpoint on that
/// intake. Both must be valid UTF-8.
///
/// # Safety
/// `subdomain`, `path`, `content_type` must point to valid UTF-8
/// memory; `payload` must point to valid memory for its declared
/// length. Other arguments follow the standard contract.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_send_evp_event_blocking(
    client: Option<&AgentClient>,
    subdomain: CharSlice,
    path: CharSlice,
    payload: ByteSlice,
    content_type: CharSlice,
    shared_runtime: Option<NonNull<SharedRuntime>>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(client) = client else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "client is null",
        )));
    };
    let Some(handle) = shared_runtime else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "shared_runtime is null",
        )));
    };
    let subdomain = match subdomain.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "subdomain is not valid UTF-8: {e}"
            ))))
        }
    };
    let path = match path.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "path is not valid UTF-8: {e}"
            ))))
        }
    };
    let content_type = match content_type.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "content_type is not valid UTF-8: {e}"
            ))))
        }
    };
    let bytes = bytes::Bytes::copy_from_slice(payload.as_bytes());
    let runtime = clone_runtime(handle);
    let result = client.send_evp_event_blocking(
        &subdomain,
        &path,
        bytes,
        &content_type,
        runtime.as_ref(),
    );
    drop(runtime);
    match result {
        Ok(()) => None,
        Err(err) => Some(Box::new(DdogAgentClientError::from(err))),
    }
}

/// Probe `GET /info` synchronously and surface the parsed agent
/// capabilities.
///
/// On success writes either a `Box<AgentInfo>` into `*out_info`, or
/// `null` when the agent returned 404 (the `Ok(None)` path on the
/// Rust side — meaning the agent does not expose `/info`). Returns
/// `None` for both successful cases. On failure leaves `*out_info`
/// unchanged and returns the error.
///
/// # Safety
/// Standard contract for `client` and `shared_runtime`. `out_info`
/// must be a valid, writable pointer to a `*mut ddog_AgentInfo` —
/// after a successful call, dereference to test for null before
/// dropping.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_client_agent_info_blocking(
    client: Option<&AgentClient>,
    shared_runtime: Option<NonNull<SharedRuntime>>,
    out_info: Option<&mut *mut AgentInfo>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(client) = client else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "client is null",
        )));
    };
    let Some(handle) = shared_runtime else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "shared_runtime is null",
        )));
    };
    let Some(out_info) = out_info else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "out_info is null",
        )));
    };
    let runtime = clone_runtime(handle);
    let result = client.agent_info_blocking(runtime.as_ref());
    drop(runtime);
    match result {
        Ok(Some(info)) => {
            *out_info = Box::into_raw(Box::new(info));
            None
        }
        Ok(None) => {
            // 404 from the agent — successful call, but no info.
            *out_info = std::ptr::null_mut();
            None
        }
        Err(err) => Some(Box::new(DdogAgentClientError::from(err))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_agent_client::{AgentClient, LanguageMetadata};
    use std::mem::MaybeUninit;
    use std::sync::Once;

    fn ensure_crypto_provider() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    fn test_client() -> AgentClient {
        ensure_crypto_provider();
        AgentClient::builder()
            .http("localhost", 0)
            .language_metadata(LanguageMetadata::new("ruby", "3.2", "MRI", "1.0"))
            .build()
            .unwrap()
    }

    #[test]
    fn null_client_returns_invalid_argument() {
        unsafe {
            let runtime = SharedRuntime::new().unwrap();
            let arc = Arc::new(runtime);
            let raw = Arc::into_raw(arc);

            let mut out: MaybeUninit<Box<AgentResponse>> = MaybeUninit::uninit();
            let err = ddog_agent_client_send_traces_blocking(
                None,
                ByteSlice::from(b"".as_slice()),
                0,
                DdogTraceFormat::MsgpackV5,
                DdogTraceSendOptions::default(),
                NonNull::new(raw as *mut SharedRuntime),
                NonNull::new_unchecked(out.as_mut_ptr()),
            );
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogAgentClientErrorCode::InvalidArgument
            );

            drop(Arc::from_raw(raw));
        }
    }

    #[test]
    fn null_runtime_returns_invalid_argument() {
        let client = test_client();
        unsafe {
            let mut out: MaybeUninit<Box<AgentResponse>> = MaybeUninit::uninit();
            let err = ddog_agent_client_send_traces_blocking(
                Some(&client),
                ByteSlice::from(b"".as_slice()),
                0,
                DdogTraceFormat::MsgpackV5,
                DdogTraceSendOptions::default(),
                None,
                NonNull::new_unchecked(out.as_mut_ptr()),
            );
            assert!(err.is_some());
        }
    }

    #[test]
    fn null_telemetry_request_returns_invalid_argument() {
        let client = test_client();
        unsafe {
            let runtime = SharedRuntime::new().unwrap();
            let arc = Arc::new(runtime);
            let raw = Arc::into_raw(arc);

            let err = ddog_agent_client_send_telemetry_blocking(
                Some(&client),
                None,
                NonNull::new(raw as *mut SharedRuntime),
            );
            assert!(err.is_some());

            drop(Arc::from_raw(raw));
        }
    }

    #[test]
    fn agent_info_null_out() {
        let client = test_client();
        unsafe {
            let runtime = SharedRuntime::new().unwrap();
            let arc = Arc::new(runtime);
            let raw = Arc::into_raw(arc);

            let err = ddog_agent_client_agent_info_blocking(
                Some(&client),
                NonNull::new(raw as *mut SharedRuntime),
                None,
            );
            assert!(err.is_some());

            drop(Arc::from_raw(raw));
        }
    }

    // End-to-end coverage lives in `examples/ffi/agent_client.c`,
    // exercised via `cargo ffi-test`. Unit tests here cover the FFI
    // boundary's null/argument validation paths only.
}
