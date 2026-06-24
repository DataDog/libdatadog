// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for the OTel thread-level context publisher.
//!
//! All symbols are only available on Linux, since spec is currently Linux-specific.

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use libdd_common_ffi::slice::{AsBytes, CharSlice};
    pub use libdd_otel_thread_ctx::linux::ThreadContextRecord;
    use libdd_otel_thread_ctx::linux::{
        ThreadContext, ThreadContextHandle as InternalThreadContextHandle,
    };
    use std::{mem, ptr::NonNull, slice};

    /// Opaque handle to an owned thread context record. Used to allow the FFI to convert
    /// [ThreadContext] to and from raw pointers without exposing Rust ownership details.
    ///
    /// This is intentionally not `repr(C)`: C only ever sees pointers to this token, and cbindgen
    /// emits it as an opaque forward declaration. The public cross-process layout is
    /// `ThreadContextRecord`, not this ownership handle.
    pub struct ThreadContextHandle {}

    #[repr(C)]
    pub struct OtelThreadContextAttribute<'a> {
        pub key_index: u8,
        pub value: CharSlice<'a>,
    }

    unsafe fn attrs_from_raw<'a>(
        attrs: *const OtelThreadContextAttribute<'a>,
        attrs_len: usize,
    ) -> &'a [OtelThreadContextAttribute<'a>] {
        if attrs.is_null() || attrs_len == 0 {
            return &[];
        }

        slice::from_raw_parts(attrs, attrs_len)
    }

    fn attrs_iter<'a>(
        attrs: &'a [OtelThreadContextAttribute<'a>],
    ) -> impl Iterator<Item = (u8, &'a str)> {
        attrs.iter().filter_map(|attr| {
            attr.value
                .try_to_utf8()
                .ok()
                .map(|value| (attr.key_index, value))
        })
    }

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

    const _: () = {
        assert!(size_of::<ThreadContextRecord>() == 640);
        assert!(mem::align_of::<ThreadContextRecord>() == 2);
    };

    /// Allocate and initialise a new thread context.
    ///
    /// Returns a non-null owned handle that must eventually be released with
    /// `ddog_otel_thread_ctx_free`.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_new(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
    ) -> NonNull<ThreadContextHandle> {
        ThreadContext::new(*trace_id, *span_id, *local_root_span_id, &[])
            .into_opaque_ptr()
            .cast()
    }

    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_new_with_attrs(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
        attrs: *const OtelThreadContextAttribute<'_>,
        attrs_len: usize,
    ) -> NonNull<ThreadContextHandle> {
        let attrs = attrs_from_raw(attrs, attrs_len);
        ThreadContext::new_with_attrs(*trace_id, *span_id, *local_root_span_id, attrs_iter(attrs))
            .into_opaque_ptr()
            .cast()
    }

    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_record_update(
        ctx: *mut ThreadContextRecord,
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
        attrs: *const OtelThreadContextAttribute<'_>,
        attrs_len: usize,
    ) -> bool {
        let Some(ctx) = ctx.as_mut() else {
            return false;
        };
        let attrs = attrs_from_raw(attrs, attrs_len);
        ctx.update(*trace_id, *span_id, *local_root_span_id, attrs_iter(attrs))
    }

    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_record_update_span_id(
        ctx: *mut ThreadContextRecord,
        span_id: &[u8; 8],
    ) -> bool {
        let Some(ctx) = ctx.as_mut() else {
            return false;
        };
        ctx.update_span_id(*span_id);
        true
    }

    /// Free an owned thread context.
    ///
    /// # Safety
    ///
    /// `ctx` must be a valid non-null pointer obtained from `ddog_otel_thread_ctx_new` or
    /// `ddog_otel_thread_ctx_detach`, and must not be used after this call. In particular, `ctx`
    /// must not be currently attached to a thread.
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_free(ctx: *mut ThreadContextHandle) {
        if let Some(ctx) = NonNull::new(ctx.cast::<InternalThreadContextHandle>()) {
            let _ = ThreadContext::from_opaque_ptr(ctx);
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
        ctx: *mut ThreadContextHandle,
    ) -> Option<NonNull<ThreadContextHandle>> {
        ThreadContext::from_opaque_ptr(NonNull::new(ctx.cast::<InternalThreadContextHandle>())?)
            .attach()
            .map(ThreadContext::into_opaque_ptr)
            .map(NonNull::cast)
    }

    /// Attach an externally owned record to the current thread without taking ownership.
    ///
    /// # Safety
    ///
    /// `ctx` must point to a live record that remains allocated until it is detached.
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_attach_record(ctx: *mut ThreadContextRecord) {
        let Some(ctx) = NonNull::new(ctx) else {
            return;
        };
        ThreadContext::attach_record(ctx);
    }

    /// Remove the currently attached context from the TLS slot.
    ///
    /// Returns the detached context (caller now owns it and must release it with
    /// `ddog_otel_thread_ctx_free`), or null if the slot was empty.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_detach() -> Option<NonNull<ThreadContextHandle>> {
        ThreadContext::detach()
            .map(ThreadContext::into_opaque_ptr)
            .map(NonNull::cast)
    }

    /// Clear the current thread's context slot without taking ownership of the previous record.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_detach_record() {
        ThreadContext::detach_record();
    }

    /// Clear the current thread's context slot if it currently points to `ctx`.
    ///
    /// Returns true when the slot was cleared.
    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_detach_record_if_current(
        ctx: *mut ThreadContextRecord,
    ) -> bool {
        let Some(ctx) = NonNull::new(ctx) else {
            return false;
        };
        ThreadContext::detach_record_if_current(ctx.cast())
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

    #[no_mangle]
    pub unsafe extern "C" fn ddog_otel_thread_ctx_update_with_attrs(
        trace_id: &[u8; 16],
        span_id: &[u8; 8],
        local_root_span_id: &[u8; 8],
        attrs: *const OtelThreadContextAttribute<'_>,
        attrs_len: usize,
    ) {
        let attrs = attrs_from_raw(attrs, attrs_len);
        ThreadContext::update_with_attrs(
            *trace_id,
            *span_id,
            *local_root_span_id,
            attrs_iter(attrs),
        );
    }
}
