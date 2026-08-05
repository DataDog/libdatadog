// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI functions for creating and manipulating tracer spans and trace chunks.
//!
//! Provides opaque handles for building trace data from C:
//!
//! - [`TracerSpan`] wraps a single `Span<BytesData>`, constructed field-by-field.
//! - [`TracerTraceChunks`] wraps `Vec<Vec<SpanBytes>>`, grouping spans into trace chunks ready for
//!   export.

use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::response::ExporterResponse;
use crate::trace_exporter::TraceExporter;
use crate::{catch_panic, gen_error};
use libdd_common_ffi::slice::{AsBytes, ByteSlice, Slice};
use libdd_common_ffi::CharSlice;
use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_utils::span::v04::{
    AttributeAnyValueBytes, AttributeArrayValueBytes, SpanBytes, SpanEventBytes, SpanLinkBytes,
};
use std::collections::HashMap;
use std::ptr::NonNull;

type TokioCancellationToken = tokio_util::sync::CancellationToken;

#[inline]
fn slice_to_rust_slice<'a, T>(s: Slice<'a, T>) -> Result<&'a [T], Option<Box<ExporterError>>> {
    s.try_as_slice()
        .map_err(|_| gen_error!(ErrorCode::InvalidArgument))
}

#[inline]
fn charslice_to_bytesstring(s: CharSlice) -> Result<BytesString, Option<Box<ExporterError>>> {
    let bytes = s
        .try_as_bytes()
        .map_err(|_| gen_error!(ErrorCode::InvalidArgument))?;
    BytesString::from_slice(bytes).map_err(|_| gen_error!(ErrorCode::InvalidInput))
}

// ---------------------------------------------------------------------------
// TracerSpan
// ---------------------------------------------------------------------------

/// Opaque handle wrapping a single `Span<BytesData>`.
pub struct TracerSpan(SpanBytes);

/// Opaque, owned span event under construction.
pub struct TracerSpanEvent(SpanEventBytes);

/// FFI-safe bundle of scalar fields for creating a [`TracerSpan`].
///
/// Passed by reference to [`ddog_tracer_span_new`] so that adding or
/// changing fields does not break the function signature.
#[derive(Debug)]
#[repr(C)]
pub struct TracerSpanFields<'a> {
    pub service: CharSlice<'a>,
    pub name: CharSlice<'a>,
    pub resource: CharSlice<'a>,
    pub span_type: CharSlice<'a>,
    pub trace_id_low: u64,
    pub trace_id_high: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub error: i32,
}

/// A string attribute belonging to a [`TracerSpanLink`].
#[derive(Debug)]
#[repr(C)]
pub struct TracerSpanLinkAttribute<'a> {
    pub key: CharSlice<'a>,
    pub value: CharSlice<'a>,
}

/// FFI-safe representation of one complete span link.
#[derive(Debug)]
#[repr(C)]
pub struct TracerSpanLink<'a> {
    pub trace_id_low: u64,
    pub trace_id_high: u64,
    pub span_id: u64,
    pub attributes: Slice<'a, TracerSpanLinkAttribute<'a>>,
    pub tracestate: CharSlice<'a>,
    pub flags: u32,
}

/// Create a new span with all scalar fields set.
///
/// String fields are copied from the provided slices. The `meta`, `metrics` and `meta_struct`
/// maps start empty; use [`ddog_tracer_span_set_meta`], [`ddog_tracer_span_set_metric`] and
/// [`ddog_tracer_span_set_meta_struct_blob`] to populate them.
///
/// Returns an error if `fields` is null, if any string field is not valid UTF-8, or if any of
/// its slices is malformed.
///
/// # Safety
///
/// `out_handle` must point to valid, writable memory for a `Box<TracerSpan>`.
/// All `CharSlice` fields in `fields` must point to valid memory for their
/// stated length.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_new(
    out_handle: NonNull<Box<TracerSpan>>,
    fields: Option<&TracerSpanFields>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(fields) = fields {
            let inner = || -> Result<(), Option<Box<ExporterError>>> {
                let service = charslice_to_bytesstring(fields.service)?;
                let name = charslice_to_bytesstring(fields.name)?;
                let resource = charslice_to_bytesstring(fields.resource)?;
                let span_type = charslice_to_bytesstring(fields.span_type)?;

                let trace_id: u128 =
                    ((fields.trace_id_high as u128) << 64) | (fields.trace_id_low as u128);

                let span = SpanBytes {
                    service,
                    name,
                    resource,
                    r#type: span_type,
                    trace_id,
                    span_id: fields.span_id,
                    parent_id: fields.parent_id,
                    start: fields.start,
                    duration: fields.duration,
                    error: fields.error,
                    ..Default::default()
                };

                out_handle.as_ptr().write(Box::new(TracerSpan(span)));
                Ok(())
            };
            inner().err().flatten()
        } else {
            gen_error!(ErrorCode::InvalidArgument)
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
pub unsafe extern "C" fn ddog_tracer_span_free(handle: Option<Box<TracerSpan>>) {
    drop(handle);
}

/// Add or overwrite a string tag (`meta`) on the span.
///
/// Both `key` and `value` are copied into the span.
///
/// Returns an error if `handle` is null, if `key` or `value` is not valid UTF-8, or if either
/// slice is malformed.
///
/// # Safety
///
/// `handle` must be a valid pointer to a `TracerSpan`. `key` and `value` must point to valid
/// memory for their stated lengths.
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
                Err(e) => return e,
            };
            let value = match charslice_to_bytesstring(value) {
                Ok(s) => s,
                Err(e) => return e,
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
/// Returns an error if `handle` is null, if `key` is not valid UTF-8, or if the slice is
/// malformed.
///
/// # Safety
///
/// `handle` must be a valid pointer to a `TracerSpan`. `key` must point to valid memory for its
/// stated length.
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
                Err(e) => return e,
            };
            span.0.metrics.insert(key, value);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a structured metadata entry (`meta_struct`) on the span.
///
/// The `key` and opaque binary `value` are copied into the span. The value is
/// not interpreted or validated as MessagePack.
///
/// Returns an error if `handle` is null, if `key` is not valid UTF-8, or if either slice is
/// malformed.
///
/// # Safety
///
/// `handle` must be a valid pointer to a `TracerSpan`. `key` and `value` must point to valid
/// memory for their stated lengths.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_set_meta_struct_blob(
    handle: Option<&mut TracerSpan>,
    key: CharSlice,
    value: ByteSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(span) = handle {
            let key = match charslice_to_bytesstring(key) {
                Ok(s) => s,
                Err(e) => return e,
            };
            let value = match value.try_as_bytes() {
                Ok(v) => v,
                Err(_) => return gen_error!(ErrorCode::InvalidArgument),
            };
            span.0
                .meta_struct
                .insert(key, Bytes::copy_from_slice(value));
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Replace all span links in one atomic operation.
///
/// The links, attributes, and strings are copied before this function returns.
/// If any slice or string is invalid, the span's existing links are unchanged.
/// Link order is preserved.
///
/// # Safety
///
/// `handle` must be a valid pointer to a `TracerSpan`. All slices must point to
/// valid memory for their stated lengths.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_set_links(
    handle: Option<&mut TracerSpan>,
    links: Slice<TracerSpanLink>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(span) = handle {
            let links = match links.try_as_slice() {
                Ok(links) => links,
                Err(_) => return gen_error!(ErrorCode::InvalidArgument),
            };
            let mut converted = Vec::with_capacity(links.len());

            for link in links {
                let attributes = match link.attributes.try_as_slice() {
                    Ok(attributes) => attributes,
                    Err(_) => return gen_error!(ErrorCode::InvalidArgument),
                };
                let mut converted_attributes = HashMap::with_capacity(attributes.len());
                for attribute in attributes {
                    let key = match charslice_to_bytesstring(attribute.key) {
                        Ok(key) => key,
                        Err(err) => return err,
                    };
                    let value = match charslice_to_bytesstring(attribute.value) {
                        Ok(value) => value,
                        Err(err) => return err,
                    };
                    converted_attributes.insert(key, value);
                }

                let tracestate = match charslice_to_bytesstring(link.tracestate) {
                    Ok(tracestate) => tracestate,
                    Err(err) => return err,
                };
                converted.push(SpanLinkBytes {
                    trace_id: link.trace_id_low,
                    trace_id_high: link.trace_id_high,
                    span_id: link.span_id,
                    attributes: converted_attributes,
                    tracestate,
                    flags: link.flags,
                });
            }

            span.0.span_links = converted;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

// ---------------------------------------------------------------------------
// TracerSpanEvent
// ---------------------------------------------------------------------------

/// Create an owned span event.
///
/// The name is copied before this function returns. The event remains detached
/// until consumed by [`ddog_tracer_span_add_event`].
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_new(
    out_handle: NonNull<Box<TracerSpanEvent>>,
    name: CharSlice,
    time_unix_nano: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        match charslice_to_bytesstring(name) {
            Ok(name) => {
                out_handle
                    .as_ptr()
                    .write(Box::new(TracerSpanEvent(SpanEventBytes {
                        time_unix_nano,
                        name,
                        ..Default::default()
                    })));
                None
            }
            Err(e) => e,
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Free a detached span event.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_free(handle: Option<Box<TracerSpanEvent>>) {
    drop(handle);
}

#[inline]
fn set_event_attribute(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    value: AttributeAnyValueBytes,
) -> Option<Box<ExporterError>> {
    let Some(event) = event else {
        return gen_error!(ErrorCode::InvalidArgument);
    };
    let key = match charslice_to_bytesstring(key) {
        Ok(key) => key,
        Err(e) => return e,
    };
    event.0.attributes.insert(key, value);
    None
}

fn set_event_array_attribute<T: Copy>(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    values: Slice<T>,
    to_value: fn(T) -> AttributeArrayValueBytes,
) -> Option<Box<ExporterError>> {
    slice_to_rust_slice(values)
        .map(|values| {
            set_event_attribute(
                event,
                key,
                AttributeAnyValueBytes::Array(values.iter().copied().map(to_value).collect()),
            )
        })
        .unwrap_or_else(|e| e)
}

/// Add or overwrite a string event attribute. The key and value are copied.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_set_string(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    value: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        match charslice_to_bytesstring(value) {
            Ok(value) => set_event_attribute(
                event,
                key,
                AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::String(value)),
            ),
            Err(e) => e,
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a boolean event attribute.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_set_bool(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    value: bool,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        set_event_attribute(
            event,
            key,
            AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Boolean(value)),
        ),
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a signed integer event attribute.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_set_int(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    value: i64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        set_event_attribute(
            event,
            key,
            AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Integer(value)),
        ),
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a double event attribute.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_set_double(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    value: f64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        set_event_attribute(
            event,
            key,
            AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Double(value)),
        ),
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a homogeneous string array event attribute.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_set_string_array(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    values: Slice<CharSlice>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        slice_to_rust_slice(values)
            .and_then(|values| {
                values
                    .iter()
                    .map(|value| {
                        charslice_to_bytesstring(*value).map(AttributeArrayValueBytes::String)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .map(|values| set_event_attribute(event, key, AttributeAnyValueBytes::Array(values)))
            .unwrap_or_else(|e| e),
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a homogeneous boolean array event attribute.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_set_bool_array(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    values: Slice<bool>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        set_event_array_attribute(event, key, values, AttributeArrayValueBytes::Boolean),
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a homogeneous signed integer array event attribute.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_set_int_array(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    values: Slice<i64>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        set_event_array_attribute(event, key, values, AttributeArrayValueBytes::Integer),
        gen_error!(ErrorCode::Panic)
    )
}

/// Add or overwrite a homogeneous double array event attribute.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_event_set_double_array(
    event: Option<&mut TracerSpanEvent>,
    key: CharSlice,
    values: Slice<f64>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        set_event_array_attribute(event, key, values, AttributeArrayValueBytes::Double),
        gen_error!(ErrorCode::Panic)
    )
}

/// Atomically attach a completed event to a span, consuming the event.
///
/// A non-null `event` is always consumed — moved into the span on success, or
/// dropped on any error (a null `span`, or an internal panic). Callers must not
/// use or free the event after this call. On error the span is unchanged.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_span_add_event(
    span: Option<&mut TracerSpan>,
    event: Option<Box<TracerSpanEvent>>,
) -> Option<Box<ExporterError>> {
    let Some(span) = span else {
        return gen_error!(ErrorCode::InvalidArgument);
    };
    let Some(event) = event else {
        return gen_error!(ErrorCode::InvalidArgument);
    };
    catch_panic!(
        {
            span.0.span_events.push(event.0);
            None
        },
        gen_error!(ErrorCode::Panic)
    )
}

// ---------------------------------------------------------------------------
// TracerTraceChunks
// ---------------------------------------------------------------------------

/// Opaque handle wrapping `Vec<Vec<SpanBytes>>` — a list of trace chunks,
/// each containing a list of spans.
pub struct TracerTraceChunks(Vec<Vec<SpanBytes>>);

/// Create a new empty trace chunks container.
///
/// `capacity` is a hint for the expected number of chunks; pass 0 if
/// unknown.
///
/// # Safety
///
/// `out_handle` must point to valid, writable memory for a
/// `Box<TracerTraceChunks>`.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_trace_chunks_new(
    capacity: usize,
    out_handle: NonNull<Box<TracerTraceChunks>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        {
            let chunks = Vec::with_capacity(capacity);
            out_handle
                .as_ptr()
                .write(Box::new(TracerTraceChunks(chunks)));
            None
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Free a trace chunks container and all its contents.
///
/// After this call the handle is invalid and must not be reused.
///
/// # Safety
///
/// `handle` must have been created by [`ddog_tracer_trace_chunks_new`].
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_trace_chunks_free(handle: Option<Box<TracerTraceChunks>>) {
    drop(handle);
}

/// Start a new chunk (trace) inside the container.
///
/// Subsequent [`ddog_tracer_trace_chunks_push_span`] calls will append
/// spans to this chunk until the next `begin_chunk` call.
///
/// `capacity` is a hint for the expected number of spans in this chunk;
/// pass 0 if unknown.
///
/// # Safety
///
/// `handle` must be a valid pointer to a `TracerTraceChunks`.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_trace_chunks_begin_chunk(
    handle: Option<&mut TracerTraceChunks>,
    capacity: usize,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(chunks) = handle {
            chunks.0.push(Vec::with_capacity(capacity));
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Move a span into the current (last) chunk, consuming the span handle.
///
/// A chunk must have been started with
/// [`ddog_tracer_trace_chunks_begin_chunk`] before calling this function.
///
/// # Safety
///
/// * `handle` must be a valid pointer to a `TracerTraceChunks`.
/// * `span` is consumed and must not be used after this call.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_trace_chunks_push_span(
    handle: Option<&mut TracerTraceChunks>,
    span: Option<Box<TracerSpan>>,
) -> Option<Box<ExporterError>> {
    let Some(chunks) = handle else {
        return gen_error!(ErrorCode::InvalidArgument);
    };
    let Some(span) = span else {
        return gen_error!(ErrorCode::InvalidArgument);
    };

    catch_panic!(
        if let Some(chunk) = chunks.0.last_mut() {
            chunk.push(span.0);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

// ---------------------------------------------------------------------------
// Cancellation token
// ---------------------------------------------------------------------------

/// Create a new cancellation token.
///
/// The returned token must be freed with
/// [`ddog_trace_exporter_cancel_token_drop`].
#[no_mangle]
pub extern "C" fn ddog_trace_exporter_cancel_token_new() -> Box<TokioCancellationToken> {
    Box::new(TokioCancellationToken::new())
}

/// Cancel a cancellation token.
///
/// All clones of the same token observe the cancellation. Cancellation is cooperative and only
/// affects a [`ddog_trace_exporter_send_trace_chunks`] call that is in flight: that send stops
/// waiting for the agent at its next await point and fails with
/// [`ExporterErrorCode::IoError`], and the chunks it was sending may be lost.
///
/// Cancelling while no send is using the token has no immediate effect. A send started later with
/// an already-cancelled token fails the same way without contacting the agent, and cancelling
/// after a send has finished does nothing.
#[no_mangle]
pub extern "C" fn ddog_trace_exporter_cancel_token_cancel(token: Option<&TokioCancellationToken>) {
    if let Some(token) = token {
        token.cancel();
    }
}

/// Free a cancellation token.
///
/// After this call the token is invalid and must not be reused.
#[no_mangle]
pub extern "C" fn ddog_trace_exporter_cancel_token_drop(
    token: Option<Box<TokioCancellationToken>>,
) {
    drop(token);
}

// ---------------------------------------------------------------------------
// Send trace chunks
// ---------------------------------------------------------------------------

/// Send trace chunks through a [`TraceExporter`], consuming the chunks.
///
/// Computes stats, serializes in the configured output format, and sends to the agent with
/// retries.
///
/// When `cancel` is non-null, cancelling that token aborts the in-flight request; see
/// [`ddog_trace_exporter_cancel_token_cancel`].
///
/// On success, if `response_out` is non-null, a heap-allocated
/// [`ExporterResponse`] is written there.  The caller owns it and must
/// free it with `ddog_trace_exporter_response_free`.
///
/// # Safety
///
/// * `chunks` is consumed and must not be used after this call.
/// * If `response_out` is non-null it must point to valid writable memory for a
///   `Box<ExporterResponse>`.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_send_trace_chunks(
    exporter: Option<&TraceExporter>,
    chunks: Option<Box<TracerTraceChunks>>,
    response_out: Option<NonNull<Box<ExporterResponse>>>,
    cancel: Option<&TokioCancellationToken>,
) -> Option<Box<ExporterError>> {
    let Some(exporter) = exporter else {
        return gen_error!(ErrorCode::InvalidArgument);
    };
    let Some(chunks) = chunks else {
        return gen_error!(ErrorCode::InvalidArgument);
    };

    catch_panic!(
        match exporter.send_trace_chunks(chunks.0, cancel) {
            Ok(resp) => {
                if let Some(out) = response_out {
                    out.as_ptr().write(Box::new(ExporterResponse::from(resp)));
                }
                None
            }
            Err(e) => Some(Box::new(ExporterError::from(e))),
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

    fn bs(bytes: &[u8]) -> ByteSlice<'_> {
        ByteSlice::from(bytes)
    }

    fn make_minimal_span() -> Box<TracerSpan> {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerSpan>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();
            let fields = TracerSpanFields {
                service: cs("svc"),
                name: cs("op"),
                resource: cs("res"),
                span_type: cs(""),
                trace_id_low: 1,
                trace_id_high: 0,
                span_id: 1,
                parent_id: 0,
                start: 0,
                duration: 0,
                error: 0,
            };
            let err = ddog_tracer_span_new(out, Some(&fields));
            assert!(err.is_none());
            handle.assume_init()
        }
    }

    fn make_event(name: &str, time_unix_nano: u64) -> Box<TracerSpanEvent> {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerSpanEvent>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();
            let err = ddog_tracer_span_event_new(out, cs(name), time_unix_nano);
            assert!(err.is_none());
            handle.assume_init()
        }
    }

    #[test]
    fn new_sets_all_scalar_fields() {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerSpan>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();

            let fields = TracerSpanFields {
                service: cs("my-service"),
                name: cs("web.request"),
                resource: cs("GET /users"),
                span_type: cs("web"),
                trace_id_low: 0xdeadbeef,
                trace_id_high: 0x00000001,
                span_id: 12345,
                parent_id: 67890,
                start: 1_700_000_000_000_000_000i64,
                duration: 25_000_000,
                error: 0,
            };
            let err = ddog_tracer_span_new(out, Some(&fields));
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
            assert!(span.0.meta_struct.is_empty());
            assert!(span.0.span_links.is_empty());
            assert!(span.0.span_events.is_empty());

            ddog_tracer_span_free(Some(span));
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

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_meta_overwrites_existing_key() {
        unsafe {
            let mut span = make_minimal_span();

            ddog_tracer_span_set_meta(Some(&mut *span), cs("k"), cs("v1"));
            ddog_tracer_span_set_meta(Some(&mut *span), cs("k"), cs("v2"));

            // After the introduction of `VecMap`, the length is still 2, as the data structure
            // tolerates duplicate entries.
            assert_eq!(span.0.meta.get("k").unwrap().as_ref(), "v2");

            ddog_tracer_span_free(Some(span));
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

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_meta_struct_blob_inserts_binary_entries() {
        unsafe {
            let mut span = make_minimal_span();
            let value = b"\x82\xa6nested\x92\xc3\xc0\xa3raw\xc4\x03\x00\xff\x80";

            let err =
                ddog_tracer_span_set_meta_struct_blob(Some(&mut *span), cs("_dd.stack"), bs(value));
            assert!(err.is_none());

            assert_eq!(span.0.meta_struct.get("_dd.stack").unwrap().as_ref(), value);

            ddog_tracer_span_free(Some(span));
        }
    }

    // Repeated keys are appended, not replaced: `VecMap` defers deduplication to encode time,
    // so both entries are retained and the last one wins on read.
    #[test]
    fn set_meta_struct_blob_last_write_wins() {
        unsafe {
            let mut span = make_minimal_span();

            ddog_tracer_span_set_meta_struct_blob(Some(&mut *span), cs("k"), bs(b"first"));
            ddog_tracer_span_set_meta_struct_blob(Some(&mut *span), cs("k"), bs(b"second"));

            assert_eq!(span.0.meta_struct.get("k").unwrap().as_ref(), b"second");
            assert_eq!(span.0.meta_struct.len(), 2);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_links_copies_complete_links_in_order() {
        unsafe {
            let mut span = make_minimal_span();
            let first_attributes = [TracerSpanLinkAttribute {
                key: cs("messaging.operation"),
                value: cs("receive"),
            }];
            let links = [
                TracerSpanLink {
                    trace_id_low: 0x0123,
                    trace_id_high: 0x4567,
                    span_id: 0x89ab,
                    attributes: Slice::from(&first_attributes[..]),
                    tracestate: cs("vendor=value"),
                    flags: 0x8000_0001,
                },
                TracerSpanLink {
                    trace_id_low: 2,
                    trace_id_high: 0,
                    span_id: 3,
                    attributes: Slice::default(),
                    tracestate: cs(""),
                    flags: 0,
                },
            ];

            let err = ddog_tracer_span_set_links(Some(&mut span), Slice::from(&links[..]));
            assert!(err.is_none());

            assert_eq!(span.0.span_links.len(), 2);
            assert_eq!(span.0.span_links[0].trace_id, 0x0123);
            assert_eq!(span.0.span_links[0].trace_id_high, 0x4567);
            assert_eq!(span.0.span_links[0].span_id, 0x89ab);
            assert_eq!(span.0.span_links[0].flags, 0x8000_0001);
            assert_eq!(span.0.span_links[0].tracestate.as_ref(), "vendor=value");
            assert_eq!(
                span.0.span_links[0]
                    .attributes
                    .get("messaging.operation")
                    .unwrap()
                    .as_ref(),
                "receive"
            );
            assert_eq!(span.0.span_links[1].trace_id, 2);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_links_copies_every_attribute_of_a_link() {
        unsafe {
            let mut span = make_minimal_span();
            let attributes = [
                TracerSpanLinkAttribute {
                    key: cs("messaging.operation"),
                    value: cs("receive"),
                },
                TracerSpanLinkAttribute {
                    key: cs("messaging.system"),
                    value: cs("kafka"),
                },
                TracerSpanLinkAttribute {
                    key: cs("link.kind"),
                    value: cs("follows_from"),
                },
            ];
            let links = [TracerSpanLink {
                trace_id_low: 1,
                trace_id_high: 2,
                span_id: 3,
                attributes: Slice::from(&attributes[..]),
                tracestate: cs(""),
                flags: 0,
            }];

            let err = ddog_tracer_span_set_links(Some(&mut span), Slice::from(&links[..]));
            assert!(err.is_none());

            let copied = &span.0.span_links[0].attributes;
            assert_eq!(copied.len(), 3);
            assert_eq!(
                copied.get("messaging.operation").unwrap().as_ref(),
                "receive"
            );
            assert_eq!(copied.get("messaging.system").unwrap().as_ref(), "kafka");
            assert_eq!(copied.get("link.kind").unwrap().as_ref(), "follows_from");

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_links_replaces_existing_links() {
        unsafe {
            let mut span = make_minimal_span();
            let first = [TracerSpanLink {
                trace_id_low: 1,
                trace_id_high: 0,
                span_id: 2,
                attributes: Slice::default(),
                tracestate: cs(""),
                flags: 0,
            }];
            assert!(ddog_tracer_span_set_links(Some(&mut span), Slice::from(&first[..])).is_none());

            let second = [TracerSpanLink {
                trace_id_low: 3,
                trace_id_high: 4,
                span_id: 5,
                attributes: Slice::default(),
                tracestate: cs("state=value"),
                flags: 6,
            }];
            assert!(
                ddog_tracer_span_set_links(Some(&mut span), Slice::from(&second[..])).is_none()
            );

            assert_eq!(span.0.span_links.len(), 1);
            assert_eq!(span.0.span_links[0].trace_id, 3);
            assert_eq!(span.0.span_links[0].trace_id_high, 4);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_links_empty_slice_clears_existing_links() {
        unsafe {
            let mut span = make_minimal_span();
            let links = [TracerSpanLink {
                trace_id_low: 1,
                trace_id_high: 0,
                span_id: 2,
                attributes: Slice::default(),
                tracestate: cs(""),
                flags: 0,
            }];
            assert!(ddog_tracer_span_set_links(Some(&mut span), Slice::from(&links[..])).is_none());
            assert_eq!(span.0.span_links.len(), 1);

            assert!(ddog_tracer_span_set_links(Some(&mut span), Slice::default()).is_none());
            assert!(span.0.span_links.is_empty());

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_links_failure_is_atomic() {
        unsafe {
            let mut span = make_minimal_span();
            span.0.span_links.push(SpanLinkBytes {
                trace_id: 7,
                span_id: 8,
                ..Default::default()
            });
            let invalid = [0xff];
            let links = [
                TracerSpanLink {
                    trace_id_low: 1,
                    trace_id_high: 2,
                    span_id: 3,
                    attributes: Slice::default(),
                    tracestate: cs("valid=value"),
                    flags: 4,
                },
                TracerSpanLink {
                    trace_id_low: 5,
                    trace_id_high: 6,
                    span_id: 7,
                    attributes: Slice::default(),
                    tracestate: CharSlice::from_bytes(&invalid),
                    flags: 8,
                },
            ];

            let err = ddog_tracer_span_set_links(Some(&mut span), Slice::from(&links[..]));
            assert!(err.is_some());
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidInput);
            ddog_trace_exporter_error_free(err);
            assert_eq!(span.0.span_links.len(), 1);
            assert_eq!(span.0.span_links[0].trace_id, 7);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_links_rejects_invalid_attribute_utf8_atomically() {
        unsafe {
            let mut span = make_minimal_span();
            span.0.span_links.push(SpanLinkBytes {
                trace_id: 7,
                ..Default::default()
            });
            let invalid = [0xff];
            let attributes = [TracerSpanLinkAttribute {
                key: cs("key"),
                value: CharSlice::from_bytes(&invalid),
            }];
            let links = [TracerSpanLink {
                trace_id_low: 1,
                trace_id_high: 2,
                span_id: 3,
                attributes: Slice::from(&attributes[..]),
                tracestate: cs(""),
                flags: 4,
            }];

            let err = ddog_tracer_span_set_links(Some(&mut span), Slice::from(&links[..]));
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
            assert_eq!(span.0.span_links.len(), 1);
            assert_eq!(span.0.span_links[0].trace_id, 7);

            ddog_tracer_span_free(Some(span));
        }
    }

    /// An invalid `links` slice must be rejected rather than dereferenced: `try_as_slice`
    /// fails and the span's existing links are left alone.
    #[test]
    fn set_links_rejects_invalid_links_slice_atomically() {
        unsafe {
            let mut span = make_minimal_span();
            span.0.span_links.push(SpanLinkBytes {
                trace_id: 7,
                ..Default::default()
            });

            let bad: Slice<'_, TracerSpanLink<'_>> = Slice::from_raw_parts(std::ptr::null(), 1);
            let err = ddog_tracer_span_set_links(Some(&mut span), bad);
            assert!(err.is_some());
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
            assert_eq!(span.0.span_links.len(), 1);
            assert_eq!(span.0.span_links[0].trace_id, 7);

            ddog_tracer_span_free(Some(span));
        }
    }

    /// Same for a nested `attributes` slice, which is validated per link.
    #[test]
    fn set_links_rejects_invalid_attributes_slice_atomically() {
        unsafe {
            let mut span = make_minimal_span();
            span.0.span_links.push(SpanLinkBytes {
                trace_id: 7,
                ..Default::default()
            });
            let links = [TracerSpanLink {
                trace_id_low: 1,
                trace_id_high: 2,
                span_id: 3,
                attributes: Slice::from_raw_parts(std::ptr::null(), 1),
                tracestate: cs(""),
                flags: 4,
            }];

            let err = ddog_tracer_span_set_links(Some(&mut span), Slice::from(&links[..]));
            assert!(err.is_some());
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
            assert_eq!(span.0.span_links.len(), 1);
            assert_eq!(span.0.span_links[0].trace_id, 7);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_links_null_handle_returns_error() {
        unsafe {
            let err = ddog_tracer_span_set_links(None, Slice::default());
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
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
    fn event_attributes_preserve_types_and_arrays() {
        unsafe {
            let mut event = make_event("exception", 123);
            assert!(
                ddog_tracer_span_event_set_string(Some(&mut event), cs("string"), cs("value"))
                    .is_none()
            );
            assert!(ddog_tracer_span_event_set_bool(Some(&mut event), cs("bool"), true).is_none());
            assert!(ddog_tracer_span_event_set_int(Some(&mut event), cs("int"), -42).is_none());
            assert!(
                ddog_tracer_span_event_set_double(Some(&mut event), cs("double"), 1.5).is_none()
            );

            let strings = [cs("one"), cs("two")];
            let bools = [true, false];
            let ints = [-1, 2];
            let doubles = [1.25, 2.5];
            assert!(ddog_tracer_span_event_set_string_array(
                Some(&mut event),
                cs("strings"),
                Slice::from(&strings[..])
            )
            .is_none());
            assert!(ddog_tracer_span_event_set_bool_array(
                Some(&mut event),
                cs("bools"),
                Slice::from(&bools[..])
            )
            .is_none());
            assert!(ddog_tracer_span_event_set_int_array(
                Some(&mut event),
                cs("ints"),
                Slice::from(&ints[..])
            )
            .is_none());
            assert!(ddog_tracer_span_event_set_double_array(
                Some(&mut event),
                cs("doubles"),
                Slice::from(&doubles[..])
            )
            .is_none());

            assert_eq!(event.0.name.as_ref(), "exception");
            assert_eq!(event.0.time_unix_nano, 123);
            assert_eq!(
                event.0.attributes.get("string"),
                Some(&AttributeAnyValueBytes::SingleValue(
                    AttributeArrayValueBytes::String(BytesString::from("value"))
                ))
            );
            assert_eq!(
                event.0.attributes.get("bool"),
                Some(&AttributeAnyValueBytes::SingleValue(
                    AttributeArrayValueBytes::Boolean(true)
                ))
            );
            assert_eq!(
                event.0.attributes.get("int"),
                Some(&AttributeAnyValueBytes::SingleValue(
                    AttributeArrayValueBytes::Integer(-42)
                ))
            );
            assert_eq!(
                event.0.attributes.get("double"),
                Some(&AttributeAnyValueBytes::SingleValue(
                    AttributeArrayValueBytes::Double(1.5)
                ))
            );
            assert_eq!(
                event.0.attributes.get("strings"),
                Some(&AttributeAnyValueBytes::Array(vec![
                    AttributeArrayValueBytes::String(BytesString::from("one")),
                    AttributeArrayValueBytes::String(BytesString::from("two")),
                ]))
            );
            assert_eq!(
                event.0.attributes.get("bools"),
                Some(&AttributeAnyValueBytes::Array(vec![
                    AttributeArrayValueBytes::Boolean(true),
                    AttributeArrayValueBytes::Boolean(false),
                ]))
            );
            assert_eq!(
                event.0.attributes.get("ints"),
                Some(&AttributeAnyValueBytes::Array(vec![
                    AttributeArrayValueBytes::Integer(-1),
                    AttributeArrayValueBytes::Integer(2),
                ]))
            );
            assert_eq!(
                event.0.attributes.get("doubles"),
                Some(&AttributeAnyValueBytes::Array(vec![
                    AttributeArrayValueBytes::Double(1.25),
                    AttributeArrayValueBytes::Double(2.5),
                ]))
            );

            ddog_tracer_span_event_free(Some(event));
        }
    }

    #[test]
    fn event_attributes_accept_empty_typed_arrays() {
        unsafe {
            let mut event = make_event("empty-arrays", 123);
            let strings: [CharSlice<'_>; 0] = [];
            let bools: [bool; 0] = [];
            let ints: [i64; 0] = [];
            let doubles: [f64; 0] = [];

            assert!(ddog_tracer_span_event_set_string_array(
                Some(&mut event),
                cs("strings"),
                Slice::from(&strings[..])
            )
            .is_none());
            assert!(ddog_tracer_span_event_set_bool_array(
                Some(&mut event),
                cs("bools"),
                Slice::from(&bools[..])
            )
            .is_none());
            assert!(ddog_tracer_span_event_set_int_array(
                Some(&mut event),
                cs("ints"),
                Slice::from(&ints[..])
            )
            .is_none());
            assert!(ddog_tracer_span_event_set_double_array(
                Some(&mut event),
                cs("doubles"),
                Slice::from(&doubles[..])
            )
            .is_none());

            for key in ["strings", "bools", "ints", "doubles"] {
                assert_eq!(
                    event.0.attributes.get(key),
                    Some(&AttributeAnyValueBytes::Array(Vec::new()))
                );
            }

            ddog_tracer_span_event_free(Some(event));
        }
    }

    #[test]
    fn invalid_event_strings_do_not_partially_update_the_event() {
        unsafe {
            let mut event = make_event("invalid", 123);
            let invalid = CharSlice::from_bytes(&[0xff]);
            let err = ddog_tracer_span_event_set_string(Some(&mut event), cs("scalar"), invalid);
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidInput);
            ddog_trace_exporter_error_free(err);
            assert!(event.0.attributes.is_empty());

            let values = [cs("valid"), invalid];
            let err = ddog_tracer_span_event_set_string_array(
                Some(&mut event),
                cs("array"),
                Slice::from(&values[..]),
            );
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidInput);
            ddog_trace_exporter_error_free(err);
            assert!(event.0.attributes.is_empty());

            ddog_tracer_span_event_free(Some(event));
        }
    }

    #[test]
    fn malformed_event_arrays_report_invalid_argument() {
        unsafe {
            let mut event = make_event("malformed", 123);

            // Null pointer
            let strings = Slice::from_raw_parts(std::ptr::null::<CharSlice>(), 2);
            let bools = Slice::from_raw_parts(std::ptr::null::<bool>(), 2);
            let ints = Slice::from_raw_parts(std::ptr::null::<i64>(), 2);
            let doubles = Slice::from_raw_parts(std::ptr::null::<f64>(), 2);

            for err in [
                ddog_tracer_span_event_set_string_array(Some(&mut event), cs("strings"), strings),
                ddog_tracer_span_event_set_bool_array(Some(&mut event), cs("bools"), bools),
                ddog_tracer_span_event_set_int_array(Some(&mut event), cs("ints"), ints),
                ddog_tracer_span_event_set_double_array(Some(&mut event), cs("doubles"), doubles),
            ] {
                assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
                ddog_trace_exporter_error_free(err);
            }
            assert!(event.0.attributes.is_empty());

            // Misaligned pointer
            let buf = [0u8; 32];
            let misaligned = buf.as_ptr().add(1).cast::<i64>();
            assert!(!misaligned.is_aligned());
            let err = ddog_tracer_span_event_set_int_array(
                Some(&mut event),
                cs("ints"),
                Slice::from_raw_parts(misaligned, 2),
            );
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
            assert!(event.0.attributes.is_empty());

            // A malformed element
            let elements = [cs("valid"), Slice::from_raw_parts(std::ptr::null(), 1)];
            let err = ddog_tracer_span_event_set_string_array(
                Some(&mut event),
                cs("strings"),
                Slice::from(&elements[..]),
            );
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
            assert!(event.0.attributes.is_empty());

            // A malformed key
            let err = ddog_tracer_span_event_set_bool(
                Some(&mut event),
                Slice::from_raw_parts(std::ptr::null(), 1),
                true,
            );
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
            assert!(event.0.attributes.is_empty());

            // A length beyond `isize::MAX`.
            let one = [1i64];
            let err = ddog_tracer_span_event_set_int_array(
                Some(&mut event),
                cs("ints"),
                Slice::from_raw_parts(one.as_ptr(), usize::MAX),
            );
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
            assert!(event.0.attributes.is_empty());

            ddog_tracer_span_event_free(Some(event));
        }
    }

    #[test]
    fn event_setters_reject_a_null_event() {
        unsafe {
            let strings = [cs("one")];
            let bools = [true];
            let ints = [1i64];
            let doubles = [1.0f64];

            for err in [
                ddog_tracer_span_event_set_string(None, cs("k"), cs("v")),
                ddog_tracer_span_event_set_bool(None, cs("k"), true),
                ddog_tracer_span_event_set_int(None, cs("k"), 1),
                ddog_tracer_span_event_set_double(None, cs("k"), 1.0),
                ddog_tracer_span_event_set_string_array(None, cs("k"), Slice::from(&strings[..])),
                ddog_tracer_span_event_set_bool_array(None, cs("k"), Slice::from(&bools[..])),
                ddog_tracer_span_event_set_int_array(None, cs("k"), Slice::from(&ints[..])),
                ddog_tracer_span_event_set_double_array(None, cs("k"), Slice::from(&doubles[..])),
            ] {
                assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
                ddog_trace_exporter_error_free(err);
            }
        }
    }

    #[test]
    fn invalid_utf8_event_keys_report_invalid_input() {
        unsafe {
            let mut event = make_event("keys", 1);
            let invalid = CharSlice::from_bytes(&[0xff]);
            let strings = [cs("one")];
            let bools = [true];
            let ints = [1i64];
            let doubles = [1.0f64];

            for err in [
                ddog_tracer_span_event_set_string(Some(&mut event), invalid, cs("v")),
                ddog_tracer_span_event_set_bool(Some(&mut event), invalid, true),
                ddog_tracer_span_event_set_int(Some(&mut event), invalid, 1),
                ddog_tracer_span_event_set_double(Some(&mut event), invalid, 1.0),
                ddog_tracer_span_event_set_string_array(
                    Some(&mut event),
                    invalid,
                    Slice::from(&strings[..]),
                ),
                ddog_tracer_span_event_set_bool_array(
                    Some(&mut event),
                    invalid,
                    Slice::from(&bools[..]),
                ),
                ddog_tracer_span_event_set_int_array(
                    Some(&mut event),
                    invalid,
                    Slice::from(&ints[..]),
                ),
                ddog_tracer_span_event_set_double_array(
                    Some(&mut event),
                    invalid,
                    Slice::from(&doubles[..]),
                ),
            ] {
                assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidInput);
                ddog_trace_exporter_error_free(err);
            }
            assert!(event.0.attributes.is_empty());

            ddog_tracer_span_event_free(Some(event));
        }
    }

    #[test]
    fn event_new_rejects_invalid_names() {
        unsafe {
            // Invalid UTF-8 contents.
            let mut handle = MaybeUninit::<Box<TracerSpanEvent>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();
            let err = ddog_tracer_span_event_new(out, CharSlice::from_bytes(&[0xff]), 1);
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidInput);
            ddog_trace_exporter_error_free(err);

            // A malformed name slice, rejected before any UTF-8 check.
            let mut handle = MaybeUninit::<Box<TracerSpanEvent>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();
            let err =
                ddog_tracer_span_event_new(out, Slice::from_raw_parts(std::ptr::null(), 1), 1);
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
        }
    }

    #[test]
    fn event_new_accepts_an_empty_name() {
        unsafe {
            let event = make_event("", 0);
            assert_eq!(event.0.name.as_ref(), "");
            assert_eq!(event.0.time_unix_nano, 0);
            assert!(event.0.attributes.is_empty());
            ddog_tracer_span_event_free(Some(event));
        }
    }

    #[test]
    fn event_free_accepts_null() {
        unsafe {
            ddog_tracer_span_event_free(None);
        }
    }

    #[test]
    fn add_event_rejects_a_null_event() {
        unsafe {
            let mut span = make_minimal_span();
            let err = ddog_tracer_span_add_event(Some(&mut span), None);
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
            assert!(span.0.span_events.is_empty());
            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn attached_events_keep_their_attributes() {
        unsafe {
            let mut span = make_minimal_span();
            let mut event = make_event("exception", 42);
            assert!(
                ddog_tracer_span_event_set_string(Some(&mut event), cs("message"), cs("boom"))
                    .is_none()
            );
            let codes = [1i64, 2];
            assert!(ddog_tracer_span_event_set_int_array(
                Some(&mut event),
                cs("codes"),
                Slice::from(&codes[..])
            )
            .is_none());

            assert!(ddog_tracer_span_add_event(Some(&mut span), Some(event)).is_none());

            assert_eq!(span.0.span_events.len(), 1);
            let attached = &span.0.span_events[0];
            assert_eq!(attached.name.as_ref(), "exception");
            assert_eq!(attached.time_unix_nano, 42);
            assert_eq!(attached.attributes.len(), 2);
            assert_eq!(
                attached.attributes.get("message"),
                Some(&AttributeAnyValueBytes::SingleValue(
                    AttributeArrayValueBytes::String(BytesString::from("boom"))
                ))
            );
            assert_eq!(
                attached.attributes.get("codes"),
                Some(&AttributeAnyValueBytes::Array(vec![
                    AttributeArrayValueBytes::Integer(1),
                    AttributeArrayValueBytes::Integer(2),
                ]))
            );

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn attaching_events_is_atomic_and_preserves_order() {
        unsafe {
            let mut span = make_minimal_span();
            let first = make_event("first", 10);
            let second = make_event("second", 20);

            assert!(ddog_tracer_span_add_event(Some(&mut span), Some(first)).is_none());
            assert!(ddog_tracer_span_add_event(Some(&mut span), Some(second)).is_none());
            assert_eq!(span.0.span_events.len(), 2);
            assert_eq!(span.0.span_events[0].name.as_ref(), "first");
            assert_eq!(span.0.span_events[0].time_unix_nano, 10);
            assert_eq!(span.0.span_events[1].name.as_ref(), "second");
            assert_eq!(span.0.span_events[1].time_unix_nano, 20);

            let detached = make_event("detached", 30);
            let err = ddog_tracer_span_add_event(None, Some(detached));
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
            assert_eq!(span.0.span_events.len(), 2);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_meta_struct_blob_null_handle_returns_error() {
        unsafe {
            let err = ddog_tracer_span_set_meta_struct_blob(None, cs("k"), bs(b"value"));
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
        }
    }

    #[test]
    fn set_meta_struct_blob_invalid_key_returns_error() {
        unsafe {
            let mut span = make_minimal_span();
            let key = CharSlice::from_bytes(&[0xff]);

            let err = ddog_tracer_span_set_meta_struct_blob(Some(&mut *span), key, bs(b"value"));
            assert!(err.is_some());
            assert!(span.0.meta_struct.is_empty());
            ddog_trace_exporter_error_free(err);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_meta_struct_blob_null_value_returns_error() {
        unsafe {
            let mut span = make_minimal_span();
            let value = ByteSlice::from_raw_parts(std::ptr::null(), 5);

            let err = ddog_tracer_span_set_meta_struct_blob(Some(&mut *span), cs("k"), value);
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            assert!(span.0.meta_struct.is_empty());
            ddog_trace_exporter_error_free(err);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_meta_struct_blob_null_key_returns_error() {
        unsafe {
            let mut span = make_minimal_span();
            let key = CharSlice::from_raw_parts(std::ptr::null(), 5);

            let err = ddog_tracer_span_set_meta_struct_blob(Some(&mut *span), key, bs(b"value"));
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            assert!(span.0.meta_struct.is_empty());
            ddog_trace_exporter_error_free(err);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_meta_struct_blob_accepts_empty_value() {
        unsafe {
            let mut span = make_minimal_span();

            let err = ddog_tracer_span_set_meta_struct_blob(Some(&mut *span), cs("k"), bs(b""));
            assert!(err.is_none());
            assert_eq!(span.0.meta_struct.get("k").unwrap().as_ref(), b"");

            ddog_tracer_span_free(Some(span));
        }
    }

    // An empty key is valid UTF-8 and is technically accepted.
    #[test]
    fn set_meta_struct_blob_accepts_empty_key() {
        unsafe {
            let mut span = make_minimal_span();

            let err = ddog_tracer_span_set_meta_struct_blob(Some(&mut *span), cs(""), bs(b"value"));
            assert!(err.is_none());
            assert_eq!(span.0.meta_struct.get("").unwrap().as_ref(), b"value");

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn set_meta_null_value_returns_error() {
        unsafe {
            let mut span = make_minimal_span();
            let value = CharSlice::from_raw_parts(std::ptr::null(), 5);

            let err = ddog_tracer_span_set_meta(Some(&mut *span), cs("k"), value);
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            assert!(span.0.meta.is_empty());
            ddog_trace_exporter_error_free(err);

            ddog_tracer_span_free(Some(span));
        }
    }

    #[test]
    fn new_with_empty_strings_succeeds() {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerSpan>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();

            let fields = TracerSpanFields {
                service: cs(""),
                name: cs(""),
                resource: cs(""),
                span_type: cs(""),
                trace_id_low: 0,
                trace_id_high: 0,
                span_id: 0,
                parent_id: 0,
                start: 0,
                duration: 0,
                error: 0,
            };
            let err = ddog_tracer_span_new(out, Some(&fields));
            assert!(err.is_none());

            let span = handle.assume_init();
            assert_eq!(span.0.name.as_ref(), "");
            assert_eq!(span.0.service.as_ref(), "");

            ddog_tracer_span_free(Some(span));
        }
    }

    // -- TracerTraceChunks tests --------------------------------------------

    fn make_chunks(capacity: usize) -> Box<TracerTraceChunks> {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerTraceChunks>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();
            let err = ddog_tracer_trace_chunks_new(capacity, out);
            assert!(err.is_none());
            handle.assume_init()
        }
    }

    #[test]
    fn trace_chunks_build_and_push() {
        unsafe {
            let mut chunks = make_chunks(2);

            // Chunk 1: two spans
            let err = ddog_tracer_trace_chunks_begin_chunk(Some(&mut *chunks), 2);
            assert!(err.is_none());

            let s1 = make_minimal_span();
            let err = ddog_tracer_trace_chunks_push_span(Some(&mut *chunks), Some(s1));
            assert!(err.is_none());

            let s2 = make_minimal_span();
            let err = ddog_tracer_trace_chunks_push_span(Some(&mut *chunks), Some(s2));
            assert!(err.is_none());

            // Chunk 2: one span
            let err = ddog_tracer_trace_chunks_begin_chunk(Some(&mut *chunks), 1);
            assert!(err.is_none());
            let s3 = make_minimal_span();
            let err = ddog_tracer_trace_chunks_push_span(Some(&mut *chunks), Some(s3));
            assert!(err.is_none());

            assert_eq!(chunks.0.len(), 2);
            assert_eq!(chunks.0[0].len(), 2);
            assert_eq!(chunks.0[1].len(), 1);

            ddog_tracer_trace_chunks_free(Some(chunks));
        }
    }

    #[test]
    fn begin_chunk_null_handle_returns_error() {
        unsafe {
            let err = ddog_tracer_trace_chunks_begin_chunk(None, 0);
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
        }
    }

    #[test]
    fn push_span_without_begin_chunk_returns_error() {
        unsafe {
            let mut chunks = make_chunks(0);

            // No begin_chunk — push should fail
            let s = make_minimal_span();
            let err = ddog_tracer_trace_chunks_push_span(Some(&mut *chunks), Some(s));
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);

            ddog_tracer_trace_chunks_free(Some(chunks));
        }
    }

    #[test]
    fn push_span_null_span_returns_error() {
        unsafe {
            let mut chunks = make_chunks(1);
            let err = ddog_tracer_trace_chunks_begin_chunk(Some(&mut *chunks), 0);
            assert!(err.is_none());

            let err = ddog_tracer_trace_chunks_push_span(Some(&mut *chunks), None);
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);

            ddog_tracer_trace_chunks_free(Some(chunks));
        }
    }

    #[test]
    fn push_span_null_handle_returns_error() {
        unsafe {
            let s = make_minimal_span();
            let err = ddog_tracer_trace_chunks_push_span(None, Some(s));
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
        }
    }

    #[test]
    fn trace_chunks_empty_is_valid() {
        unsafe {
            let chunks = make_chunks(0);
            assert_eq!(chunks.0.len(), 0);
            ddog_tracer_trace_chunks_free(Some(chunks));
        }
    }

    #[test]
    fn trace_chunks_empty_chunk_is_valid() {
        unsafe {
            let mut chunks = make_chunks(1);
            let err = ddog_tracer_trace_chunks_begin_chunk(Some(&mut *chunks), 0);
            assert!(err.is_none());

            assert_eq!(chunks.0.len(), 1);
            assert_eq!(chunks.0[0].len(), 0);

            ddog_tracer_trace_chunks_free(Some(chunks));
        }
    }

    #[test]
    fn span_new_null_fields_returns_error() {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerSpan>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();
            let err = ddog_tracer_span_new(out, None);
            assert!(err.is_some());
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
        }
    }

    #[test]
    fn send_trace_chunks_null_exporter_returns_error() {
        unsafe {
            let chunks = make_chunks(0);
            let err = ddog_trace_exporter_send_trace_chunks(None, Some(chunks), None, None);
            assert!(err.is_some());
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(err);
        }
    }

    // Capacity-overflow tests: a C caller passing `usize::MAX` would make
    // `Vec::with_capacity` panic with "capacity overflow"; the `catch_panic!`
    // guard must convert that into `ErrorCode::Panic` instead of aborting
    // the process.
    #[cfg(all(feature = "catch_panic", panic = "unwind"))]
    #[test]
    fn trace_chunks_new_with_overflow_capacity_returns_panic_error() {
        unsafe {
            let mut handle = MaybeUninit::<Box<TracerTraceChunks>>::uninit();
            let out = NonNull::new(handle.as_mut_ptr()).unwrap();
            let err = ddog_tracer_trace_chunks_new(usize::MAX, out);
            assert!(err.is_some());
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::Panic);
            ddog_trace_exporter_error_free(err);
        }
    }

    #[cfg(all(feature = "catch_panic", panic = "unwind"))]
    #[test]
    fn begin_chunk_with_overflow_capacity_returns_panic_error() {
        unsafe {
            let mut chunks = make_chunks(0);
            let err = ddog_tracer_trace_chunks_begin_chunk(Some(&mut *chunks), usize::MAX);
            assert!(err.is_some());
            assert_eq!(err.as_ref().unwrap().code, ErrorCode::Panic);
            ddog_trace_exporter_error_free(err);
            ddog_tracer_trace_chunks_free(Some(chunks));
        }
    }

    // -- Cancellation token -------------------------------------------------

    #[test]
    fn cancel_token_new_and_drop() {
        let token = ddog_trace_exporter_cancel_token_new();
        ddog_trace_exporter_cancel_token_drop(Some(token));
    }

    #[test]
    fn cancel_token_cancel() {
        let token = ddog_trace_exporter_cancel_token_new();
        ddog_trace_exporter_cancel_token_cancel(Some(&token));
        ddog_trace_exporter_cancel_token_drop(Some(token));
    }

    #[test]
    fn send_trace_chunks_null_cancel_is_accepted() {
        // Passing a null (None) cancel argument behaves like no cancellation.
        unsafe {
            let chunks = make_chunks(0);
            let err = ddog_trace_exporter_send_trace_chunks(None, Some(chunks), None, None);
            // exporter is None, so we get InvalidArgument, but no crash
            // from the absent cancel argument.
            assert!(err.is_some());
            ddog_trace_exporter_error_free(err);
        }
    }
}
