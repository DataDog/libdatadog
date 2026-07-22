// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # Thread-level context sharing
//!
//! This crate implements the publisher side of the Thread Context OTEP (PR #4947).
//!
//! Since `rustc` doesn't currently support the TLSDESC dialect, we define the thread-local
//! storage symbol and its accessor using inline assembly (`global_asm!` / `asm!`).
//!
//! ## Usage
//!
//! There are two main patterns for publishing and updating thread contexts.
//!
//! ### In-place update
//!
//! The simplest pattern, when applicable, is to attach one record and then mutate it in place.
//! This avoids allocation in the hot path.
//!
//! ```rust
//! # #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
//! # fn main() {
//! use libdd_otel_thread_ctx::linux::ThreadContext;
//!
//! let trace_id = [0u8; 16];
//! let span_id = [1u8; 8];
//! let local_root_span_id = [2u8; 8];
//!
//! // First call allocates a record and attaches it.
//! ThreadContext::new(trace_id, span_id, local_root_span_id, &[(0, "first")]).attach();
//! ThreadContext::update(trace_id, span_id, local_root_span_id, &[(0, "second")]);
//! ThreadContext::detach();
//! # }
//! # #[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
//! # fn main() {}
//! ```
//!
//! ### Swapping
//!
//! Swapping can be used when it's beneficial to pre-allocate or keep around a bunch of contexts
//! to be saved and restored repeatedly. Could be the case with async-runtimes where several tasks
//! might run on the same thread, or even move from one thread to another, for example.
//!
//! ```rust
//! # #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
//! # fn main() {
//! use libdd_otel_thread_ctx::linux::ThreadContext;
//!
//! let trace_id = [0u8; 16];
//! let span_id = [1u8; 8];
//! let local_root_span_id = [2u8; 8];
//! let attrs: &[(u8, &str)] = &[(0, "GET"), (1, "/api/v1")];
//!
//! // Publish a new context and save the previously attached one (if any).
//! let ctx = ThreadContext::new(trace_id, span_id, local_root_span_id, attrs);
//! let previous = ctx.attach();
//!
//! // ... do work inside the span ...
//!
//! // Restore the previous context: detach the current one and re-attach the saved one.
//! if let Some(prev) = previous {
//!     // here we drop `ctx`, but we could store for later usage
//!     let _ = prev.attach();
//! }
//! # }
//! # #[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
//! # fn main() {}
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

mod record;

pub use record::{ThreadContextRecord, MAX_ATTRS_DATA_SIZE};

// The `linux` module below resolves the TLS slot with TLSDESC inline assembly that is only written
// for x86_64 and aarch64. Reject any other architecture on Linux at compile time. On non-Linux
// targets the `linux` module is not compiled, so there's no such constraint.
#[cfg(all(
    feature = "tls-storage",
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
compile_error!(
    "Unsupported architecture for otel-thread-ctx on Linux. Only x86_64 and aarch64 are currently \
     supported."
);

#[cfg(all(feature = "tls-storage", target_os = "linux", feature = "sanity-check"))]
pub mod sanity_check;

#[cfg(feature = "test-utils")]
pub mod test_utils;

#[cfg(all(
    feature = "tls-storage",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod linux {
    use super::ThreadContextRecord;
    pub use super::MAX_ATTRS_DATA_SIZE;
    use std::{
        mem,
        ptr::{self, NonNull},
        sync::atomic::{compiler_fence, AtomicPtr, Ordering},
    };

    // Define the thread-local pointer that external readers (e.g. the eBPF profiler) discover via
    // the dynamic symbol table. It must be an exported ELF `STT_TLS` object accessed via the
    // TLSDESC dialect, as mandated by the OTel thread-level context sharing spec.
    //
    // Stable `rustc` cannot select the TLS dialect for a `#[thread_local]` static, so we declare
    // the symbol directly in assembly (an 8-byte, zero-initialised slot in `.tbss`) and resolve
    // its per-thread address through TLSDESC in [`tls_slot`].
    core::arch::global_asm!(
        ".section .tbss,\"awT\",@nobits",
        ".globl otel_thread_ctx_v1",
        ".type  otel_thread_ctx_v1, @tls_object",
        ".size  otel_thread_ctx_v1, 8",
        ".balign 8",
        "otel_thread_ctx_v1:",
        ".zero  8",
        ".previous",
    );

    /// Return the address of the current thread's `otel_thread_ctx_v1` TLS slot, resolved through
    /// the TLSDESC dialect.
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    unsafe fn tls_slot() -> *mut *mut ThreadContextRecord {
        let ptr: usize;
        // WARNING: keep the assembly below in the canonical compiler-emitted TLSDESC form. Linkers
        // rely on these exact relocation-bearing instruction patterns for TLS relaxation,
        // especially when this crate is linked statically. Harmless-looking rewrites can hide part
        // of the sequence from the linker and produce a partially relaxed access that computes an
        // invalid TLS address.
        //
        // This code match byte-per-byte what clang generates, and this is verified during tests.
        core::arch::asm!(
            "leaq otel_thread_ctx_v1@tlsdesc(%rip), %rax",
            "call *otel_thread_ctx_v1@TLSCALL(%rax)",
            "addq %fs:0, %rax",
            // There is a call instruction, but the whole point of TLSDESC is to use a fast calling
            // convention. GCC's x86-64 port assumes that FLAGS_REG and RAX are changed while all
            // other registers are preserved[^1]. LLVM similarly only clobbers RAX[^2] (and flags).
            // So we don't need to clobber additional registers or to use `clobber_abi` here (which
            // would negate most of the advantage of TLSDESC).
            //
            // [^1]: https://maskray.me/blog/2021-02-14-all-about-thread-local-storage
            // [^2]: https://raw.githubusercontent.com/llvm/llvm-project/main/llvm/lib/Target/X86/X86InstrCompiler.td
            out("rax") ptr,
            options(att_syntax),
        );
        ptr as *mut *mut ThreadContextRecord
    }

    /// Return the address of the current thread's `otel_thread_ctx_v1` TLS slot, resolved through
    /// the TLSDESC dialect.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    unsafe fn tls_slot() -> *mut *mut ThreadContextRecord {
        let ptr: usize;
        // WARNING: do not change the assembly below. See the warning above for amd64, and
        // https://github.com/ARM-software/abi-aa/blob/main/sysvabi64/sysvabi64.rst#general-dynamic.
        // This code match byte-per-byte what clang generates, and this is verified during tests.
        core::arch::asm!(
            "adrp  x0, :tlsdesc:otel_thread_ctx_v1",
            "ldr   x1, [x0, :tlsdesc_lo12:otel_thread_ctx_v1]",
            "add   x0, x0, :tlsdesc_lo12:otel_thread_ctx_v1",
            ".tlsdesccall otel_thread_ctx_v1",
            "blr   x1",
            "mrs   x8, tpidr_el0",
            "add   x0, x8, x0",
            out("x0") ptr,
            out("x1") _,
            out("x8") _,
            out("x30") _,
        );
        ptr as *mut *mut ThreadContextRecord
    }

    /// Run `f` with an atomic view of the current thread's TLS slot.
    ///
    /// The address calculation goes through the TLSDESC dialect via [`tls_slot`]. The returned
    /// address is stable (per thread), so callers should try to do as much work as possible
    /// inside a single call.
    ///
    /// The slot is read by an async signal handler. Atomic operations should in general use
    /// [Ordering::Relaxed], but modifications to the record might need additional compiler-only
    /// fences (see [ThreadContext::update] for an example).
    fn with_tls_slot<F, R>(f: F) -> R
    where
        F: FnOnce(&AtomicPtr<ThreadContextRecord>) -> R,
    {
        const {
            assert!(
                mem::align_of::<AtomicPtr<ThreadContextRecord>>()
                    == mem::align_of::<*mut ThreadContextRecord>()
            )
        }

        // Safety: the const assertion above ensures the alignment is correct. The TLS slot is
        // valid for the lifetime of the current thread, and all accesses go through the
        // `AtomicPtr` wrapper.
        let slot = unsafe { AtomicPtr::from_ptr(tls_slot()) };
        f(slot)
    }

    /// An owned (and non-moving) thread context record allocation.
    ///
    /// We don't use `Box` under the hood because it precludes aliasing, while we share the context
    /// to readers through thread-level context and through the FFI. But it is a boxed
    /// `ThreadContextRecord` for all intent and purpose.
    ///
    /// The context is `!Send` and `!Sync`; it is supposed to stay on the same thread and is thus
    /// not thread-safe.
    pub struct ThreadContext(NonNull<ThreadContextRecord>);

    /// Opaque handle to a thread context record. Used to allow the FFI to convert [ThreadContext]
    /// to and from raw pointers without exposing [ThreadContextRecord], as the latter needs extra
    /// care to be manipulated (async-signal-safety, seq-lock-like modification protocol through
    /// [ThreadContextRecord::valid], etc.)
    #[repr(C)]
    pub struct ThreadContextHandle {}

    impl ThreadContext {
        /// Create a new thread context with the given trace/span IDs and encoded attributes.
        pub fn new(
            trace_id: [u8; 16],
            span_id: [u8; 8],
            local_root_span_id: [u8; 8],
            attrs: &[(u8, &str)],
        ) -> Self {
            let mut record = ThreadContextRecord::default();
            record.initialize(trace_id, span_id, local_root_span_id, attrs.iter().copied());
            Self::from(record)
        }

        /// Turn this thread context into a pointer to the underlying [ThreadContextRecord].
        /// The pointer must be reconstructed through [`Self::from_ptr`] in order to be properly
        /// dropped, or the record will leak.
        fn into_ptr(self) -> NonNull<ThreadContextRecord> {
            let mdrop = mem::ManuallyDrop::new(self);
            mdrop.0
        }

        /// Turn this thread context into an opaque pointer to the underlying [ThreadContextRecord].
        /// The pointer must be reconstructed through [`Self::from_opaque_ptr`] in order to be
        /// properly dropped, or the record will leak.
        pub fn into_opaque_ptr(self) -> NonNull<ThreadContextHandle> {
            let mdrop = mem::ManuallyDrop::new(self);
            mdrop.0.cast()
        }

        /// Reconstruct a [ThreadContextRecord] from a pointer that comes
        /// from [`Self::into_ptr`].
        ///
        /// # Safety
        ///
        /// - `ptr` must come from a prior call to [`Self::into_ptr`].
        /// - if `ptr` is aliased, accesses through aliases must not be interleaved with method
        ///   calls on the returned [ThreadContextRecord]. More precisely, mutable references might
        ///   be reconstructed during those calls, so any constraint from either Stacked Borrows,
        ///   Tree Borrows or whatever is the current aliasing model implemented in Miri applies.
        unsafe fn from_ptr(ptr: NonNull<ThreadContextRecord>) -> Self {
            Self(ptr)
        }

        /// Reconstruct an [OpaqueThreadContextRecord] from a pointer that comes from
        /// [`Self::into_opaque_ptr`].
        ///
        /// # Safety
        ///
        /// - `ptr` must come from a prior call to [`Self::into_opaque_ptr`].
        /// - if `ptr` is aliased, accesses through aliases must not be interleaved with method
        ///   calls on the returned [ThreadContextRecord]. More precisely, mutable references might
        ///   be reconstructed during those calls, so any constraint from either Stacked Borrows,
        ///   Tree Borrows or whatever is the current aliasing model implemented in Miri applies.
        pub unsafe fn from_opaque_ptr(ptr: NonNull<ThreadContextHandle>) -> Self {
            Self(ptr.cast())
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
            // Safety: a non-null value in the slot came from a prior `into_ptr` call.
            NonNull::new(slot.swap(tgt, Ordering::Relaxed))
                .map(|ptr| unsafe { ThreadContext::from_ptr(ptr) })
        }

        /// Publish a new (or previously detached) thread context record by writing its pointer
        /// into the TLS slot. Returns the previously attached context, if any.
        ///
        /// `valid` is already `1` since construction, so any reader that observes the new pointer
        /// also observes `valid = 1`.
        pub fn attach(self) -> Option<ThreadContext> {
            // [^tls-slot-ordering]: since we get back the previous context, we should in principle
            // use an `Acquire` (thus combining into an `AcqRel`) compiler fence to make sure we
            // don't get back a not-yet-initialized record.
            //
            // However, this thread (excluding the reader signal handler) is the only one to ever
            // _write_ to the context, so the store we load the value from automatically
            // happens-before (because it's sequenced-before) the swap.
            //
            // We still need a release fence to avoid exposing uninitialized memory to the handler.
            compiler_fence(Ordering::Release);
            with_tls_slot(|slot| Self::swap(slot, self.into_ptr().as_ptr()))
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
            with_tls_slot(|slot| {
                // Safety: a non-null value in the slot came from `into_ptr` (i.e. `Box::into_raw`),
                // and only this thread ever writes to the slot, so the pointer is valid and not
                // accessed for the duration of this closure.
                if let Some(current) = unsafe { slot.load(Ordering::Relaxed).as_mut() } {
                    current.update(trace_id, span_id, local_root_span_id, attrs.iter().copied());
                } else {
                    let ctxt = ThreadContext::new(trace_id, span_id, local_root_span_id, attrs)
                        .into_ptr()
                        .as_ptr();
                    // No need for `AcqRel`, see [^tls-slot-ordering].
                    compiler_fence(Ordering::Release);
                    // `ThreadContext::new` already initialises `valid = 1`.
                    let _ = Self::swap(slot, ctxt);
                }
            })
        }

        /// Detach the current record from the TLS slot. Writes null to the slot and returns the
        /// detached record.
        pub fn detach() -> Option<ThreadContext> {
            // We don't need any fence here, see [^tls-slot-ordering].
            with_tls_slot(|slot| Self::swap(slot, ptr::null_mut()))
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
    // The tests are set to be ignored by Miri, since the inline-asm TLSDESC access isn't supported.
    mod tests {
        use super::{ThreadContext, ThreadContextRecord};
        use std::sync::atomic::Ordering;

        /// Read the TLS pointer for the current thread (the value stored in the TLS slot, not the
        /// address of the slot itself).
        fn read_tls_context_ptr() -> *const ThreadContextRecord {
            super::with_tls_slot(|slot| slot.load(Ordering::Relaxed))
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
