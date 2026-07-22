// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for the OTel thread-level context publisher.
//!
//! Record layout and explicit record operations are platform-neutral. Linux additionally exposes
//! the libdatadog-owned TLS convenience API.

use libdd_otel_thread_ctx::ThreadContextRecord;

#[repr(C)]
pub struct OtelThreadContextAttribute<'a> {
    pub key_index: u8,
    pub value: libdd_common_ffi::slice::CharSlice<'a>,
}

unsafe fn attrs_from_raw<'a>(
    attrs: *const OtelThreadContextAttribute<'a>,
    attrs_len: usize,
) -> &'a [OtelThreadContextAttribute<'a>] {
    if attrs.is_null() || attrs_len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(attrs, attrs_len) }
    }
}

fn attrs_iter<'a>(
    attrs: &'a [OtelThreadContextAttribute<'a>],
) -> impl Iterator<Item = (u8, &'a str)> {
    use libdd_common_ffi::slice::AsBytes;
    attrs.iter().filter_map(|attr| {
        attr.value
            .try_to_utf8()
            .ok()
            .map(|value| (attr.key_index, value))
    })
}

/// Initialize a caller-owned thread-context record.
///
/// # Safety
///
/// `ctx` must point to writable storage for a `ThreadContextRecord`. The identifier pointers and
/// `attrs..attrs + attrs_len` must be valid for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_thread_ctx_record_init(
    ctx: *mut ThreadContextRecord,
    trace_id: &[u8; 16],
    span_id: &[u8; 8],
    local_root_span_id: &[u8; 8],
    attrs: *const OtelThreadContextAttribute<'_>,
    attrs_len: usize,
) -> bool {
    if ctx.is_null() {
        return false;
    }
    let attrs = unsafe { attrs_from_raw(attrs, attrs_len) };
    unsafe { ctx.write(ThreadContextRecord::default()) };
    unsafe { &mut *ctx }.initialize(*trace_id, *span_id, *local_root_span_id, attrs_iter(attrs))
}

/// Replace all fields in a caller-owned thread-context record.
///
/// # Safety
///
/// `ctx` must point to a live, writable `ThreadContextRecord`. The identifier pointers and
/// `attrs..attrs + attrs_len` must be valid for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_thread_ctx_record_update(
    ctx: *mut ThreadContextRecord,
    trace_id: &[u8; 16],
    span_id: &[u8; 8],
    local_root_span_id: &[u8; 8],
    attrs: *const OtelThreadContextAttribute<'_>,
    attrs_len: usize,
) -> bool {
    let Some(ctx) = (unsafe { ctx.as_mut() }) else {
        return false;
    };
    let attrs = unsafe { attrs_from_raw(attrs, attrs_len) };
    ctx.update(*trace_id, *span_id, *local_root_span_id, attrs_iter(attrs))
}

/// Replace the span identifier in a caller-owned thread-context record.
///
/// # Safety
///
/// `ctx` must point to a live, writable `ThreadContextRecord`, and `span_id` must be valid for the
/// duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ddog_otel_thread_ctx_record_update_span_id(
    ctx: *mut ThreadContextRecord,
    span_id: &[u8; 8],
) -> bool {
    let Some(ctx) = (unsafe { ctx.as_mut() }) else {
        return false;
    };
    ctx.update_span_id(*span_id);
    true
}

#[cfg(test)]
mod record_tests {
    use super::*;
    use core::mem::{size_of, MaybeUninit};

    #[test]
    fn ffi_initializes_caller_owned_storage() {
        let mut record = MaybeUninit::<ThreadContextRecord>::uninit();
        let trace_id = [1; 16];
        let span_id = [2; 8];
        let local_root_span_id = [3; 8];
        let attrs = [OtelThreadContextAttribute {
            key_index: 1,
            value: "service".into(),
        }];

        assert!(unsafe {
            ddog_otel_thread_ctx_record_init(
                record.as_mut_ptr(),
                &trace_id,
                &span_id,
                &local_root_span_id,
                attrs.as_ptr(),
                attrs.len(),
            )
        });
        let record = unsafe { record.assume_init() };
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (&raw const record).cast::<u8>(),
                size_of::<ThreadContextRecord>(),
            )
        };
        assert_eq!(&bytes[..16], &trace_id);
        assert_eq!(&bytes[16..24], &span_id);
        assert_eq!(bytes[24], 1);
        assert_eq!(&bytes[48..55], b"service");
    }
}

#[cfg(all(target_os = "linux", feature = "tls-storage"))]
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

#[cfg(all(target_os = "linux", feature = "tls-storage"))]
mod linux {
    use libdd_otel_thread_ctx::linux::{ThreadContext, ThreadContextHandle};
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
    ) -> NonNull<ThreadContextHandle> {
        ThreadContext::new(*trace_id, *span_id, *local_root_span_id, &[]).into_opaque_ptr()
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
        if let Some(ctx) = NonNull::new(ctx) {
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
        ThreadContext::from_opaque_ptr(NonNull::new(ctx)?)
            .attach()
            .map(ThreadContext::into_opaque_ptr)
    }

    /// Remove the currently attached context from the TLS slot.
    ///
    /// Returns the detached context (caller now owns it and must release it with
    /// `ddog_otel_thread_ctx_free`), or null if the slot was empty.
    #[no_mangle]
    pub extern "C" fn ddog_otel_thread_ctx_detach() -> Option<NonNull<ThreadContextHandle>> {
        ThreadContext::detach().map(ThreadContext::into_opaque_ptr)
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
