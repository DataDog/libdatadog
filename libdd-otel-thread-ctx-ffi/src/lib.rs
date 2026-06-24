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
    use libdd_otel_thread_ctx::linux::{
        ThreadContext, ThreadContextHandle as InternalThreadContextHandle,
        ThreadContextRecord as InternalThreadContextRecord,
    };
    use std::{mem, ptr::NonNull, slice};

    /// Opaque handle to an owned thread context record. Used to allow the FFI to convert
    /// [ThreadContext] to and from raw pointers without exposing Rust ownership details.
    ///
    /// This intentionally mirrors, rather than re-exports,
    /// `libdd_otel_thread_ctx::linux::ThreadContextHandle`: cbindgen only sees this crate and must
    /// emit a self-contained C header.
    #[repr(C)]
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

    /// In-memory layout of a thread-level context.
    ///
    /// This is the C-facing mirror of `libdd_otel_thread_ctx::linux::ThreadContextRecord`.
    /// cbindgen must not emit the internal Rust type directly: the Rust implementation owns the
    /// synchronization details, while C consumers need the exact OTel ABI layout as primitive
    /// fields. The const assertions below keep this mirror tied to the internal record layout.
    ///
    /// **CAUTION**: The structure MUST match exactly the OTel thread-level context specification.
    /// It is read by external, out-of-process code. Do not re-order fields or modify in any way,
    /// unless you know exactly what you're doing.
    ///
    /// # Synchronization
    ///
    /// Readers are async-signal handlers. The writer is always stopped while a reader runs.
    /// Sharing memory with a signal handler still requires some form of synchronization, which is
    /// achieved through atomics and compiler fence in the Rust implementation, using `valid` and/or
    /// the TLS slot as synchronization points.
    ///
    /// - The writer stores `valid = 0` *before* modifying fields in-place, guarded by a fence.
    /// - The writer stores `valid = 1` *after* all fields are populated, guarded by a fence.
    /// - `valid` starts at `1` on construction and is never set to `0` except during an in-place
    ///   update.
    #[repr(C)]
    pub struct ThreadContextRecord {
        /// Trace identifier; all-zeroes means "no trace".
        pub trace_id: [u8; 16],
        /// Span identifier, stored with the exact byte representation provided by the caller.
        pub span_id: u64,
        /// Whether the record is ready/consistent. Always set to `1` except during in-place update
        /// of the current record.
        pub valid: u8,
        pub _reserved: u8,
        /// Number of populated bytes in `attrs_data`.
        pub attrs_data_size: u16,
        /// Packed variable-length key-value records.
        ///
        /// It's a contiguous list of blocks with layout:
        ///
        /// 1. 1-byte `key_index`
        /// 2. 1-byte `val_len`
        /// 3. `val_len` bytes of a string value.
        ///
        /// # Size
        ///
        /// Currently, we always allocate the max recommended size. This potentially wastes a few
        /// hundred bytes per thread, but it guarantees that we can modify the context in-place
        /// without (re)allocation in the hot path.
        pub attrs_data: [u8; MAX_ATTRS_DATA_SIZE],
    }

    const _: () = {
        assert!(size_of::<ThreadContextRecord>() == size_of::<InternalThreadContextRecord>());
        assert!(
            mem::align_of::<ThreadContextRecord>()
                == mem::align_of::<InternalThreadContextRecord>()
        );
        assert!(mem::offset_of!(ThreadContextRecord, trace_id) == 0);
        assert!(mem::offset_of!(ThreadContextRecord, span_id) == 16);
        assert!(mem::offset_of!(ThreadContextRecord, valid) == 24);
        assert!(mem::offset_of!(ThreadContextRecord, _reserved) == 25);
        assert!(mem::offset_of!(ThreadContextRecord, attrs_data_size) == 26);
        assert!(mem::offset_of!(ThreadContextRecord, attrs_data) == 28);
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
        let Some(ctx) = ctx.cast::<InternalThreadContextRecord>().as_mut() else {
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
        let Some(ctx) = ctx.cast::<InternalThreadContextRecord>().as_mut() else {
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
        let Some(ctx) = NonNull::new(ctx.cast::<InternalThreadContextRecord>()) else {
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
