// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Thread contexts owned by a single thread, provided by the `owned-context` feature.

use std::{
    mem,
    ptr::NonNull,
    sync::atomic::{compiler_fence, Ordering},
};

use super::{with_tls_slot, ThreadContext, ThreadContextRecord};

impl ThreadContextRecord {
    /// Update the record in-place. Sets `valid=0` before the update and `valid=1` after, so a
    /// reader that fires between the two writes sees an inconsistent record and skips it. Compiler
    /// fences prevent the compiler from reordering field writes outside that window.
    fn update_in_place(
        &mut self,
        trace_id: [u8; 16],
        span_id: [u8; 8],
        trace_flags: u8,
        local_root_span_id: [u8; 8],
        attrs: &[(u8, &str)],
    ) -> bool {
        self.valid.store(0, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);

        self.trace_id = trace_id;
        self.span_id = span_id;
        self.trace_flags = trace_flags;
        let fully_encoded = self.set_attrs(local_root_span_id, attrs);

        compiler_fence(Ordering::SeqCst);
        self.valid.store(1, Ordering::Relaxed);

        fully_encoded
    }
}

/// An owned (and non-moving) thread context record allocation.
///
/// We don't use `Box` under the hood because it precludes aliasing, while we share the context to
/// readers through thread-level context and through the FFI. But it is a boxed
/// `ThreadContextRecord` for all intent and purpose.
///
/// Since an owned context can be modified in place, it is `!Send` and `!Sync`. Readers rely on the
/// fact that their can't be any writer while they interrupt the current thread, but this wouldn't
/// be true anymore if we moved `OwnedThreadContext` to a different thread. It is thus not
/// thread-safe.
pub struct OwnedThreadContext(NonNull<ThreadContextRecord>);

impl OwnedThreadContext {
    /// Create a new thread context with the given trace/span IDs, W3C trace-flags byte, and
    /// encoded attributes.
    #[inline]
    pub fn new(
        trace_id: [u8; 16],
        span_id: [u8; 8],
        trace_flags: u8,
        local_root_span_id: [u8; 8],
        attrs: &[(u8, &str)],
    ) -> Self {
        Self::from(ThreadContextRecord::new(
            trace_id,
            span_id,
            trace_flags,
            local_root_span_id,
            attrs,
        ))
    }

    /// Turn this thread context into a pointer to the underlying [`ThreadContextRecord`].
    /// The pointer must be reconstructed through [`Self::from_ptr`] in order to be properly
    /// dropped, or the record will leak.
    fn into_ptr(self) -> NonNull<ThreadContextRecord> {
        let mdrop = mem::ManuallyDrop::new(self);
        mdrop.0
    }

    /// Turn this thread context into an opaque pointer to the underlying [`ThreadContext`].
    /// The pointer must be reconstructed through [`Self::from_opaque_ptr`] in order to be
    /// properly dropped, or the record will leak.
    #[inline]
    pub fn into_opaque_ptr(self) -> NonNull<ThreadContext> {
        let mdrop = mem::ManuallyDrop::new(self);
        mdrop.0.cast()
    }

    /// Reconstruct an [`OwnedThreadContext`] from a pointer that comes from
    /// [`Self::into_ptr`].
    ///
    /// # Safety
    ///
    /// - `ptr` must come from a prior call to [`Self::into_ptr`].
    /// - if `ptr` is aliased, accesses through aliases must not be interleaved with method calls on
    ///   the returned [`OwnedThreadContext`]. More precisely, mutable references might be
    ///   reconstructed during those calls, so any constraint from either Stacked Borrows, Tree
    ///   Borrows or whatever is the current aliasing model implemented in Miri applies.
    #[inline]
    unsafe fn from_ptr(ptr: NonNull<ThreadContextRecord>) -> Self {
        Self(ptr)
    }

    /// Reconstruct an [`OwnedThreadContext`] from a pointer that comes from
    /// [`Self::into_opaque_ptr`].
    ///
    /// # Safety
    ///
    /// - `ptr` must come from a prior call to [`Self::into_opaque_ptr`].
    /// - if `ptr` is aliased, accesses through aliases must not be interleaved with method calls on
    ///   the returned [`OwnedThreadContext`]. More precisely, mutable references might be
    ///   reconstructed during those calls, so any constraint from either Stacked Borrows, Tree
    ///   Borrows or whatever is the current aliasing model implemented in Miri applies.
    #[inline]
    pub unsafe fn from_opaque_ptr(ptr: NonNull<ThreadContext>) -> Self {
        Self(ptr.cast())
    }

    /// Publish a new (or previously detached) thread context record by writing its pointer
    /// into the TLS slot. Returns the previously attached context, if any.
    ///
    /// `valid` is already `1` since construction, so any reader that observes the new pointer
    /// also observes `valid = 1`.
    #[inline]
    pub fn attach(self) -> Option<OwnedThreadContext> {
        let prev = ThreadContextRecord::attach_raw(self.into_ptr().as_ptr());
        // Safety: a non-null value in the slot came from a prior `into_ptr` call.
        NonNull::new(prev).map(|ptr| unsafe { OwnedThreadContext::from_ptr(ptr) })
    }

    /// Update `target` and make it the current thread's context.
    ///
    /// If `target` is already current, its pointer remains unchanged and no previous
    /// context is returned. Otherwise, the updated target is published and the
    /// different previously attached context is returned.
    ///
    /// # Safety
    ///
    /// - `target` must originate from [`Self::into_opaque_ptr`] and remain live for the duration of
    ///   the call.
    /// - If `target` is attached when this function is called, it must be attached only to the
    ///   calling native thread.
    /// - It must not be concurrently updated or freed.
    pub unsafe fn update_and_attach(
        target: NonNull<ThreadContext>,
        trace_id: [u8; 16],
        span_id: [u8; 8],
        trace_flags: u8,
        local_root_span_id: [u8; 8],
        attrs: &[(u8, &str)],
    ) -> Option<OwnedThreadContext> {
        let mut target = target.cast::<ThreadContextRecord>();

        with_tls_slot(|slot| {
            let is_current = slot.load(Ordering::Relaxed) == target.as_ptr();

            // Safety: the caller guarantees that `target` is live and exclusively
            // writable by this thread.
            let target_record = unsafe { target.as_mut() };
            let _ = target_record.update_in_place(
                trace_id,
                span_id,
                trace_flags,
                local_root_span_id,
                attrs,
            );

            if is_current {
                None
            } else {
                let prev = ThreadContextRecord::attach_raw(target.as_ptr());
                // Safety: a non-null value in the slot came from a prior `into_ptr` call.
                NonNull::new(prev).map(|ptr| unsafe { OwnedThreadContext::from_ptr(ptr) })
            }
        })
    }

    /// Update the currently attached record in-place. Sets `valid = 0` before the update and
    /// `valid = 1` after, so a reader that fires between the two writes sees an inconsistent
    /// record and skips it. Compiler fences prevent the compiler from reordering field writes
    /// outside that window.
    ///
    /// If there's currently no attached context, `update` will create one, and is in this case
    /// equivalent to
    /// `OwnedThreadContext::new(trace_id, span_id, trace_flags, local_root_span_id,
    /// attrs).attach()`.
    pub fn update(
        trace_id: [u8; 16],
        span_id: [u8; 8],
        trace_flags: u8,
        local_root_span_id: [u8; 8],
        attrs: &[(u8, &str)],
    ) {
        with_tls_slot(|slot| {
            // Safety: a non-null value in the slot came from `into_ptr` (i.e. `Box::into_raw`),
            // and only this thread ever writes to the slot, so the pointer is valid and not
            // accessed for the duration of this closure.
            if let Some(current) = unsafe { slot.load(Ordering::Relaxed).as_mut() } {
                current.update_in_place(trace_id, span_id, trace_flags, local_root_span_id, attrs);
            } else {
                let ctxt = OwnedThreadContext::new(
                    trace_id,
                    span_id,
                    trace_flags,
                    local_root_span_id,
                    attrs,
                )
                .into_ptr()
                .as_ptr();
                // No need for `AcqRel`, see [^tls-slot-ordering].
                compiler_fence(Ordering::Release);
                // The slot was null; publish the freshly created record, which
                // `OwnedThreadContext::new` already initialises with `valid = 1`.
                slot.store(ctxt, Ordering::Relaxed);
            }
        })
    }

    /// Detach the current record from the TLS slot. Writes null to the slot and returns the
    /// detached record.
    pub fn detach() -> Option<OwnedThreadContext> {
        // Safety: a non-null value in the slot came from a prior `into_ptr` call.
        NonNull::new(ThreadContextRecord::detach_raw())
            .map(|ptr| unsafe { OwnedThreadContext::from_ptr(ptr) })
    }
}

impl Default for OwnedThreadContext {
    fn default() -> Self {
        Self::from(ThreadContextRecord::default())
    }
}

impl From<ThreadContextRecord> for OwnedThreadContext {
    fn from(record: ThreadContextRecord) -> Self {
        // Safety: `Box::into_raw` returns a non-null pointer
        unsafe { Self(NonNull::new_unchecked(Box::into_raw(Box::new(record)))) }
    }
}

impl From<ThreadContext> for OwnedThreadContext {
    fn from(ctx: ThreadContext) -> Self {
        Self::from(ctx.0)
    }
}

impl Drop for OwnedThreadContext {
    fn drop(&mut self) {
        // Safety: `self.0` was obtained from a `Box::new`, and `OwnedThreadContext` represents
        // ownership of the underlying memory.
        unsafe {
            let _ = Box::from_raw(self.0.as_ptr());
        }
    }
}

#[cfg(test)]
// The tests are set to be ignored by Miri, since the inline-asm TLSDESC access isn't supported.
mod tests {
    use super::{OwnedThreadContext, ThreadContextRecord};
    use crate::linux::read_tls_context_ptr;
    use std::sync::atomic::Ordering;

    const NO_TRACE_FLAGS: u8 = 0;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn tls_lifecycle_basic() {
        let trace_id = [1u8; 16];
        let span_id = [2u8; 8];
        let root_span_id = [3u8; 8];

        assert!(
            read_tls_context_ptr().is_null(),
            "TLS must be null initially"
        );
        OwnedThreadContext::new(trace_id, span_id, NO_TRACE_FLAGS, root_span_id, &[]).attach();
        assert!(
            !read_tls_context_ptr().is_null(),
            "TLS must not be null after attach"
        );

        let prev = OwnedThreadContext::detach().unwrap();

        unsafe {
            assert!(
                prev.0.as_ref().trace_id == trace_id,
                "got back a different trace_id than attached"
            );
            assert!(
                prev.0.as_ref().span_id == span_id,
                "got back a different span_id than attached"
            );
        }

        assert!(
            read_tls_context_ptr().is_null(),
            "TLS must be null after detach"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn raw_tls_pointer_read() {
        let trace_id = [1u8; 16];
        let span_id = [2u8; 8];
        let root_span_id = [3u8; 8];

        OwnedThreadContext::new(trace_id, span_id, NO_TRACE_FLAGS, root_span_id, &[]).attach();

        let ptr = read_tls_context_ptr();
        assert!(!ptr.is_null(), "TLS must be non-null after attach");

        // Safety: context is still live.
        let record = unsafe { &*ptr };
        assert_eq!(record.trace_id, trace_id);
        assert_eq!(record.span_id, span_id);
        assert_eq!(record.valid.load(Ordering::Relaxed), 1);
        // 1 (key) + 1 (len) + 16 (root_span_id hex chars) = 18
        assert_eq!(record.attrs_data_size, 18);

        let _ = OwnedThreadContext::detach();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn attribute_encoding_basic() {
        let attrs: &[(u8, &str)] = &[(1, "GET"), (2, "/api/v1")];
        OwnedThreadContext::new([0u8; 16], [0u8; 8], NO_TRACE_FLAGS, [0u8; 8], attrs).attach();

        let ptr = read_tls_context_ptr();
        assert!(!ptr.is_null());
        let record = unsafe { &*ptr };
        // 1+1+16 (root_span_id hex) + 1+1+3 (GET) + 1+1+7 (/api/v1)
        let expected_size: u16 = (2 + 16 + 2 + 3 + 2 + 7) as u16;
        assert_eq!(record.attrs_data_size, expected_size);
        assert_eq!(record.attrs_data[0], 0);
        assert_eq!(record.attrs_data[1], 16);
        assert_eq!(&record.attrs_data[2..18], b"0000000000000000");
        assert_eq!(record.attrs_data[18], 1);
        assert_eq!(record.attrs_data[19], 3);
        assert_eq!(&record.attrs_data[20..23], b"GET");
        assert_eq!(record.attrs_data[23], 2);
        assert_eq!(record.attrs_data[24], 7);
        assert_eq!(&record.attrs_data[25..32], b"/api/v1");

        let _ = OwnedThreadContext::detach();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn attribute_truncation_on_overflow() {
        // Build attributes whose combined encoded size exceeds MAX_ATTRS_DATA_SIZE.
        // Each max entry: 1 (key) + 1 (len) + 255 (val) = 257 bytes.
        // root_span_id: 1 (key) + 1 (len) + 16 (hex val) = 18 bytes.
        // Two such entries: 514 bytes, plus root_span_id: 532.
        // A third entry of 100 chars would need 102 bytes, bringing the total to 634 > 612, so
        // the third entry must be dropped.
        let val_a = "a".repeat(255); // 257 bytes encoded
        let val_b = "b".repeat(255); // 257 bytes encoded → 514 total
        let val_c = "c".repeat(100); // 102 bytes encoded → 626 total: must be dropped

        let attrs: &[(u8, &str)] = &[
            (1, val_a.as_str()),
            (2, val_b.as_str()),
            (3, val_c.as_str()),
        ];

        OwnedThreadContext::new([0u8; 16], [0u8; 8], NO_TRACE_FLAGS, [0u8; 8], attrs).attach();

        let ptr = read_tls_context_ptr();
        assert!(!ptr.is_null());
        let record = unsafe { &*ptr };
        // Only the first two entries fit (514 bytes + 18 bytes for root_span_id).
        assert_eq!(record.attrs_data_size, 532);
        assert_eq!(record.attrs_data[18], 1);
        assert_eq!(record.attrs_data[19], 255);
        assert_eq!(record.attrs_data[275], 2);
        assert_eq!(record.attrs_data[276], 255);

        let _ = OwnedThreadContext::detach();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn update_record_in_place() {
        let trace_id1 = [1u8; 16];
        let span_id1 = [0x01, 0x12, 0x23, 0x34, 0x45, 0x56, 0x67, 0x78];
        let root_span_id1 = [0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F];
        let trace_id2 = [2u8; 16];
        let span_id2 = [0x0A, 0x1B, 0x2C, 0x3D, 0x4E, 0x5F, 0x6A, 0x7B];
        let root_span_id2 = [0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F, 0x80];

        // Updating before any context is attached should be equivalent to `attach()`
        OwnedThreadContext::update(trace_id1, span_id1, 0xA5, root_span_id1, &[(0, "v1")]);

        let ptr_before = read_tls_context_ptr();
        assert!(!ptr_before.is_null());
        let record = unsafe { &*ptr_before };
        assert_eq!(record.trace_id, trace_id1);
        assert_eq!(record.span_id, span_id1);
        assert_eq!(record.trace_flags, 0xA5);
        assert_eq!(record.valid.load(Ordering::Relaxed), 1);
        assert_eq!(record.attrs_data[0], 0);
        assert_eq!(record.attrs_data[1], 16);
        assert_eq!(&record.attrs_data[2..18], b"78797a7b7c7d7e7f");
        assert_eq!(record.attrs_data[18], 0);
        assert_eq!(record.attrs_data[19], 2);
        assert_eq!(&record.attrs_data[20..22], b"v1");

        OwnedThreadContext::update(trace_id2, span_id2, 1, root_span_id2, &[(0, "v2")]);

        let ptr_after = read_tls_context_ptr();
        assert_eq!(
            ptr_before, ptr_after,
            "modify must not change the TLS pointer"
        );

        let record = unsafe { &*ptr_after };
        assert_eq!(record.trace_id, trace_id2);
        assert_eq!(record.span_id, span_id2);
        assert_eq!(record.trace_flags, 1);
        assert_eq!(record.valid.load(Ordering::Relaxed), 1);
        assert_eq!(record.attrs_data[0], 0);
        assert_eq!(record.attrs_data[1], 16);
        assert_eq!(&record.attrs_data[2..18], b"797a7b7c7d7e7f80");
        assert_eq!(record.attrs_data[18], 0);
        assert_eq!(record.attrs_data[19], 2);
        assert_eq!(&record.attrs_data[20..22], b"v2");

        let _ = OwnedThreadContext::detach();
        assert!(read_tls_context_ptr().is_null());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn update_and_attach_replaces_current_context() {
        OwnedThreadContext::new([0u8; 16], [0u8; 8], 0x11, [0u8; 8], &[]).attach();
        let previous_ptr = read_tls_context_ptr();

        let target =
            OwnedThreadContext::new([1u8; 16], [1u8; 8], 0x22, [1u8; 8], &[]).into_opaque_ptr();
        let target_ptr = target.cast::<ThreadContextRecord>().as_ptr();

        let previous = unsafe {
            OwnedThreadContext::update_and_attach(target, [2u8; 16], [2u8; 8], 0xA5, [2u8; 8], &[])
        }
        .expect("the previously attached context must be returned");

        assert_eq!(read_tls_context_ptr(), target_ptr.cast_const());
        assert_eq!(previous.0.as_ptr().cast_const(), previous_ptr);

        let target_record = unsafe { &*target_ptr };
        assert_eq!(target_record.trace_id, [2u8; 16]);
        assert_eq!(target_record.span_id, [2u8; 8]);
        assert_eq!(target_record.trace_flags, 0xA5);
        assert_eq!(target_record.attrs_data_size, 18);
        assert_eq!(target_record.attrs_data[0], 0);
        assert_eq!(target_record.attrs_data[1], 16);
        assert_eq!(&target_record.attrs_data[2..18], b"0202020202020202");

        let previous_record = unsafe { previous.0.as_ref() };
        assert_eq!(previous_record.trace_id, [0u8; 16]);
        assert_eq!(previous_record.span_id, [0u8; 8]);
        assert_eq!(previous_record.trace_flags, 0x11);
        assert_eq!(previous_record.attrs_data_size, 18);
        assert_eq!(previous_record.attrs_data[0], 0);
        assert_eq!(previous_record.attrs_data[1], 16);
        assert_eq!(&previous_record.attrs_data[2..18], b"0000000000000000");

        let attached_target = OwnedThreadContext::detach().expect("target must be attached");
        assert_eq!(attached_target.0.as_ptr(), target_ptr);
        drop(attached_target);
        drop(previous);

        assert!(read_tls_context_ptr().is_null());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn update_and_attach_updates_current_context_in_place() {
        let target =
            OwnedThreadContext::new([1u8; 16], [1u8; 8], 0x11, [1u8; 8], &[]).into_opaque_ptr();

        let target_ptr = target.cast::<ThreadContextRecord>().as_ptr();

        let owner = unsafe { OwnedThreadContext::from_opaque_ptr(target) };
        assert!(owner.attach().is_none());
        assert_eq!(read_tls_context_ptr(), target_ptr.cast_const());

        let previous = unsafe {
            OwnedThreadContext::update_and_attach(target, [2u8; 16], [2u8; 8], 0xA5, [2u8; 8], &[])
        };

        assert!(
            previous.is_none(),
            "an already-current target must not be returned as a previous owner"
        );
        assert_eq!(read_tls_context_ptr(), target_ptr.cast_const());

        let target_record = unsafe { &*target_ptr };
        assert_eq!(target_record.trace_id, [2u8; 16]);
        assert_eq!(target_record.span_id, [2u8; 8]);
        assert_eq!(target_record.trace_flags, 0xA5);
        assert_eq!(target_record.attrs_data_size, 18);
        assert_eq!(target_record.attrs_data[0], 0);
        assert_eq!(target_record.attrs_data[1], 16);
        assert_eq!(&target_record.attrs_data[2..18], b"0202020202020202");

        let previous = unsafe {
            OwnedThreadContext::update_and_attach(target, [3u8; 16], [3u8; 8], 0x5A, [3u8; 8], &[])
        };

        assert!(previous.is_none());
        assert_eq!(read_tls_context_ptr(), target_ptr.cast_const());

        let target_record = unsafe { &*target_ptr };
        assert_eq!(target_record.trace_id, [3u8; 16]);
        assert_eq!(target_record.span_id, [3u8; 8]);
        assert_eq!(target_record.trace_flags, 0x5A);
        assert_eq!(target_record.attrs_data_size, 18);
        assert_eq!(target_record.attrs_data[0], 0);
        assert_eq!(target_record.attrs_data[1], 16);
        assert_eq!(&target_record.attrs_data[2..18], b"0303030303030303");
        assert_eq!(target_record.valid.load(Ordering::Relaxed), 1);

        let detached = OwnedThreadContext::detach().expect("target must remain attached");
        assert_eq!(detached.0.as_ptr(), target_ptr);
        drop(detached);

        assert!(OwnedThreadContext::detach().is_none());
        assert!(read_tls_context_ptr().is_null());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn update_and_attach_attaches_to_empty_tls() {
        assert!(read_tls_context_ptr().is_null());

        let target = OwnedThreadContext::new([1u8; 16], [1u8; 8], 0x11, [1u8; 8], &[(7, "stale")])
            .into_opaque_ptr();

        let target_ptr = target.cast::<ThreadContextRecord>().as_ptr();

        assert!(read_tls_context_ptr().is_null());

        let previous = unsafe {
            OwnedThreadContext::update_and_attach(
                target,
                [2u8; 16],
                [2u8; 8],
                0xA5,
                [2u8; 8],
                &[(8, "fresh")],
            )
        };

        assert!(previous.is_none());
        assert_eq!(read_tls_context_ptr(), target_ptr.cast_const());

        let target_record = unsafe { &*target_ptr };
        assert_eq!(target_record.trace_id, [2u8; 16]);
        assert_eq!(target_record.span_id, [2u8; 8]);
        assert_eq!(target_record.trace_flags, 0xA5);
        assert_eq!(target_record.valid.load(Ordering::Relaxed), 1);

        assert_eq!(target_record.attrs_data_size, 25);
        assert_eq!(target_record.attrs_data[0], 0);
        assert_eq!(target_record.attrs_data[1], 16);
        assert_eq!(&target_record.attrs_data[2..18], b"0202020202020202");
        assert_eq!(target_record.attrs_data[18], 8);
        assert_eq!(target_record.attrs_data[19], 5);
        assert_eq!(&target_record.attrs_data[20..25], b"fresh");

        let detached = OwnedThreadContext::detach().expect("target must be attached");
        assert_eq!(detached.0.as_ptr(), target_ptr);
        drop(detached);

        assert!(OwnedThreadContext::detach().is_none());
        assert!(read_tls_context_ptr().is_null());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn explicit_detach_nulls_tls() {
        OwnedThreadContext::new([0u8; 16], [0u8; 8], NO_TRACE_FLAGS, [0u8; 8], &[]).attach();
        assert!(!read_tls_context_ptr().is_null());

        let _ = OwnedThreadContext::detach();
        assert!(read_tls_context_ptr().is_null());

        // Calling detach again is safe (no-op, returns None).
        let _ = OwnedThreadContext::detach();
        assert!(read_tls_context_ptr().is_null());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn long_value_capped_at_255_bytes() {
        let long_val = "a".repeat(300);
        OwnedThreadContext::new(
            [0u8; 16],
            [0u8; 8],
            NO_TRACE_FLAGS,
            [0u8; 8],
            &[(0, long_val.as_str())],
        )
        .attach();

        let ptr = read_tls_context_ptr();
        assert!(!ptr.is_null());
        let record = unsafe { &*ptr };
        // root_span_id occupies offset 0..18, then the attr entry starts at 18: key at [18],
        // len at [19]
        let val_len = record.attrs_data[2 + 16 + 1];
        assert_eq!(val_len, 255, "value must be capped at 255 bytes");
        assert_eq!(record.attrs_data_size, 2 + 16 + 2 + 255);

        let _ = OwnedThreadContext::detach();
    }

    // Make sure the TLSDESC accessor is indeed providing a thread-local address.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn tls_slots_are_per_thread() {
        use std::sync::{Arc, Barrier};

        let barrier = Arc::new(Barrier::new(2));
        let b = barrier.clone();

        let spawned_trace_id = [0xABu8; 16];
        let spawned_span_id = [0xCD, 0xBC, 0xAB, 0x9A, 0x89, 0x78, 0x67, 0x56];
        let spawned_root_span_id = [0xEF, 0xDE, 0xCD, 0xBC, 0xAB, 0x9A, 0x89, 0x78];
        let main_trace_id = [0x11u8; 16];
        let main_span_id = [0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99];
        let main_root_span_id = [0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA];

        let handle = std::thread::spawn(move || {
            OwnedThreadContext::new(
                spawned_trace_id,
                spawned_span_id,
                0,
                spawned_root_span_id,
                &[],
            )
            .attach();

            // Let the main thread attach its own record and verify its slot.
            b.wait();
            // Wait for the main thread to finish observing before we verify ours.
            b.wait();

            // The main thread's attach must not have touched this slot.
            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null(), "spawned thread TLS must still be set");
            let record = unsafe { &*ptr };
            assert_eq!(record.trace_id, spawned_trace_id);
            assert_eq!(record.span_id, spawned_span_id);
            assert_eq!(&record.attrs_data[2..18], b"efdecdbcab9a8978");

            let _ = OwnedThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());
        });

        // Wait for the spawned thread to attach its record, then attach our own.
        barrier.wait();

        assert!(
            read_tls_context_ptr().is_null(),
            "main thread should see a null pointer and not another thread's context"
        );

        OwnedThreadContext::new(main_trace_id, main_span_id, 0, main_root_span_id, &[]).attach();

        let ptr = read_tls_context_ptr();
        assert!(!ptr.is_null(), "main thread TLS must be set");
        let record = unsafe { &*ptr };
        assert_eq!(record.trace_id, main_trace_id);
        assert_eq!(record.span_id, main_span_id);
        assert_eq!(&record.attrs_data[2..18], b"33445566778899aa");

        barrier.wait();

        let _ = OwnedThreadContext::detach();
        assert!(read_tls_context_ptr().is_null());

        handle.join().unwrap();
    }
}
