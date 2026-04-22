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
    /// `ddog_otel_thread_ctx_drop`, unless ownership is first transferred to the TLS slot
    /// via `ddog_otel_thread_ctx_attach`.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_new(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
    ) -> NonNull<ThreadContextRecord> {
        ThreadContext::new(*trace_id, *span_id, *local_root_span_id, &[]).into_raw()
    }

    /// Release an owned thread context.
    ///
    /// `ctx` must have been obtained from `ddog_otel_thread_ctx_new` or
    /// `ddog_otel_thread_ctx_detach`, and must not be used after this call.
    ///
    /// # Safety
    ///
    /// `ctx` must be a valid non-null pointer obtained from this API.
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_drop(ctx: *mut ThreadContextRecord) {
        if let Some(nn) = NonNull::new(ctx) {
            drop(ThreadContext::from_raw(nn));
        }
    }

    /// Publish `ctx` to the current thread's TLS slot.
    ///
    /// Ownership of `ctx` is transferred to the TLS slot; the caller must not use the
    /// pointer after this call. Returns the previously attached context (caller now owns it
    /// and must release it), or null if the slot was empty.
    ///
    /// # Safety
    ///
    /// `ctx` must be a valid non-null pointer obtained from this API.
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_attach(
        ctx: *mut ThreadContextRecord,
    ) -> Option<NonNull<ThreadContextRecord>> {
        let ctx = NonNull::new(ctx)?;
        ThreadContext::from_raw(ctx).attach().map(ThreadContext::into_raw)
    }

    /// Remove the currently attached context from the TLS slot.
    ///
    /// Returns the detached context (caller now owns it and must release it with
    /// `ddog_otel_thread_ctx_drop`), or null if the slot was empty.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_detach() -> Option<NonNull<ThreadContextRecord>> {
        ThreadContext::detach().map(ThreadContext::into_raw)
    }

    /// Update the currently attached context in-place.
    ///
    /// Avoids allocation in the common case. If no context is currently attached, one is
    /// created and attached, equivalent to calling `ddog_otel_thread_ctx_new` followed
    /// by `ddog_otel_thread_ctx_attach`.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_update(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
    ) {
        ThreadContext::update(*trace_id, *span_id, *local_root_span_id, &[]);
    }
}
