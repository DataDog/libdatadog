// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for the OTel thread-level context publisher.
//!
//! All symbols are only available on Linux, since spec is currently Linux-specific.

#[cfg(target_os = "linux")]
pub use linux::*;

/// Verify that this binary was linked with the correct options such that the thread contexts are
/// visible to an external reader (typically the eBPF profiler).
///
/// Returns `VoidResult::Ok` if all checks pass, or a `VoidResult::Err` with a
/// diagnostic message on failure.
#[cfg(all(target_os = "linux", feature = "sanity-check"))]
#[no_mangle]
pub extern "C" fn ddog_otel_thread_ctx_sanity_check() -> libdd_common_ffi::VoidResult {
    match libdd_otel_thread_ctx::sanity_check::sanity_check() {
        Ok(()) => libdd_common_ffi::VoidResult::Ok,
        Err(e) => libdd_common_ffi::VoidResult::Err(libdd_common_ffi::Error::from(e)),
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use libdd_otel_thread_ctx::linux::{OwnedThreadContext, ThreadContext};
    use std::ptr::NonNull;

    /// Maximum size in bytes of the `attrs_data` field of a thread context record.
    // This is ugly, but I couldn't get cbindgen to generate the corresponding #define in any other
    // way. It doesn't like re-exports (pub use), and doing `pub const MAX_ATTRS_DATA_SIZE = _MAX`
    // (where `_MAX` has been imported properly) generates something dumb such as `#define
    // ddog_MAX_ATTRS_DATA_SIZE = _MAX` instead of propagating the actual value.
    // This solution is at least marginally better than prepending a hardcoded define manually in
    // build.rs, as it will at least keep the value in sync.
    pub const MAX_ATTRS_DATA_SIZE: usize = 612;
    const _: () = assert!(
        MAX_ATTRS_DATA_SIZE == libdd_otel_thread_ctx::linux::MAX_ATTRS_DATA_SIZE,
        "MAX_ATTRS_DATA_SIZE out of sync with libdd-otel-thread-ctx"
    );

    /// Allocate and initialise a new thread context.
    ///
    /// Returns a non-null owned handle that must eventually be released with
    /// `ddog_otel_thread_ctx_free`.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_new(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
    ) -> NonNull<ThreadContext> {
        OwnedThreadContext::new(*trace_id, *span_id, *local_root_span_id, &[]).into_opaque_ptr()
    }

    /// Free an owned thread context.
    ///
    /// # Safety
    ///
    /// `ctx` must be a valid non-null pointer obtained from `ddog_otel_thread_ctx_new` or
    /// `ddog_otel_thread_ctx_detach`, and must not be used after this call. In particular, `ctx`
    /// must not be currently attached to a thread.
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_free(ctx: *mut ThreadContext) {
        if let Some(ctx) = NonNull::new(ctx) {
            let _ = OwnedThreadContext::from_opaque_ptr(ctx);
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
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_attach(
        ctx: *mut ThreadContext,
    ) -> Option<NonNull<ThreadContext>> {
        OwnedThreadContext::from_opaque_ptr(NonNull::new(ctx)?)
            .attach()
            .map(OwnedThreadContext::into_opaque_ptr)
    }

    /// Remove the currently attached context from the TLS slot.
    ///
    /// Returns the detached context (caller now owns it and must release it with
    /// `ddog_otel_thread_ctx_free`), or null if the slot was empty.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_detach() -> Option<NonNull<ThreadContext>> {
        OwnedThreadContext::detach().map(OwnedThreadContext::into_opaque_ptr)
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
        OwnedThreadContext::update(*trace_id, *span_id, *local_root_span_id, &[]);
    }
}
