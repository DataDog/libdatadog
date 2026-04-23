// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for the OTel thread-level context publisher.
//!
//! All symbols are only available on Linux, since the TLSDESC TLS mechanism
//! required by the spec is Linux-specific.

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use libdd_otel_thread_ctx::linux::{ThreadContext, ThreadContextRecord};
    use std::ptr::NonNull;

    /// Allocate and initialise a new thread context.
    ///
    /// Returns a non-null owned handle that must eventually be released with
    /// `ddog_otel_thread_ctx_free`.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_new(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
    ) -> NonNull<ThreadContextRecord> {
        ThreadContext::new(*trace_id, *span_id, *local_root_span_id, &[]).into_ptr()
    }

    /// Free an owned thread context.
    ///
    /// # Safety
    ///
    /// `ctx` must be a valid non-null pointer obtained from `ddog_otel_thread_ctx_new` or
    /// `ddog_otel_thread_ctx_detach`, and must not be used after this call. In particular, `ctx`
    /// must not be currently attached to a thread.
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_free(ctx: *mut ThreadContextRecord) {
        if let Some(ctx) = NonNull::new(ctx) {
            let _ = ThreadContext::from_ptr(ctx);
        }
    }

    /// Attach `ctx` to the current thread. Returns the previously attached context if any, or null
    /// otherwise.
    ///
    /// # Safety
    ///
    /// `ctx` must be a valid non-null pointer obtained from this API. Ownership of `ctx` is
    /// transferred to the TLS slot: the caller must not drop `ctx` while it is still actively
    /// attached.
    ///
    /// ## In-place update
    ///
    /// The preferred method to update the thread context in place is [ddog_otel_thread_ctx_update].
    ///
    /// If calling into native code is too costly, it is possible to update an attached context
    /// directly in-memory without going through libdatadog (contexts are guaranteed to have a
    /// stable address through their lifetime). **HOWEVER, IF DOING SO, PLEASE BE VERY CAUTIOUS OF
    /// THE FOLLOWING POINTS**:
    ///
    /// 1. The update process requires a [seqlock](https://en.wikipedia.org/wiki/Seqlock)-like
    ///    pattern: [ThreadContextRecord::valid] must be first set to `0` before the update and set
    ///    to `1` again at the end. Additionally, depending on your language's memory model, you
    ///    might need specific synchronization primitives (compiler fences, atomics, etc.), since
    ///    the context can be read by an asynchronous signal handler at any point in time. See the
    ///    [Otel thread context
    ///    specification](https://github.com/open-telemetry/opentelemetry-specification/pull/4947)
    ///    for more details.
    /// 2. Only update the context from the thread it's attached to. Contexts are designed to be
    ///    attached, written to and read from on the same thread (whether from signal code or
    ///    program code). Thus, they are NOT thread-safe. Given the current specification, I don't
    ///    think it's possible to safely update an attached context from a different thread, since
    ///    the signal handler doesn't assume the context can be written to concurrently from another
    ///    thread.
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_attach(
        ctx: *mut ThreadContextRecord,
    ) -> Option<NonNull<ThreadContextRecord>> {
        ThreadContext::from_ptr(NonNull::new(ctx)?)
            .attach()
            .map(ThreadContext::into_ptr)
    }

    /// Remove the currently attached context from the TLS slot.
    ///
    /// Returns the detached context (caller now owns it and must release it with
    /// `ddog_otel_thread_ctx_free`), or null if the slot was empty.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_detach() -> Option<NonNull<ThreadContextRecord>> {
        ThreadContext::detach().map(ThreadContext::into_ptr)
    }

    /// Update the currently attached context in-place.
    ///
    /// If no context is currently attached, one is created and attached, equivalent to calling
    /// `ddog_otel_thread_ctx_new` followed by `ddog_otel_thread_ctx_attach`.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_update(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
    ) {
        ThreadContext::update(*trace_id, *span_id, *local_root_span_id, &[]);
    }
}
