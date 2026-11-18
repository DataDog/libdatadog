// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provider responses, read whole or as a stream.

use libdd_ai_usage::{
    ProviderReport, ProviderResponse, StreamOutcome, UsageFormat, UsageStream, parse_provider_body,
};
use libdd_common_ffi::slice::ByteSlice;
use libdd_common_ffi::{CharSlice, Handle, StringWrapper, ToInner};

use crate::{
    AiUsageError, Failure, bytes, catch_panic, error_of, invalid, json_text, panicked,
    panicked_output, shared, text, write_output,
};

/// An option of [`ddog_ai_usage_parse_provider_response_with`] and
/// [`ddog_ai_usage_stream_new_with`]: report usage with inclusive totals,
/// cache reads and writes added to an input that excludes them and
/// reasoning added to an output that excludes it, both bases then
/// `includes_*`.
pub const INCLUSIVE_TOTALS: u32 = 1;

/// A provider stream being read. Use one from one thread at a time.
pub struct Stream {
    inner: UsageStream,
    /// Report usage with inclusive totals ([`INCLUSIVE_TOTALS`]).
    inclusive: bool,
}

fn format_of(name: CharSlice<'_>) -> Result<UsageFormat, Failure> {
    let name = text(name).ok_or(Failure::Internal("format is not UTF-8"))?;
    Ok(UsageFormat::parse(name)?)
}

/// Whether `options` asks for inclusive totals; unknown bits are refused.
fn options_of(options: u32) -> Result<bool, Failure> {
    if options & !INCLUSIVE_TOTALS != 0 {
        return Err(invalid(format!("unknown options: {options:#x}")));
    }
    Ok(options & INCLUSIVE_TOTALS != 0)
}

/// What a provider response or stream reported, as JSON.
fn report_text(
    (response, outcome, error_name): (ProviderResponse, StreamOutcome, Option<String>),
    inclusive: bool,
) -> Result<String, Failure> {
    let report = ProviderReport {
        inclusive_totals: inclusive,
        ..ProviderReport::new(response, outcome, error_name)
    };
    json_text(serde_json::to_value(&report))
}

/// Read a whole provider response `body` (a JSON document, or a buffered
/// stream of server-sent events) in the usage `format` (`auto`,
/// `openai_chat`, `openai_responses`, `anthropic`, `gemini` or
/// `gemini_openai_compat`; empty is `auto`), and write `{"usage": {...} |
/// null, "response_model", "service_tier", "outcome", "error_name",
/// "error_type", "error_code", "error_message", "issues"}` to `out`.
/// `issues` says why something reported was not kept:
/// `service_tier_dropped` for a tier outside `^[A-Za-z0-9._-]{1,32}$`, and
/// `response_model_dropped` for a served model outside the deployment value
/// grammar (so a provider cannot mint tag values from free text).
/// `usage` holds the members to merge into an observation. Its
/// `input_tokens_by_modality` and `output_tokens_by_modality` hold the
/// provider's own `image` and `audio` parts, counted within the totals:
/// OpenAI-shaped usage reports image output tokens (image generation) as
/// `output_tokens_by_modality.image`, which a caller prices at the model's
/// image rate. `error_code` and `error_message` are the provider's own, from
/// the body or an in-band stream error: control characters removed, the
/// message cut to 256 bytes, so a gateway can log them without parsing the
/// error itself. `embeddings` is true for an embeddings operation.
///
/// # Safety
///
/// `format` and `body` must be valid slices, and `out` null or point to a
/// `ddog_StringWrapper` the caller releases with `ddog_StringWrapper_drop`.
/// `out` is written whether the call fails or not (empty when it fails).
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_parse_provider_response(
    format: CharSlice,
    embeddings: bool,
    body: ByteSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe { parse(format, embeddings, 0, body, out) },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// [`ddog_ai_usage_parse_provider_response`] with `options`, a bit set:
/// [`INCLUSIVE_TOTALS`] reports usage with inclusive totals. Unknown bits
/// are rejected (`field_type_invalid`).
///
/// # Safety
///
/// As [`ddog_ai_usage_parse_provider_response`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_parse_provider_response_with(
    format: CharSlice,
    embeddings: bool,
    options: u32,
    body: ByteSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe { parse(format, embeddings, options, body, out) },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// # Safety
///
/// As [`ddog_ai_usage_parse_provider_response`].
unsafe fn parse(
    format: CharSlice,
    embeddings: bool,
    options: u32,
    body: ByteSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    // SAFETY: forwarded from the caller.
    unsafe {
        write_output(out, || {
            let inclusive = options_of(options)?;
            let format = format_of(format)?;
            let body = bytes(body).ok_or(Failure::Internal("the body is null"))?;
            report_text(parse_provider_body(format, body, embeddings), inclusive)
        })
    }
}

/// Start reading a provider stream in the usage `format`, as
/// [`ddog_ai_usage_parse_provider_response`] names them, and write its handle
/// to `out`. Release it with [`ddog_ai_usage_stream_take`] or
/// [`ddog_ai_usage_stream_drop`]. `out` is left as it was when the call
/// fails.
///
/// # Safety
///
/// `format` must be a valid slice, and `out` null or point to memory a
/// handle can be written to.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_stream_new(
    format: CharSlice,
    embeddings: bool,
    out: *mut Handle<Stream>,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe { new_stream(format, embeddings, 0, out) },
        panicked()
    )
}

/// [`ddog_ai_usage_stream_new`] with `options`, as
/// [`ddog_ai_usage_parse_provider_response_with`] reads them: with
/// [`INCLUSIVE_TOTALS`], `_take` reports inclusive totals.
///
/// # Safety
///
/// As [`ddog_ai_usage_stream_new`].
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_stream_new_with(
    format: CharSlice,
    embeddings: bool,
    options: u32,
    out: *mut Handle<Stream>,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe { new_stream(format, embeddings, options, out) },
        panicked()
    )
}

/// # Safety
///
/// As [`ddog_ai_usage_stream_new`].
unsafe fn new_stream(
    format: CharSlice,
    embeddings: bool,
    options: u32,
    out: *mut Handle<Stream>,
) -> Option<Box<AiUsageError>> {
    error_of((|| {
        if out.is_null() {
            return Err(Failure::Internal("the output is null"));
        }
        let inclusive = options_of(options)?;
        let format = format_of(format)?;
        let stream = Stream {
            inner: UsageStream::new(format, embeddings),
            inclusive,
        };
        // SAFETY: `out` is not null and the caller guarantees it is writable.
        unsafe { out.write(Handle::from(stream)) };
        Ok(())
    })())
}

/// The stream a handle holds, borrowed exclusively.
///
/// # Safety
///
/// `stream` must be null or a handle from [`ddog_ai_usage_stream_new`], not
/// used by another thread during the borrow.
unsafe fn stream_of<'a>(stream: *mut Handle<Stream>) -> Result<&'a mut Stream, Failure> {
    let released = || Failure::Internal("the stream is null or was released");
    // SAFETY: forwarded from the caller.
    let handle = unsafe { stream.as_mut() }.ok_or_else(released)?;
    // SAFETY: a handle this library wrote holds a live stream or null.
    unsafe { handle.to_inner_mut() }.map_err(|_| released())
}

/// Read the next `chunk` of the stream, which may end anywhere.
///
/// # Safety
///
/// `stream` must be a handle from [`ddog_ai_usage_stream_new`], not used by
/// another thread during the call, and `chunk` a valid slice.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_stream_feed(
    stream: *mut Handle<Stream>,
    chunk: ByteSlice,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        error_of((|| {
            // SAFETY: forwarded from the caller.
            let stream = unsafe { stream_of(stream) }?;
            let chunk = bytes(chunk).ok_or(Failure::Internal("the chunk is null"))?;
            stream.inner.feed(chunk);
            Ok(())
        })()),
        panicked()
    )
}

/// Read one server-sent event the caller already split from the stream: its
/// `event` field `event_name` (empty when it had none) and its `data` (the
/// JSON payload or `[DONE]`; may be empty). The event is read as
/// [`ddog_ai_usage_stream_feed`] reads it from the stream, and can be mixed
/// with it on one stream.
///
/// # Safety
///
/// `stream` must be a handle from [`ddog_ai_usage_stream_new`], not used by
/// another thread during the call, and `event_name` and `data` valid slices.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_stream_feed_event(
    stream: *mut Handle<Stream>,
    event_name: CharSlice,
    data: CharSlice,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        error_of((|| {
            // SAFETY: forwarded from the caller.
            let stream = unsafe { stream_of(stream) }?;
            let name = text(event_name).ok_or(Failure::Internal("event_name is not UTF-8"))?;
            let data = text(data).ok_or(Failure::Internal("data is not UTF-8"))?;
            stream.inner.feed_event(name, data);
            Ok(())
        })()),
        panicked()
    )
}

/// Write what the stream reported, as
/// [`ddog_ai_usage_parse_provider_response`] does, reading a last line that
/// had no line ending, and release the stream, taking its state rather than
/// copying it. The handle is left null, so the stream cannot be used
/// afterwards and a later [`ddog_ai_usage_stream_drop`] is a no-op; it is
/// released even when the call fails. A null or released stream is an
/// internal error.
///
/// # Safety
///
/// `stream` must be null or a handle from [`ddog_ai_usage_stream_new`], not
/// used by another thread during the call, and `out` null or as
/// [`ddog_ai_usage_parse_provider_response`] takes it.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_stream_take(
    mut stream: *mut Handle<Stream>,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        {
            // Released whether or not the output can be written.
            // SAFETY: forwarded from the caller.
            let taken = unsafe { stream.take() };
            // SAFETY: forwarded from the caller.
            unsafe {
                write_output(out, move || {
                    let stream = taken
                        .map_err(|_| Failure::Internal("the stream is null or was released"))?;
                    let inclusive = stream.inclusive;
                    report_text(stream.inner.finish(), inclusive)
                })
            }
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Release a stream and leave its handle null, so releasing it again is a
/// no-op. A null handle is a no-op.
///
/// # Safety
///
/// `stream` must be null or a handle from [`ddog_ai_usage_stream_new`], not
/// used by another thread during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_stream_drop(mut stream: *mut Handle<Stream>) {
    catch_panic!(
        if !stream.is_null() {
            // SAFETY: forwarded from the caller.
            drop(unsafe { stream.take() });
        },
        ()
    )
}

/// How a provider stream ended so far, as [`ddog_ai_usage_stream_outcome`]
/// reports it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No terminal event yet: a stream that ends here was cut short.
    Open = 0,
    /// The provider's terminal event arrived.
    Completed = 1,
    /// The provider reported a failure in the stream.
    Failed = 2,
}

/// How `stream` ended so far, without building any output: cheap enough to
/// call after every chunk, so a gateway needs no end-of-stream detection of
/// its own. A line still held without its line ending is not read (see
/// [`ddog_ai_usage_stream_take`]). A null or released stream is
/// [`Outcome::Open`].
///
/// # Safety
///
/// `stream` must be null or a handle from [`ddog_ai_usage_stream_new`], not
/// used by another thread during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_stream_outcome(stream: *const Handle<Stream>) -> Outcome {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        match unsafe { shared(stream) }.map(|stream| stream.inner.outcome()) {
            Some(StreamOutcome::Completed) => Outcome::Completed,
            Some(StreamOutcome::Failed) => Outcome::Failed,
            Some(StreamOutcome::Open) | None => Outcome::Open,
        },
        Outcome::Open
    )
}
