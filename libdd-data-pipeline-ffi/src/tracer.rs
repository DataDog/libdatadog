// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI functions for creating and manipulating individual tracer spans.
//!
//! Provides an opaque [`TracerSpan`] handle wrapping a `Span<BytesData>`,
//! allowing callers to construct spans field-by-field from C.

use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::{catch_panic, gen_error};
use libdd_common_ffi::slice::AsBytes;
use libdd_common_ffi::CharSlice;
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::v04::SpanBytes;
use std::ptr::NonNull;

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Convert a [`CharSlice`] to a [`BytesString`], copying the bytes.
///
/// Returns an error if the slice is not valid UTF-8.
#[inline]
fn charslice_to_bytesstring(s: CharSlice) -> Result<BytesString, Box<ExporterError>> {
    match BytesString::from_slice(s.as_bytes()) {
        Ok(bs) => Ok(bs),
        Err(_) => Err(Box::new(ExporterError::new(
            ErrorCode::InvalidInput,
            &ErrorCode::InvalidInput.to_string(),
        ))),
    }
}

// ---------------------------------------------------------------------------
// TracerSpan
// ---------------------------------------------------------------------------

/// Opaque handle wrapping a single `Span<BytesData>`.
pub struct TracerSpan(pub(crate) SpanBytes);

/// Create a new span with all scalar fields set.
///
/// String fields are copied from the provided slices.  The `meta` and
/// `metrics` maps start empty; use [`ddog_tracer_span_set_meta`] and
/// [`ddog_tracer_span_set_metric`] to populate them.
///
/// # Arguments
///
/// * `out_handle`  – Receives the new `TracerSpan` handle on success.
/// * `service`, `name`, `resource`, `span_type` – UTF-8 string fields.
/// * `trace_id_low`, `trace_id_high` – 128-bit trace ID split into two 64-bit halves (low = bits
///   0‥63, high = bits 64‥127).
/// * `span_id`   – Span identifier.
/// * `parent_id` – Parent span identifier (0 for root spans).
/// * `start`     – Start time in nanoseconds since Unix epoch.
/// * `duration`  – Duration in nanoseconds.
/// * `error`     – Error status (0 = no error).
///
/// # Safety
///
/// `out_handle` must point to valid, writable memory for a `Box<TracerSpan>`.
/// All `CharSlice` arguments must point to valid memory for their stated
/// length.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_new(
    out_handle: NonNull<Box<TracerSpan>>,
    service: CharSlice,
    name: CharSlice,
    resource: CharSlice,
    span_type: CharSlice,
    trace_id_low: u64,
    trace_id_high: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    duration: i64,
    error: i32,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        {
            let service = match charslice_to_bytesstring(service) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            let name = match charslice_to_bytesstring(name) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            let resource = match charslice_to_bytesstring(resource) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            let span_type = match charslice_to_bytesstring(span_type) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };

            let trace_id: u128 = ((trace_id_high as u128) << 64) | (trace_id_low as u128);

            let span = SpanBytes {
                service,
                name,
                resource,
                r#type: span_type,
                trace_id,
                span_id,
                parent_id,
                start,
                duration,
                error,
                ..Default::default()
            };

            out_handle.as_ptr().write(Box::new(TracerSpan(span)));
            None
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Free a `TracerSpan` and all its contents.
///
/// After this call the handle is invalid and must not be reused.
///
/// # Safety
///
/// `handle` must have been created by [`ddog_tracer_span_new`] and must not
/// be used after this call.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_free(handle: Box<TracerSpan>) {
    drop(handle);
}

/// Add or overwrite a string tag (`meta`) on the span.
///
/// Both `key` and `value` are copied into the span.
///
/// # Safety
///
/// `handle` must be a valid pointer to a `TracerSpan`.
/// `key` and `value` must point to valid UTF-8 memory.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_set_meta(
    handle: Option<&mut TracerSpan>,
    key: CharSlice,
    value: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(span) = handle {
            let key = match charslice_to_bytesstring(key) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            let value = match charslice_to_bytesstring(value) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            span.0.meta.insert(key, value);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a numeric tag (`metric`) on the span.
///
/// The `key` is copied into the span.
///
/// # Safety
///
/// `handle` must be a valid pointer to a `TracerSpan`.
/// `key` must point to valid UTF-8 memory.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_set_metric(
    handle: Option<&mut TracerSpan>,
    key: CharSlice,
    value: f64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(span) = handle {
            let key = match charslice_to_bytesstring(key) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            span.0.metrics.insert(key, value);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ddog_trace_exporter_error_free;
    use std::mem::MaybeUninit;

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from_bytes(s.as_bytes())
    }

    unsafe fn make_minimal_span() -> Box<TracerSpan> {
        let mut handle = MaybeUninit::<Box<TracerSpan>>::uninit();
        let out = NonNull::new(handle.as_mut_ptr()).unwrap();
        let err = ddog_tracer_span_new(
            out,
            cs("svc"),
            cs("op"),
            cs("res"),
            cs(""),
            1,
            0,
            1,
            0,
            0,
            0,
            0,
        );
        assert!(err.is_none());
        handle.assume_init()
    }

    #[test]
    fn new_sets_all_scalar_fields() {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerSpan>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();

            let err = ddog_tracer_span_new(
                out,
                cs("my-service"),
                cs("web.request"),
                cs("GET /users"),
                cs("web"),
                0xdeadbeef,                   // trace_id_low
                0x00000001,                   // trace_id_high
                12345,                        // span_id
                67890,                        // parent_id
                1_700_000_000_000_000_000i64, // start (ns)
                25_000_000,                   // duration (25 ms)
                0,                            // error
            );
            assert!(err.is_none());

            let span = handle.assume_init();
            assert_eq!(span.0.service.as_ref(), "my-service");
            assert_eq!(span.0.name.as_ref(), "web.request");
            assert_eq!(span.0.resource.as_ref(), "GET /users");
            assert_eq!(span.0.r#type.as_ref(), "web");
            assert_eq!(span.0.trace_id, (1u128 << 64) | 0xdeadbeef);
            assert_eq!(span.0.span_id, 12345);
            assert_eq!(span.0.parent_id, 67890);
            assert_eq!(span.0.start, 1_700_000_000_000_000_000);
            assert_eq!(span.0.duration, 25_000_000);
            assert_eq!(span.0.error, 0);
            assert!(span.0.meta.is_empty());
            assert!(span.0.metrics.is_empty());
            assert!(span.0.span_links.is_empty());
            assert!(span.0.span_events.is_empty());

            ddog_tracer_span_free(span);
        }
    }

    #[test]
    fn set_meta_inserts_entries() {
        unsafe {
            let mut span = make_minimal_span();

            let err = ddog_tracer_span_set_meta(Some(&mut *span), cs("http.method"), cs("GET"));
            assert!(err.is_none());

            let err = ddog_tracer_span_set_meta(Some(&mut *span), cs("http.url"), cs("/users"));
            assert!(err.is_none());

            assert_eq!(span.0.meta.len(), 2);
            assert_eq!(span.0.meta.get("http.method").unwrap().as_ref(), "GET");
            assert_eq!(span.0.meta.get("http.url").unwrap().as_ref(), "/users");

            ddog_tracer_span_free(span);
        }
    }

    #[test]
    fn set_meta_overwrites_existing_key() {
        unsafe {
            let mut span = make_minimal_span();

            ddog_tracer_span_set_meta(Some(&mut *span), cs("k"), cs("v1"));
            ddog_tracer_span_set_meta(Some(&mut *span), cs("k"), cs("v2"));

            assert_eq!(span.0.meta.len(), 1);
            assert_eq!(span.0.meta.get("k").unwrap().as_ref(), "v2");

            ddog_tracer_span_free(span);
        }
    }

    #[test]
    fn set_metric_inserts_entries() {
        unsafe {
            let mut span = make_minimal_span();

            let err = ddog_tracer_span_set_metric(Some(&mut *span), cs("_dd.measured"), 1.0);
            assert!(err.is_none());

            let err =
                ddog_tracer_span_set_metric(Some(&mut *span), cs("_sampling_priority_v1"), 2.0);
            assert!(err.is_none());

            assert_eq!(span.0.metrics.len(), 2);
            assert_eq!(*span.0.metrics.get("_dd.measured").unwrap(), 1.0);
            assert_eq!(*span.0.metrics.get("_sampling_priority_v1").unwrap(), 2.0);

            ddog_tracer_span_free(span);
        }
    }

    #[test]
    fn set_meta_null_handle_returns_error() {
        unsafe {
            let err = ddog_tracer_span_set_meta(None, cs("k"), cs("v"));
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
        }
    }

    #[test]
    fn set_metric_null_handle_returns_error() {
        unsafe {
            let err = ddog_tracer_span_set_metric(None, cs("k"), 1.0);
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
        }
    }

    #[test]
    fn new_with_empty_strings_succeeds() {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerSpan>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();

            let err =
                ddog_tracer_span_new(out, cs(""), cs(""), cs(""), cs(""), 0, 0, 0, 0, 0, 0, 0);
            assert!(err.is_none());

            let span = handle.assume_init();
            assert_eq!(span.0.name.as_ref(), "");
            assert_eq!(span.0.service.as_ref(), "");

            ddog_tracer_span_free(span);
        }
    }
}
