// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # Thread-level context sharing
//!
//! This module implements the publisher side of the Thread Context OTEP (PR #4947).
//!
//! Since `rustc` doesn't currently support the TLSDESC dialect, we use a C shim to set and get the
//! thread-local storage used for the context.
//!
//! ## Synchronization
//!
//! Readers are constrained to the same thread as the writer and operate like async-signal
//! handlers: the writer thread is always stopped while a reader runs. There is thus no
//! cross-thread synchronization concerns. The only hazard is compiler reordering or not committing
//! writes to memory, which is handled using volatile writes.

#[cfg(target_os = "linux")]
pub mod linux {
    use std::{
        ffi::c_void,
        mem,
        ops::{Deref, DerefMut},
        ptr::{self, NonNull},
    };

    extern "C" {
        /// Return the address of the current thread's `custom_labels_current_set_v2` local.
        fn libdd_get_custom_labels_current_set_v2() -> *mut *mut c_void;
    }

    /// Return a pointer to the TLS slot holding the context. The address calculation requires a
    /// call to a C shim in order to use the TLSDESC dialect from Rust. Note that the returned
    /// address is stable (per thread), so the result can be reused .
    ///
    /// Note that the address is read by an external process or a signal handler, so it should be
    /// written to using [std::ptr::write_volatile].
    #[allow(clippy::missing_safety_doc)]
    fn get_tls_ptr() -> *mut *mut ThreadContextRecord {
        // Safety: this is just an FFI call, but there's no particular pre-condition to uphold: the
        // TLS slot should always be accessible.
        unsafe { libdd_get_custom_labels_current_set_v2().cast() }
    }

    /// Maximum size in bytes of the `attrs_data` field.
    ///
    /// Chosen so that the total record size (`28 + MAX_ATTRS_DATA_SIZE`) stays within the 640-byte
    /// limit recommended by the spec (the eBPF profiler read limit).
    pub const MAX_ATTRS_DATA_SIZE: usize = 612;

    /// In-memory layout of a thread-level context. The structure MUST match exactly the
    /// specification.
    ///
    /// # Synchronization
    ///
    /// Readers are async-signal handlers on the same thread; the writer is always stopped while a
    /// reader runs. `valid` is written with `ptr::write_volatile` so the compiler cannot reorder
    /// its store past the surrounding field writes:
    ///
    /// - The writer sets `valid = 1` *after* all other fields are populated (publish).
    /// - The writer sets `valid = 0` *before* modifying fields in-place (modify).
    #[repr(C)]
    pub struct ThreadContextRecord {
        /// 128-bit trace identifier; all-zeroes means "no trace".
        pub trace_id: [u8; 16],
        /// 64-bit span identifier.
        pub span_id: [u8; 8],
        /// Whether the record is ready/consistent. Written with `ptr::write_volatile`.
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
        /// without (re)allocation in the hot path. Having a hybrid scheme (starting smaller and
        /// resizing up a few times) is not out of the question.
        pub attrs_data: [u8; MAX_ATTRS_DATA_SIZE],
    }

    impl ThreadContextRecord {
        /// Build a record with the given trace id, span id and attributes.
        pub fn new(trace_id: [u8; 16], span_id: [u8; 8], attrs: &[(u8, &str)]) -> Self {
            let mut record = Self {
                trace_id,
                span_id,
                ..Default::default()
            };
            record.set_attrs(attrs);
            record
        }

        /// Write `value` to `self.valid` using a volatile store, preventing the compiler from
        /// eliminating or reordering or this write past other volatile writes to the record.
        fn write_valid_volatile(&mut self, value: u8) {
            // Safety: we have an exclusive borrow of `self`, so `valid` is live and valid for
            // writes.
            unsafe { ptr::write_volatile(&mut self.valid as *mut u8, value) }
        }

        /// Write `trace_id` to `self.trace_id` using a volatile store, preventing the compiler
        /// from eliminating or reordering this write past other volatile writes to the record.
        pub fn write_trace_id_volatile(&mut self, trace_id: [u8; 16]) {
            // Safety: we have an exclusive borrow of `self`, so `trace_id` is live and valid for
            // writes.
            unsafe { ptr::write_volatile(&mut self.trace_id, trace_id) }
        }

        /// Write `span_id` to `self.span_id` using a volatile store, preventing the compiler from
        /// eliminating or reordering this write past other volatile writes to the record.
        pub fn write_span_id_volatile(&mut self, span_id: [u8; 8]) {
            // Safety: we have an exclusive borrow of `self`, so `span_id` is live and valid for
            // writes.
            unsafe { ptr::write_volatile(&mut self.span_id, span_id) }
        }

        /// Write a single byte of the encoded attributes using a volatile store, preventing the
        /// compiler from eliminating or reordering or this write past other volatile writes to the
        /// record.
        ///
        /// # Panic
        ///
        /// Panics if the offset is out of bound.
        fn write_attrs_volatile(&mut self, offset: usize, value: u8) {
            // Safety: we have an exclusive borrow of `self`, so `attrs_data` is live and valid for
            // writes.
            unsafe { ptr::write_volatile(&mut self.attrs_data[offset] as *mut u8, value) }
        }

        /// Copy `src` into the encoded attributes starting at `offset` using volatile stores,
        /// preventing the compiler from eliminating or reordering or this write past other
        /// volatile writes to the record.
        ///
        /// # Panic
        ///
        /// Panics if `offset + src.len() > self.attrs_data.len()`.
        fn copy_attrs_volatile(&mut self, src: &[u8], offset: usize) {
            // Safety: we have an exclusive borrow of `self`, so `attrs_data` is live and valid for
            // writes.
            for (idx, byte) in src.iter().enumerate() {
                unsafe { ptr::write_volatile(&mut self.attrs_data[offset + idx] as *mut u8, *byte) }
            }
        }

        /// Encode `attributes` into `record.attrs_data` as packed key-value records. Existing data
        /// are overridden (and if there were more entires than `attributes.len()`, they aren't
        /// zeroed, but they will be ignored by readers).
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
        ///
        /// # Memory
        ///
        /// Disclaimer: This note is for internal usage only. As a consumer of this library, please
        /// use [ThreadContext::modify] to mutate the currently attached record.
        ///
        /// Writes are volatile, making `set_attrs` suitable for mutating an attached record
        /// in-place (TODO: does this impact the performance of writing to a non-attached record?
        /// Should we have two specialized version `set_attrs` and `set_attrs_volatile`?).
        pub fn set_attrs(&mut self, attributes: &[(u8, &str)]) {
            let mut offset = 0;

            for &(key_index, val) in attributes {
                let val_bytes = val.as_bytes();
                let val_len = val_bytes.len().min(255);
                let entry_size = 2 + val_len;

                if offset + entry_size > MAX_ATTRS_DATA_SIZE {
                    break;
                }

                self.write_attrs_volatile(offset, key_index);
                // `val_len <= 255` thanks to the `min()`
                self.write_attrs_volatile(offset + 1, val_len as u8);
                self.copy_attrs_volatile(&val_bytes[..val_len], offset + 2);
                offset += entry_size;
            }

            // `offset < MAX_ATTRS_DATA_SIZE`, which guarantees it fits in a `u16`
            self.attrs_data_size = offset as u16;
        }
    }

    impl Default for ThreadContextRecord {
        fn default() -> Self {
            Self {
                trace_id: [0u8; 16],
                span_id: [0u8; 8],
                valid: 0,
                _reserved: 0,
                attrs_data_size: 0,
                attrs_data: [0u8; MAX_ATTRS_DATA_SIZE],
            }
        }
    }

    /// An owned (and non-moving) thread context record allocation.
    ///
    /// We don't use `Box` under the hood because it excludes aliasing, while we share the context
    /// to readers through thread-level context and through the FFI. But it is a boxed
    /// `ThreadContextRecord` for all intent of purpose.
    pub struct ThreadContext(NonNull<ThreadContextRecord>);

    impl ThreadContext {
        /// Create a new thread context with the given trace/span IDs and encoded attributes.
        pub fn new(trace_id: [u8; 16], span_id: [u8; 8], attrs: &[(u8, &str)]) -> Self {
            Self::from(ThreadContextRecord::new(trace_id, span_id, attrs))
        }

        /// Turn this thread context into a raw pointer to the underlying [ThreadContextRecord].
        /// The pointer must be reconstructed through [`Self::from_raw`] in order to be properly
        /// dropped, or the record will leak.
        pub fn into_raw(self) -> *mut ThreadContextRecord {
            use mem::ManuallyDrop;

            let mdrop = ManuallyDrop::new(self);
            mdrop.0.as_ptr()
        }

        /// Reconstruct a [ThreadContextRecord] from a raw pointer to `ThreadContextRecord` that is
        /// either `null` or comes from [`Self::into_raw`]. Return `None` if `ptr` is null.
        ///
        /// # Safety
        ///
        /// - `ptr` must be `null` or come from a prior call to [`Self::into_raw`].
        /// - if `ptr` is aliased, accesses to through aliases must not be interleaved with method
        ///   calls on the returned [ThreadContextRecord]. More precisely, mutable references might
        ///   be reconstructed during those calls, so any constraint from either Stacked Borrows,
        ///   Tree Borrows or whatever is the current aliasing model implemented in Miri applies.
        pub unsafe fn from_raw(ptr: *mut ThreadContextRecord) -> Option<Self> {
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
        /// Swap the current context with a pointer value. Return the previously attached context,
        /// if any.
        fn swap(
            slot: *mut *mut ThreadContextRecord,
            tgt: *mut ThreadContextRecord,
        ) -> Option<ThreadContext> {
            unsafe {
                // Safety: TLS slot is always live and valid for reads and writes.
                let prev = ThreadContext::from_raw(*slot);
                ptr::write_volatile(slot, tgt);
                // Safety: a non-null value in the slot came from a prior `into_raw` call.
                prev
            }
        }

        /// Publish a new (or previously detached) thread context record by writing its pointer
        /// into the TLS slot. Sets `valid = 1` before publishing. Returns the previously attached
        /// context, if any.
        pub fn attach(mut self) -> Option<ThreadContext> {
            let slot = get_tls_ptr();
            // Set `valid = 1` written before the TLS pointer is updated, so any reader that
            // observes the new pointer also observes `valid = 1`.
            self.write_valid_volatile(1);
            Self::swap(slot, self.into_raw())
        }

        /// Modify the currently attached record in-place. Sets `valid = 0` before the update and
        /// `valid = 1` after, so a reader that fires between the two writes sees an inconsistent
        /// record and skips it.
        ///
        /// If there's currently no attached context, `modify` will create one, and is in this case
        /// equivalent to `ThreadContext::new(trace_id, span_id, attrs).attach()`.
        pub fn update(trace_id: [u8; 16], span_id: [u8; 8], attrs: &[(u8, &str)]) {
            let slot = get_tls_ptr();

            // Safety: the tls slot is always valid for reads and writes.
            if let Some(current) = unsafe { (*slot).as_mut() } {
                current.write_valid_volatile(0);
                current.write_trace_id_volatile(trace_id);
                current.write_span_id_volatile(span_id);
                current.set_attrs(attrs);
                current.write_valid_volatile(1);
            } else {
                let mut ctx = ThreadContext::new(trace_id, span_id, attrs);
                ctx.write_valid_volatile(1);
                let _ = Self::swap(slot, ctx.into_raw());
            }
        }

        /// Detach the current record from the TLS slot. Writes null to the slot, sets `valid = 0`
        /// on the detached record, and returns it.
        pub fn detach() -> Option<ThreadContext> {
            Self::swap(get_tls_ptr(), ptr::null_mut()).map(|mut ctx| {
                ctx.write_valid_volatile(0);
                ctx
            })
        }
    }

    impl Drop for ThreadContext {
        fn drop(&mut self) {
            // Safety: `self.0` was obtained from a `Box::new`, and `ThreadContext` represents
            // ownership of the underyling memory.
            unsafe {
                let _ = Box::from_raw(self.0.as_ptr());
            }
        }
    }

    impl Deref for ThreadContext {
        type Target = ThreadContextRecord;

        fn deref(&self) -> &Self::Target {
            // Safety: `ThreadContext` represents ownership of a valid, alive
            // `ThreadContextRecord`.
            unsafe { self.0.as_ref() }
        }
    }

    impl DerefMut for ThreadContext {
        fn deref_mut(&mut self) -> &mut Self::Target {
            // Safety: `ThreadContext` represents ownership of a valid, alive
            // `ThreadContextRecord`.
            unsafe { self.0.as_mut() }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{ThreadContext, ThreadContextRecord};

        /// Read the TLS pointer for the current thread (the value stored in the TLS slot, not the
        /// address of the slot itself).
        fn read_tls_context_ptr() -> *const ThreadContextRecord {
            unsafe { *super::get_tls_ptr() as *const ThreadContextRecord }
        }

        #[test]
        fn tls_lifecycle_basic() {
            let trace_id = [1u8; 16];
            let span_id = [2u8; 8];

            assert!(
                read_tls_context_ptr().is_null(),
                "TLS must be null initially"
            );
            ThreadContext::new(trace_id, span_id, &[]).attach();
            assert!(
                !read_tls_context_ptr().is_null(),
                "TLS must not be null after attach"
            );

            let prev = ThreadContext::detach().unwrap();
            assert!(
                prev.trace_id == trace_id,
                "got back a different trace_id than attached"
            );
            assert!(
                prev.span_id == span_id,
                "got back a different span_id than attached"
            );

            assert!(
                read_tls_context_ptr().is_null(),
                "TLS must be null after detach"
            );
        }

        #[test]
        fn raw_tls_pointer_read() {
            let trace_id = [1u8; 16];
            let span_id = [2u8; 8];

            ThreadContext::new(trace_id, span_id, &[]).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null(), "TLS must be non-null after attach");

            // Safety: context is still live.
            let record = unsafe { &*ptr };
            assert_eq!(record.trace_id, trace_id);
            assert_eq!(record.span_id, span_id);
            assert_eq!(record.valid, 1);
            assert_eq!(record.attrs_data_size, 0);

            let _ = ThreadContext::detach();
        }

        #[test]
        fn attribute_encoding_basic() {
            let attrs: &[(u8, &str)] = &[(0, "GET"), (1, "/api/v1")];
            ThreadContext::new([0u8; 16], [0u8; 8], attrs).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null());
            let record = unsafe { &*ptr };
            let expected_size: u16 = (2 + 3 + 2 + 7) as u16;
            assert_eq!(record.attrs_data_size, expected_size);
            assert_eq!(record.attrs_data[0], 0);
            assert_eq!(record.attrs_data[1], 3);
            assert_eq!(&record.attrs_data[2..5], b"GET");
            assert_eq!(record.attrs_data[5], 1);
            assert_eq!(record.attrs_data[6], 7);
            assert_eq!(&record.attrs_data[7..14], b"/api/v1");

            let _ = ThreadContext::detach();
        }

        #[test]
        fn attribute_truncation_on_overflow() {
            // Build attributes whose combined encoded size exceeds MAX_ATTRS_DATA_SIZE.
            // Each max entry: 1 (key) + 1 (len) + 255 (val) = 257 bytes.
            // Two such entries: 514 bytes. A third entry of 100 chars would need 102 bytes,
            // bringing the total to 616 > 612, so the third entry must be dropped.
            let val_a = "a".repeat(255); // 257 bytes encoded
            let val_b = "b".repeat(255); // 257 bytes encoded → 514 total
            let val_c = "c".repeat(100); // 102 bytes encoded → 616 total: must be dropped

            let attrs: &[(u8, &str)] = &[
                (0, val_a.as_str()),
                (1, val_b.as_str()),
                (2, val_c.as_str()),
            ];

            ThreadContext::new([0u8; 16], [0u8; 8], attrs).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null());
            let record = unsafe { &*ptr };
            // Only the first two entries fit (514 bytes).
            assert_eq!(record.attrs_data_size, 514);
            assert_eq!(record.attrs_data[0], 0);
            assert_eq!(record.attrs_data[1], 255);
            assert_eq!(record.attrs_data[257], 1);
            assert_eq!(record.attrs_data[258], 255);

            let _ = ThreadContext::detach();
        }

        #[test]
        fn update_record_in_place() {
            let trace_id_1 = [1u8; 16];
            let span_id_1 = [1u8; 8];
            let trace_id_2 = [2u8; 16];
            let span_id_2 = [2u8; 8];

            // Updating before any context is attached should be equivalent to `attach()`
            ThreadContext::update(trace_id_1, span_id_1, &[(0, "v1")]);

            let ptr_before = read_tls_context_ptr();
            assert!(!ptr_before.is_null());
            let record = unsafe { &*ptr_before };
            assert_eq!(record.trace_id, trace_id_1);
            assert_eq!(record.span_id, span_id_1);
            assert_eq!(record.valid, 1);
            assert_eq!(record.attrs_data[0], 0);
            assert_eq!(record.attrs_data[1], 2);
            assert_eq!(&record.attrs_data[2..4], b"v1");

            ThreadContext::update(trace_id_2, span_id_2, &[(0, "v2")]);

            let ptr_after = read_tls_context_ptr();
            assert_eq!(
                ptr_before, ptr_after,
                "modify must not change the TLS pointer"
            );

            let record = unsafe { &*ptr_after };
            assert_eq!(record.trace_id, trace_id_2);
            assert_eq!(record.span_id, span_id_2);
            assert_eq!(record.valid, 1);
            assert_eq!(record.attrs_data[0], 0);
            assert_eq!(record.attrs_data[1], 2);
            assert_eq!(&record.attrs_data[2..4], b"v2");

            let _ = ThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());
        }

        #[test]
        fn explicit_detach_nulls_tls() {
            ThreadContext::new([3u8; 16], [3u8; 8], &[]).attach();
            assert!(!read_tls_context_ptr().is_null());

            let _ = ThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());

            // Calling detach again is safe (no-op, returns None).
            let _ = ThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());
        }

        #[test]
        fn long_value_capped_at_255_bytes() {
            let long_val = "a".repeat(300);
            ThreadContext::new([0u8; 16], [0u8; 8], &[(0, long_val.as_str())]).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null());
            let record = unsafe { &*ptr };
            let val_len = record.attrs_data[1] as usize;
            assert_eq!(val_len, 255, "value must be capped at 255 bytes");
            assert_eq!(record.attrs_data_size as usize, 2 + 255);

            let _ = ThreadContext::detach();
        }

        // Make sure the C shim is indeed providing a thread-local address.
        #[test]
        fn tls_slots_are_per_thread() {
            use std::sync::{Arc, Barrier};

            let barrier = Arc::new(Barrier::new(2));
            let b = barrier.clone();

            let spawned_trace_id = [0xABu8; 16];
            let spawned_span_id = [0xCDu8; 8];
            let main_trace_id = [0x11u8; 16];
            let main_span_id = [0x22u8; 8];

            let handle = std::thread::spawn(move || {
                ThreadContext::new(spawned_trace_id, spawned_span_id, &[]).attach();

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

                let _ = ThreadContext::detach();
                assert!(read_tls_context_ptr().is_null());
            });

            // Wait for the spawned thread to attach its record, then attach our own.
            barrier.wait();

            assert!(
                read_tls_context_ptr().is_null(),
                "main thread should see a null pointer and not another thread's context"
            );

            ThreadContext::new(main_trace_id, main_span_id, &[]).attach();

            let ptr = read_tls_context_ptr();
            assert!(!ptr.is_null(), "main thread TLS must be set");
            let record = unsafe { &*ptr };
            assert_eq!(record.trace_id, main_trace_id);
            assert_eq!(record.span_id, main_span_id);

            barrier.wait();

            let _ = ThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());

            handle.join().unwrap();
        }
    }
}
