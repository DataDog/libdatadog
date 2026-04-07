// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # Thread-level context sharing
//!
//! This module implements the publisher side of the Thread Context OTEP (PR #4947).
//!
//! Since `rustc` doesn't currently support the TLSDESC dialect, we use a C shim to set and get the
//! thread-local storage used for the context.
//!
//! ## Usage
//!
//! There are two main patterns for publishing and updating thread contexts.
//!
//! ### In-place update
//!
//! The simplest pattern, when applicable, is to attach one record and then mutate it in place.
//! This avoid allocation in the hot path.
//!
//! ```ignore
//! #use libdd_library_config::otel_thread_ctx::linux::ThreadContext;
//!
//! let trace_id = [0u8; 16];
//! let span_id  = [1u8; 8];
//!
//! // First call allocates a record and attaches it.
//! ThreadContext::new(trace_id, span_id, &[(0, "first")]).attach();
//! ThreadContext::update(trace_id, span_id, &[(0, "second")]);
//! ThreadContext::detach();
//! ```
//!
//! ### Swapping
//!
//! Swapping can be used when it's beneficial to pre-allocate or keep around a bunch of contexts to
//! be saved and restored repeatedly. Could be the case with async-runtimes were several tasks
//! might run on the same thread, or even move from one thread to another, for example.
//!
//! ```ignore
//! #use libdd_library_config::otel_thread_ctx::linux::ThreadContext;
//!
//! let trace_id = [0u8; 16];
//! let span_id  = [1u8; 8];
//! let attrs: &[(u8, &str)] = &[(0, "GET"), (1, "/api/v1")];
//!
//! // Publish a new context and save the previously attached one (if any).
//! let ctx = ThreadContext::new(trace_id, span_id, attrs);
//! let previous = ctx.attach();
//!
//! // ... do work inside the span ...
//!
//! // Restore the previous context: detach the current one and re-attach the saved one.
//! if let Some(prev) = previous {
//!     // here we drop `ctx`, but we could store for later usage
//!     let _ = prev.attach();
//! }
//! ```
//!
//! ## Synchronization
//!
//! Readers are constrained to the same thread as the writer and operate like async-signal
//! handlers: the writer thread is always stopped while a reader runs. There is thus no
//! cross-thread synchronization concerns. The only hazard is compiler reordering, which is
//! handled by making `valid` atomic and using compiler-only fences (equivalent to C's
//! `atomic_signal_fence`) to keep field writes boxed between the `valid = 0` and `valid = 1`
//! stores during in-place updates.

#[cfg(target_os = "linux")]
pub mod linux {
    use std::{
        ffi::c_void,
        mem,
        ptr::{self, NonNull},
        sync::atomic::{compiler_fence, AtomicPtr, AtomicU8, Ordering},
    };

    extern "C" {
        /// Return the address of the current thread's `otel_thread_ctx_v1` local.
        ///
        /// **CAUTION**: do not use this directly, always go through [get_tls_slot] to read and
        /// write it atomically.
        fn libdd_get_otel_thread_ctx_v1() -> *mut *mut c_void;
    }

    /// Return an atomic view of the TLS slot. The address calculation requires a call to a C shim
    /// in order to use the TLSDESC dialect from Rust. The returned address is stable (per thread),
    /// so the resulting atomic should be reused whenever possible, to reduce the number of calls
    /// to this function.
    ///
    /// The slot is read by an async signal handler. Atomic operations should in general use
    /// [Odering::Relaxed], but modifications to the record might need additional compiler-only
    /// fences (see [ThreadContext::update] for an example).
    fn get_tls_slot<'a>() -> &'a AtomicPtr<ThreadContextRecord> {
        const {
            assert!(
                mem::align_of::<AtomicPtr<ThreadContextRecord>>()
                    == mem::align_of::<*mut ThreadContextRecord>()
            )
        }

        // Safety: the const assertion above ensures the alignment is correct. The TLS slot is
        // valid for writes during the lifetime of the program.
        //
        // We forbid direct usage of `libdd_get_otel_thread_ctx_v1`, which guarantees
        // that there's never conflicting non-atomic accesses to the TLS slot.
        unsafe {
            AtomicPtr::from_ptr(libdd_get_otel_thread_ctx_v1().cast::<*mut ThreadContextRecord>())
        }
    }

    // We maintain the convention in libdatadog that the `local_root_span_id` attribute key is
    // always the very first in the string table, so its key index is guaranteed to be zero.
    const ROOT_SPAN_KEY_INDEX: u8 = 0;

    /// Maximum size in bytes of the `attrs_data` field.
    ///
    /// Chosen so that the total record size (`28 + MAX_ATTRS_DATA_SIZE`) stays within the 640-byte
    /// limit recommended by the spec (the eBPF profiler read limit).
    pub const MAX_ATTRS_DATA_SIZE: usize = 612;

    /// In-memory layout of a thread-level context.
    ///
    /// **CAUTION**: The structure MUST match exactly the OTel thread-level context specification.
    /// It is read by external, out-of-process code. Do not re-order fields or modify in any way,
    /// unless you know exactly what you're doing.
    ///
    /// # Synchronization
    ///
    /// Readers are async-signal handlers. The writer is always stopped while a reader runs.
    /// Sharing memory with a signal handler still requires some form of synchronization, which is
    /// achieved through atomics and compiler fence, using `valid` and/or the TLS slot as
    /// synchronization points.
    ///
    /// - The writer stores `valid = 0` *before* modifying fields in-place, guarded by a fence.
    /// - The writer stores `valid = 1` *after* all fields are populated, guarded by a fence.
    /// - `valid` starts at `1` on construction and is never set to `0` except during an in-place
    ///   update.
    // Note: we don't need to make this struct packed, because it's already designed to avoid
    // padding. Moreover, doing so would make it 1-aligned, potentially making access to
    // `attrs_data_size` unaligned and thus slower, and prevent us from using `AtomicU8` for
    // `valid`. We just use a const assertion in `new()` to surprises and make sure this struct has
    // the right total size.
    #[repr(C)]
    struct ThreadContextRecord {
        /// Trace identifier; all-zeroes means "no trace".
        trace_id: [u8; 16],
        /// Span identifier.
        span_id: [u8; 8],
        /// Whether the record is ready/consistent. Always set to `1` except during in-place update
        /// of the current record.
        valid: AtomicU8,
        _reserved: u8,
        /// Number of populated bytes in `attrs_data`.
        attrs_data_size: u16,
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
        /// without (re)allocation in the hot path. Having a hybrid scheme (starting smaller and
        /// resizing up a few times) is not out of the question.
        attrs_data: [u8; MAX_ATTRS_DATA_SIZE],
    }

    impl ThreadContextRecord {
        /// Build a record with the given trace id, span id and attributes. The
        /// `local_root_span_id` is a distinguished attribute with special handling for
        /// convenience, but it ends up as other attributes in `attrs_data`.
        fn new(
            trace_id: [u8; 16],
            span_id: [u8; 8],
            local_root_span_id: [u8; 8],
            attrs: &[(u8, &str)],
        ) -> Self {
            const { assert!(size_of::<ThreadContextRecord>() == 640) }

            let mut record = Self {
                trace_id,
                span_id,
                ..Default::default()
            };
            record.set_attrs(local_root_span_id, attrs);
            record
        }

        /// Encode `attributes` into `record.attrs_data` as packed key-value records. Existing data
        /// are overridden (and if there were more entires than `attributes.len()`, they aren't
        /// zeroed, but they will be ignored by readers).
        ///
        /// # Return
        ///
        /// Returns `true` if all attributes were properly encoded, or `false` if some of the data
        /// needed to be dropped. See Size limits below.
        ///
        /// # Arguments
        ///
        /// Each input entry is a pair of a 1-byte `key_index` and a string value.
        ///
        /// # Size limits
        ///
        /// Any value over 255 bytes will be capped at this size. If the total size of the encoded
        /// attributes is over [MAX_ATTRS_DATA_SIZE], extra attributes are ignored. We do this
        /// instead of raising an error because we encode the attributes on-the-fly. Proper error
        /// recovery would require us to be able to rollback to the previous attributes which would
        /// hurt the happy path, or leave the record in a inconsistent state. Another possibility
        /// would be to error out and reset the record in that situation.
        fn set_attrs(&mut self, local_root_span_id: [u8; 8], attributes: &[(u8, &str)]) -> bool {
            let mut fully_encoded = true;

            const { assert!(MAX_ATTRS_DATA_SIZE >= 18) }
            // The local root span id is provided as raw bytes (can be seen as a big-endian u64),
            // but readers will expect a string hex representation. We convert it to a fixed
            // 16-characters string in the usual lowercase hex format.
            //
            // There's currently no easy way to use Rust format capabilities to write directly in a
            // fixed-size array. Since the conversion is simple, we do it manually.
            const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
            self.attrs_data[0] = ROOT_SPAN_KEY_INDEX;
            self.attrs_data[1] = 16;
            for (i, &byte) in local_root_span_id.iter().enumerate() {
                self.attrs_data[2 + i * 2] = HEX_DIGITS[(byte >> 4) as usize];
                self.attrs_data[2 + i * 2 + 1] = HEX_DIGITS[(byte & 0xF) as usize];
            }

            let mut offset = 18;

            for &(key_index, val) in attributes {
                let val_bytes = val.as_bytes();
                let val_len = val_bytes.len();
                let val_len = if val_len > 255 {
                    fully_encoded = false;
                    255
                } else {
                    val_len
                };
                let entry_size = 2 + val_len;

                if offset + entry_size > MAX_ATTRS_DATA_SIZE {
                    fully_encoded = false;
                    break;
                }

                self.attrs_data[offset] = key_index;
                // `val_len <= 255` thanks to the `min()`
                self.attrs_data[offset + 1] = val_len as u8;
                self.attrs_data[offset + 2..offset + 2 + val_len]
                    .copy_from_slice(&val_bytes[..val_len]);
                offset += entry_size;
            }

            // `offset < MAX_ATTRS_DATA_SIZE`, which guarantees it fits in a `u16`. This also
            // effectively hide the remaining of the previous `attrs` bytes, so we don't have to
            // zero them.
            self.attrs_data_size = offset as u16;
            fully_encoded
        }
    }

    impl Default for ThreadContextRecord {
        fn default() -> Self {
            Self {
                trace_id: [0u8; 16],
                span_id: [0u8; 8],
                // We only ever set `valid` to `0` during in-place update of an attached context.
                valid: AtomicU8::new(1),
                _reserved: 0,
                attrs_data_size: 0,
                attrs_data: [0u8; MAX_ATTRS_DATA_SIZE],
            }
        }
    }

    /// An owned (and non-moving) thread context record allocation.
    ///
    /// We don't use `Box` under the hood because it precludes aliasing, while we share the context
    /// to readers through thread-level context and through the FFI. But it is a boxed
    /// `ThreadContextRecord` for all intent of purpose.
    ///
    /// The context is `!Send` and `!Sync`; it is supposed to stay on the same thread and is thus
    /// not thread-safe.
    pub struct ThreadContext(NonNull<ThreadContextRecord>);

    impl ThreadContext {
        /// Create a new thread context with the given trace/span IDs and encoded attributes.
        pub fn new(
            trace_id: [u8; 16],
            span_id: [u8; 8],
            local_root_span_id: [u8; 8],
            attrs: &[(u8, &str)],
        ) -> Self {
            Self::from(ThreadContextRecord::new(
                trace_id,
                span_id,
                local_root_span_id,
                attrs,
            ))
        }

        /// Turn this thread context into a raw pointer to the underlying [ThreadContextRecord].
        /// The pointer must be reconstructed through [`Self::from_raw`] in order to be properly
        /// dropped, or the record will leak.
        fn into_raw(self) -> *mut ThreadContextRecord {
            let mdrop = mem::ManuallyDrop::new(self);
            mdrop.0.as_ptr()
        }

        /// Reconstruct a [ThreadContextRecord] from a raw pointer that is either `null` or comes
        /// from [`Self::into_raw`]. Return `None` if `ptr` is null.
        ///
        /// # Safety
        ///
        /// - `ptr` must be `null` or come from a prior call to [`Self::into_raw`].
        /// - if `ptr` is aliased, accesses to through aliases must not be interleaved with method
        ///   calls on the returned [ThreadContextRecord]. More precisely, mutable references might
        ///   be reconstructed during those calls, so any constraint from either Stacked Borrows,
        ///   Tree Borrows or whatever is the current aliasing model implemented in Miri applies.
        unsafe fn from_raw(ptr: *mut ThreadContextRecord) -> Option<Self> {
            NonNull::new(ptr).map(Self)
        }
    }

    impl Default for ThreadContext {
        fn default() -> Self {
            Self::from(ThreadContextRecord::default())
        }
    }

    impl From<ThreadContextRecord> for ThreadContext {
        fn from(record: ThreadContextRecord) -> Self {
            // Safety: `Box::into_raw` returns a non-null pointer
            unsafe { Self(NonNull::new_unchecked(Box::into_raw(Box::new(record)))) }
        }
    }

    impl ThreadContext {
        /// Atomically swap the current context with a pointer value. Return the previously
        /// attached context, if any.
        fn swap(
            slot: &AtomicPtr<ThreadContextRecord>,
            tgt: *mut ThreadContextRecord,
        ) -> Option<ThreadContext> {
            // Safety: a non-null value in the slot came from a prior `into_raw` call.
            unsafe { ThreadContext::from_raw(slot.swap(tgt, Ordering::Relaxed)) }
        }

        /// Publish a new (or previously detached) thread context record by writing its pointer
        /// into the TLS slot. Returns the previously attached context, if any.
        ///
        /// `valid` is already `1` since construction, so any reader that observes the new pointer
        /// also observes `valid = 1`.
        pub fn attach(self) -> Option<ThreadContext> {
            // [^tls-slot-ordering]: since we get back the previous context, we should in principle
            // use an `Acquire` compiler fence to make sure we don't get back a not-yet-initialized
            // record.
            //
            // However, this thread (excluding the reader signal handler) is the only one to ever
            // _write_ to the context, so the store we load the value from automatically
            // happens-before (because it's sequenced-before) the swap.
            Self::swap(get_tls_slot(), self.into_raw())
        }

        /// Update the currently attached record in-place. Sets `valid = 0` before the update and
        /// `valid = 1` after, so a reader that fires between the two writes sees an inconsistent
        /// record and skips it. Compiler fences prevent the compiler from reordering field writes
        /// outside that window.
        ///
        /// If there's currently no attached context, `update` will create one, and is in this case
        /// equivalent to `ThreadContext::new(trace_id, span_id, attrs).attach()`.
        pub fn update(
            trace_id: [u8; 16],
            span_id: [u8; 8],
            local_root_span_id: [u8; 8],
            attrs: &[(u8, &str)],
        ) {
            let slot = get_tls_slot();

            if let Some(current) = unsafe { slot.load(Ordering::Relaxed).as_mut() } {
                current.valid.store(0, Ordering::Relaxed);
                compiler_fence(Ordering::SeqCst);

                current.trace_id = trace_id;
                current.span_id = span_id;
                current.set_attrs(local_root_span_id, attrs);

                compiler_fence(Ordering::SeqCst);
                current.valid.store(1, Ordering::Relaxed);
            } else {
                // No need for `AcqRel`, see [^tls-slot-ordering].
                compiler_fence(Ordering::Release);
                // `ThreadContext::new` already initialises `valid = 1`.
                let _ = Self::swap(
                    slot,
                    ThreadContext::new(trace_id, span_id, local_root_span_id, attrs).into_raw(),
                );
            }
        }

        /// Detach the current record from the TLS slot. Writes null to the slot and returns the
        /// detached record.
        pub fn detach() -> Option<ThreadContext> {
            // We don't need any fence here, see [^tls-slot-ordering].
            Self::swap(get_tls_slot(), ptr::null_mut())
        }
    }

    impl Drop for ThreadContext {
        fn drop(&mut self) {
            // Safety: `self.0` was obtained from a `Box::new`, and `ThreadContext` represents
            // ownership of the underlying memory.
            unsafe {
                let _ = Box::from_raw(self.0.as_ptr());
            }
        }
    }

    #[cfg(test)]
    // The tests are set to be ignored by Miri, since accessing the TLS through C isn't supported.
    mod tests {
        use super::{ThreadContext, ThreadContextRecord};
        use std::sync::atomic::Ordering;

        /// Read the TLS pointer for the current thread (the value stored in the TLS slot, not the
        /// address of the slot itself).
        fn read_tls_context_ptr() -> *const ThreadContextRecord {
            super::get_tls_slot().load(Ordering::Relaxed)
        }

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
            ThreadContext::new(trace_id, span_id, root_span_id, &[]).attach();
            assert!(
                !read_tls_context_ptr().is_null(),
                "TLS must not be null after attach"
            );

            let prev = ThreadContext::detach().unwrap();

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

            ThreadContext::new(trace_id, span_id, root_span_id, &[]).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null(), "TLS must be non-null after attach");

            // Safety: context is still live.
            let record = unsafe { &*ptr };
            assert_eq!(record.trace_id, trace_id);
            assert_eq!(record.span_id, span_id);
            assert_eq!(record.valid.load(Ordering::Relaxed), 1);
            // 1 (key) + 1 (len) + 16 (root_span_id hex chars) = 18
            assert_eq!(record.attrs_data_size, 18);

            let _ = ThreadContext::detach();
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn attribute_encoding_basic() {
            let attrs: &[(u8, &str)] = &[(1, "GET"), (2, "/api/v1")];
            ThreadContext::new([0u8; 16], [0u8; 8], [0u8; 8], attrs).attach();

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

            let _ = ThreadContext::detach();
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

            ThreadContext::new([0u8; 16], [0u8; 8], [0u8; 8], attrs).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null());
            let record = unsafe { &*ptr };
            // Only the first two entries fit (514 bytes + 18 bytes for root_span_id).
            assert_eq!(record.attrs_data_size, 532);
            assert_eq!(record.attrs_data[18], 1);
            assert_eq!(record.attrs_data[19], 255);
            assert_eq!(record.attrs_data[275], 2);
            assert_eq!(record.attrs_data[276], 255);

            let _ = ThreadContext::detach();
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
            ThreadContext::update(trace_id1, span_id1, root_span_id1, &[(0, "v1")]);

            let ptr_before = read_tls_context_ptr();
            assert!(!ptr_before.is_null());
            let record = unsafe { &*ptr_before };
            assert_eq!(record.trace_id, trace_id1);
            assert_eq!(record.span_id, span_id1);
            assert_eq!(record.valid.load(Ordering::Relaxed), 1);
            assert_eq!(record.attrs_data[0], 0);
            assert_eq!(record.attrs_data[1], 16);
            assert_eq!(&record.attrs_data[2..18], b"78797a7b7c7d7e7f");
            assert_eq!(record.attrs_data[18], 0);
            assert_eq!(record.attrs_data[19], 2);
            assert_eq!(&record.attrs_data[20..22], b"v1");

            ThreadContext::update(trace_id2, span_id2, root_span_id2, &[(0, "v2")]);

            let ptr_after = read_tls_context_ptr();
            assert_eq!(
                ptr_before, ptr_after,
                "modify must not change the TLS pointer"
            );

            let record = unsafe { &*ptr_after };
            assert_eq!(record.trace_id, trace_id2);
            assert_eq!(record.span_id, span_id2);
            assert_eq!(record.valid.load(Ordering::Relaxed), 1);
            assert_eq!(record.attrs_data[0], 0);
            assert_eq!(record.attrs_data[1], 16);
            assert_eq!(&record.attrs_data[2..18], b"797a7b7c7d7e7f80");
            assert_eq!(record.attrs_data[18], 0);
            assert_eq!(record.attrs_data[19], 2);
            assert_eq!(&record.attrs_data[20..22], b"v2");

            let _ = ThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn explicit_detach_nulls_tls() {
            ThreadContext::new([0u8; 16], [0u8; 8], [0u8; 8], &[]).attach();
            assert!(!read_tls_context_ptr().is_null());

            let _ = ThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());

            // Calling detach again is safe (no-op, returns None).
            let _ = ThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn long_value_capped_at_255_bytes() {
            let long_val = "a".repeat(300);
            ThreadContext::new([0u8; 16], [0u8; 8], [0u8; 8], &[(0, long_val.as_str())]).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null());
            let record = unsafe { &*ptr };
            // root_span_id occupies offset 0..18, then the attr entry starts at 18: key at [18],
            // len at [19]
            let val_len = record.attrs_data[2 + 16 + 1];
            assert_eq!(val_len, 255, "value must be capped at 255 bytes");
            assert_eq!(record.attrs_data_size, 2 + 16 + 2 + 255);

            let _ = ThreadContext::detach();
        }

        // Make sure the C shim is indeed providing a thread-local address.
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
                ThreadContext::new(spawned_trace_id, spawned_span_id, spawned_root_span_id, &[])
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

                let _ = ThreadContext::detach();
                assert!(read_tls_context_ptr().is_null());
            });

            // Wait for the spawned thread to attach its record, then attach our own.
            barrier.wait();

            assert!(
                read_tls_context_ptr().is_null(),
                "main thread should see a null pointer and not another thread's context"
            );

            ThreadContext::new(main_trace_id, main_span_id, main_root_span_id, &[]).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null(), "main thread TLS must be set");
            let record = unsafe { &*ptr };
            assert_eq!(record.trace_id, main_trace_id);
            assert_eq!(record.span_id, main_span_id);
            assert_eq!(&record.attrs_data[2..18], b"33445566778899aa");

            barrier.wait();

            let _ = ThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());

            handle.join().unwrap();
        }
    }
}
