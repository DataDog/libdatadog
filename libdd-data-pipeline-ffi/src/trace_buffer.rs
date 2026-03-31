// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::response::ExporterResponse;
use crate::{catch_panic, gen_error};
use libdd_data_pipeline::trace_buffer::{
    Exporter, QueueMetrics, ResponseHandler, TraceBuffer, TraceBufferConfig, TraceBufferError,
    TraceChunk, TraceExporterWorker,
};
use libdd_data_pipeline::trace_exporter::{
    agent_response::AgentResponse, error::TraceExporterError, TraceExporter,
};
use libdd_shared_runtime::SharedRuntime;
use libdd_trace_utils::span::v04::SpanBytes;
use std::{ffi::c_void, fmt::Debug, pin::Pin, ptr::NonNull, sync::Arc, time::Duration};
use tracing::error;

// ─── FfiSpanChunk ────────────────────────────────────────────────────────────

/// Opaque handle holding the decoded spans of one trace.
///
/// Must be freed with [`ddog_trace_buffer_chunk_free`] unless it is consumed by
/// [`ddog_trace_buffer_send_chunk`].
pub struct FfiSpanChunk(pub Box<Vec<SpanBytes>>);

/// Free a [`FfiSpanChunk`] that was not sent to the buffer.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_chunk_free(_chunk: Box<FfiSpanChunk>) {}

// ─── FfiExporter ─────────────────────────────────────────────────────────────

#[derive(Debug)]
struct FfiExporter;

impl Exporter<SpanBytes> for FfiExporter {
    fn trace_chunks<'a>(
        &'a mut self,
        chunks: Vec<TraceChunk<SpanBytes>>,
        te: &TraceExporter,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + 'a>>
    {
        // SAFETY: The caller (TraceExporterWorker::export_trace_chunks) always awaits the
        // returned future before either `self` or `te` go away, so `te` is valid for at least
        // as long as `'a`. We transmute the lifetime to express this to the type system.
        let te: &'a TraceExporter = unsafe { std::mem::transmute(te) };
        Box::pin(async move {
            if chunks.is_empty() {
                return Ok(AgentResponse::Unchanged);
            }
            te.send_trace_chunks_async(chunks).await
        })
    }
}

// ─── Response callback ───────────────────────────────────────────────────────

/// C callback invoked after each flush attempt.
///
/// Exactly one of `error` or `response` is non-null per call:
/// - on success `error` is null and `response` points to a valid [`ExporterResponse`];
/// - on failure `response` is null and `error` points to a valid [`ExporterError`].
///
/// Both pointers are only valid for the duration of the callback.
pub type FfiResponseHandlerFn = unsafe extern "C" fn(
    error: Option<&ExporterError>,
    response: Option<&ExporterResponse>,
    user_data: *mut c_void,
);

struct CallbackData {
    callback: FfiResponseHandlerFn,
    user_data: *mut c_void,
}

// SAFETY: the C caller guarantees that `user_data` (and any data it refers to) is safe to
// use from any thread for the lifetime of the buffer.
unsafe impl Send for CallbackData {}
unsafe impl Sync for CallbackData {}

fn make_response_handler(callback: FfiResponseHandlerFn, user_data: *mut c_void) -> ResponseHandler {
    // Box the data so that the closure captures the Box (a pointer), which prevents the Rust 2021
    // edition precise-capture analysis from splitting out the raw `*mut c_void` field.
    let cb = Box::new(CallbackData { callback, user_data });
    Box::new(move |result: Result<AgentResponse, TraceExporterError>| match result {
        Ok(response) => {
            let ffi_response = ExporterResponse::from(response);
            // SAFETY: callback is a valid C function pointer.
            unsafe { (cb.callback)(None, Some(&ffi_response), cb.user_data) };
        }
        Err(err) => {
            let ffi_err = ExporterError::from(err);
            // SAFETY: callback is a valid C function pointer.
            unsafe { (cb.callback)(Some(&ffi_err), None, cb.user_data) };
        }
    })
}

// ─── TraceBufferConfig FFI ───────────────────────────────────────────────────

/// Allocate a [`TraceBufferConfig`] with default values.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_config_new(
    out_handle: NonNull<Box<TraceBufferConfig>>,
) {
    catch_panic!(
        out_handle
            .as_ptr()
            .write(Box::<TraceBufferConfig>::default()),
        ()
    )
}

/// Free a [`TraceBufferConfig`].
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_config_free(_handle: Box<TraceBufferConfig>) {}

/// Enable or disable synchronous writes.
///
/// When enabled, [`ddog_trace_buffer_send_chunk`] blocks until the chunk is flushed (or the
/// timeout expires).
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_config_set_synchronous_writes(
    config: Option<&mut TraceBufferConfig>,
    value: bool,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            let new = (*handle).synchronous_writes(value);
            *handle = new;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the maximum time [`ddog_trace_buffer_send_chunk`] will wait for a flush in synchronous
/// mode.
///
/// A value of `0` disables the timeout (waits indefinitely).
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_config_set_synchronous_writes_timeout(
    config: Option<&mut TraceBufferConfig>,
    timeout_ms: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            let timeout = if timeout_ms == 0 {
                None
            } else {
                Some(Duration::from_millis(timeout_ms))
            };
            let new = (*handle).synchronous_writes_timeout(timeout);
            *handle = new;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the maximum interval between two automatic flushes (in milliseconds).
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_config_set_max_flush_interval(
    config: Option<&mut TraceBufferConfig>,
    interval_ms: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            let new = (*handle).max_flush_interval(Duration::from_millis(interval_ms));
            *handle = new;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the maximum number of spans that may be buffered before new chunks are dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_config_set_max_buffered_spans(
    config: Option<&mut TraceBufferConfig>,
    max: usize,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            let new = (*handle).max_buffered_spans(max);
            *handle = new;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the number of buffered spans that triggers an early flush.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_config_set_span_flush_threshold(
    config: Option<&mut TraceBufferConfig>,
    threshold: usize,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            let new = (*handle).span_flush_threshold(threshold);
            *handle = new;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

// ─── FfiTraceBuffer ──────────────────────────────────────────────────────────

/// C-visible metrics snapshot from a [`FfiTraceBuffer`].
#[repr(C)]
pub struct FfiTraceBufferMetrics {
    /// Number of spans dropped because the buffer was full.
    pub spans_dropped: usize,
    /// Number of spans currently queued.
    pub spans_queued: usize,
}

impl From<QueueMetrics> for FfiTraceBufferMetrics {
    fn from(m: QueueMetrics) -> Self {
        Self {
            spans_dropped: m.spans_dropped_full_buffer,
            spans_queued: m.spans_queued,
        }
    }
}

/// Buffered trace exporter.
///
/// Accumulates trace chunks and forwards them to the agent via a background worker.
///
/// Must be freed with [`ddog_trace_buffer_free`].
pub struct FfiTraceBuffer {
    inner: TraceBuffer<SpanBytes>,
}

fn buffer_error_to_ffi(e: TraceBufferError) -> Box<ExporterError> {
    match e {
        TraceBufferError::AlreadyShutdown => {
            Box::new(ExporterError::new(ErrorCode::Shutdown, "Buffer already shutdown"))
        }
        TraceBufferError::TimedOut(d) => Box::new(ExporterError::new(
            ErrorCode::TimedOut,
            &format!("Operation timed out after {d:?}"),
        )),
        TraceBufferError::MutexPoisoned => {
            Box::new(ExporterError::new(ErrorCode::Internal, "Mutex was poisoned"))
        }
        TraceBufferError::BatchFull(e) => Box::new(ExporterError::new(
            ErrorCode::BufferFull,
            &format!("Buffer full: {e:?}"),
        )),
        TraceBufferError::TraceExporter(e) => Box::new(ExporterError::from(e)),
    }
}

/// Create a new [`FfiTraceBuffer`].
///
/// # Arguments
///
/// * `out_handle` – Receives the newly created buffer handle.
/// * `config` – Buffer configuration produced by [`ddog_trace_buffer_config_new`]. May be null,
///   in which case defaults are used.
/// * `trace_exporter` – The transport used to send spans to the agent. Ownership is transferred;
///   the caller must not free it afterwards.
/// * `response_handler` – C callback invoked after every flush.
/// * `user_data` – Opaque pointer forwarded to every `response_handler` invocation.
/// * `shared_runtime` – Runtime on which the background worker is spawned. The caller retains
///   ownership; its lifetime must exceed that of the buffer.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_new(
    out_handle: NonNull<Box<FfiTraceBuffer>>,
    config: Option<&TraceBufferConfig>,
    trace_exporter: Box<TraceExporter>,
    response_handler: FfiResponseHandlerFn,
    user_data: *mut c_void,
    shared_runtime: Option<NonNull<SharedRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        {
            let rt_ptr = match shared_runtime {
                Some(p) => p,
                None => return gen_error!(ErrorCode::InvalidArgument),
            };

            // Reconstruct an Arc without consuming the caller's reference.
            // SAFETY: rt_ptr was produced by Arc::into_raw and the Arc is still alive.
            Arc::increment_strong_count(rt_ptr.as_ptr());
            let rt: Arc<SharedRuntime> = Arc::from_raw(rt_ptr.as_ptr());

            let buf_config = config.copied().unwrap_or_default();
            let handler = make_response_handler(response_handler, user_data);

            let (buffer, worker): (TraceBuffer<SpanBytes>, TraceExporterWorker<SpanBytes>) =
                TraceBuffer::new(buf_config, handler, Box::new(FfiExporter), *trace_exporter);

            if let Err(e) = rt.spawn_worker(worker) {
                return Some(Box::new(ExporterError::new(
                    ErrorCode::Internal,
                    &e.to_string(),
                )));
            }

            out_handle
                .as_ptr()
                .write(Box::new(FfiTraceBuffer { inner: buffer }));
            None
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Free a [`FfiTraceBuffer`].
///
/// The caller should have shut down the associated [`SharedRuntime`] (or the background worker)
/// before calling this function to ensure a clean shutdown.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_free(_handle: Box<FfiTraceBuffer>) {}

/// Add a trace chunk to the buffer.
///
/// The `chunk` is consumed regardless of whether the call succeeds. The caller must not free it
/// afterwards.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_send_chunk(
    handle: Option<&FfiTraceBuffer>,
    chunk: Box<FfiSpanChunk>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(buf) = handle {
            match buf.inner.send_chunk(*chunk.0) {
                Ok(()) => None,
                Err(e) => Some(buffer_error_to_ffi(e)),
            }
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Trigger an immediate flush without waiting for it to complete.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_force_flush(
    handle: Option<&FfiTraceBuffer>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(buf) = handle {
            match buf.inner.force_flush() {
                Ok(()) => None,
                Err(e) => Some(buffer_error_to_ffi(e)),
            }
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Write a metrics snapshot to `out_metrics`.
///
/// If `handle` is null the output is zeroed.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_get_metrics(
    handle: Option<&FfiTraceBuffer>,
    out_metrics: NonNull<FfiTraceBufferMetrics>,
) {
    catch_panic!(
        {
            let metrics: FfiTraceBufferMetrics = match handle {
                Some(buf) => buf.inner.queue_metrics().get_metrics().into(),
                None => FfiTraceBufferMetrics {
                    spans_dropped: 0,
                    spans_queued: 0,
                },
            };
            out_metrics.as_ptr().write(metrics);
        },
        ()
    )
}

/// Block until the background worker has acknowledged shutdown or `timeout_ms` elapses.
///
/// A `timeout_ms` of `0` returns [`ExporterErrorCode::TimedOut`] immediately.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_buffer_wait_shutdown_done(
    handle: Option<&FfiTraceBuffer>,
    timeout_ms: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(buf) = handle {
            match buf
                .inner
                .wait_shutdown_done(Duration::from_millis(timeout_ms))
            {
                Ok(()) => None,
                Err(e) => Some(buffer_error_to_ffi(e)),
            }
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn config_new_and_free() {
        let mut out: MaybeUninit<Box<TraceBufferConfig>> = MaybeUninit::uninit();
        unsafe {
            ddog_trace_buffer_config_new(NonNull::new(out.as_mut_ptr()).unwrap());
            let handle = out.assume_init();
            ddog_trace_buffer_config_free(handle);
        }
    }

    #[test]
    fn config_setters_null_handle_returns_error() {
        let err = unsafe { ddog_trace_buffer_config_set_synchronous_writes(None, true) };
        assert!(err.is_some());
        assert_eq!(err.unwrap().code, ErrorCode::InvalidArgument);

        let err = unsafe { ddog_trace_buffer_config_set_max_buffered_spans(None, 100) };
        assert!(err.is_some());
        assert_eq!(err.unwrap().code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn config_setters_apply() {
        let mut out: MaybeUninit<Box<TraceBufferConfig>> = MaybeUninit::uninit();
        unsafe {
            ddog_trace_buffer_config_new(NonNull::new(out.as_mut_ptr()).unwrap());
            let mut handle = out.assume_init();
            let err = ddog_trace_buffer_config_set_synchronous_writes(Some(&mut *handle), true);
            assert!(err.is_none());
            let err = ddog_trace_buffer_config_set_max_flush_interval(Some(&mut *handle), 500);
            assert!(err.is_none());
            let err = ddog_trace_buffer_config_set_max_buffered_spans(Some(&mut *handle), 5000);
            assert!(err.is_none());
            let err =
                ddog_trace_buffer_config_set_span_flush_threshold(Some(&mut *handle), 1000);
            assert!(err.is_none());
            ddog_trace_buffer_config_free(handle);
        }
    }
}
