// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Thread contexts shared between threads, provided by the `shared-context` feature.

use std::{ptr::NonNull, sync::Arc};

use super::{ThreadContext, ThreadContextRecord};

/// A thread-level context shared immutably across threads.
///
/// Unlike `OwnedThreadContext`, a shared context holds its record behind an `Arc<ThreadContext>`.
/// The record can't be mutated in place (there is no `update` method). Attaching and detaching only
/// publish or retract the record pointer in the current thread's TLS slot while updating the `Arc`
/// pointer to ensure the record stays alive for exactly as long as it is attached somewhere.
///
/// Since attaching is an atomic swap, and a shared context is immutable once created, it can be
/// moved freely between threads. Several threads can also attach the same shared context
/// concurrently and their readers can observe it at the same time. The type is therefore `Send` and
/// `Sync`, in contrast with `OwnedThreadContext`.
///
/// The wrapped `Arc<ThreadContext>` is recoverable through the [`From`] conversions in both
/// directions, so callers can build, store, clone or share the context through their own machinery.
///
/// # Mixing with `OwnedThreadContext`
///
/// The TLS slot is untyped, it only holds a record pointer. [`Self::detach`] (and the previous
/// context returned by [`Self::attach`]) assumes the attached record, if any, is a shared one and
/// reconstructs an `Arc` from it. **A single thread must therefore never interleave owned and
/// shared attaches on the same slot**, or detaching would misinterpret the pointer. Similarly, one
/// should never call `OwnedThreadContext::update` after installing a shared context.
///
/// This is enforced by crate-level features:
///
/// - by default, `owned-context` is enabled, providing only `OwnedThreadContext`.
/// - if the feature `shared-context` is enabled instead, the crate provides only
///   [`SharedThreadContext`].
/// - if both features are enabled, `owned-context` wins (so that building the workspace with all
///   features still compiles the FFI, which needs the owned mode), and the build script emits a
///   warning.
///
/// Cloning is cheap: it only bumps the underlying `Arc`'s strong count. The record itself is shared
/// immutably, so several clones can be attached on different threads concurrently.
#[derive(Clone)]
pub struct SharedThreadContext(Arc<ThreadContext>);

impl From<Arc<ThreadContext>> for SharedThreadContext {
    fn from(inner: Arc<ThreadContext>) -> Self {
        Self(inner)
    }
}

impl From<SharedThreadContext> for Arc<ThreadContext> {
    fn from(ctx: SharedThreadContext) -> Self {
        ctx.0
    }
}

impl SharedThreadContext {
    /// Reconstruct a [`SharedThreadContext`] from the record pointer stored in the TLS slot,
    /// reclaiming the strong reference that [`Self::attach`] moved into the slot.
    ///
    /// # Safety
    ///
    /// `ptr` must originate from a prior [`Self::attach`] (i.e. it is the record pointer of an
    /// `Arc<ThreadContext>` whose strong reference was moved into the slot), and that reference
    /// must not have been reclaimed yet.
    unsafe fn from_record_ptr(ptr: NonNull<ThreadContextRecord>) -> Self {
        // `ThreadContext` is `repr(transparent)` over `ThreadContextRecord`, so the record
        // pointer is exactly the `*const ThreadContext` that `Arc::into_raw` produced.
        Self(Arc::from_raw(ptr.as_ptr() as *const ThreadContext))
    }

    /// Publish this shared context. Write its record pointer into the current thread's TLS slot.
    /// The underlying `Arc` is moved into the slot, so the record stays alive for as long as it is
    /// attached.
    ///
    /// Returns the previously attached shared context, if any.
    pub fn attach(self) -> Option<SharedThreadContext> {
        // Move the strong reference into the TLS slot; it stays alive until detached.
        // `ThreadContext` is `repr(transparent)` over `ThreadContextRecord`, so the data pointer is
        // also the record pointer readers expect.
        //
        // Though our `SharedThreadContext` might actually come from a different thread now, it's
        // wrapped in an `Arc` that handles drop safety by synchronizing on the reference count. The
        // record is already `valid = 1` and, being shared, immutable.
        let record_ptr = Arc::into_raw(self.0) as *mut ThreadContextRecord;
        // Safety: a non-null value in the slot came from a prior `attach` of a shared
        // context (see the type-level note on not mixing owned and shared attaches).
        NonNull::new(ThreadContextRecord::attach_raw(record_ptr))
            .map(|ptr| unsafe { Self::from_record_ptr(ptr) })
    }

    /// Detach the currently attached shared context from the TLS slot and return it.
    ///
    /// Returns `None` if the slot was empty.
    pub fn detach() -> Option<SharedThreadContext> {
        // Safety: a non-null value in the slot came from a prior `attach` of a shared context (see
        // the type-level note on not mixing owned and shared attaches).
        NonNull::new(ThreadContextRecord::detach_raw())
            .map(|ptr| unsafe { Self::from_record_ptr(ptr) })
    }
}

#[cfg(test)]
// The tests are set to be ignored by Miri, since the inline-asm TLSDESC access isn't supported.
mod tests {
    use super::{SharedThreadContext, ThreadContext};
    use crate::linux::read_tls_context_ptr;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    const NO_TRACE_FLAGS: u8 = 0;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn shared_attach_detach_lifecycle() {
        let trace_id = [4u8; 16];
        let span_id = [5u8; 8];
        let root_span_id = [6u8; 8];

        let cell: Arc<ThreadContext> = Arc::new(ThreadContext::new(
            trace_id,
            span_id,
            NO_TRACE_FLAGS,
            root_span_id,
            &[],
        ));

        assert!(read_tls_context_ptr().is_null());
        assert_eq!(Arc::strong_count(&cell), 1);

        // Attaching moves a (cloned) strong reference into the slot and publishes the record.
        let prev = SharedThreadContext::from(Arc::clone(&cell)).attach();
        assert!(prev.is_none(), "nothing was attached before");
        assert_eq!(
            Arc::strong_count(&cell),
            2,
            "attach must keep a strong reference alive in the slot"
        );

        let ptr = read_tls_context_ptr();
        assert!(!ptr.is_null(), "TLS must be set after attach");
        let record = unsafe { &*ptr };
        assert_eq!(record.trace_id, trace_id);
        assert_eq!(record.span_id, span_id);
        assert_eq!(record.valid.load(Ordering::Relaxed), 1);

        // Detaching gives the shared context back and clears the slot.
        let detached = SharedThreadContext::detach().expect("a context must be attached");
        assert!(
            read_tls_context_ptr().is_null(),
            "TLS must be null after detach"
        );
        // `detached` still holds the reference that was in the slot: cell + detached = 2.
        assert_eq!(Arc::strong_count(&cell), 2);

        drop(detached);
        assert_eq!(
            Arc::strong_count(&cell),
            1,
            "dropping the detached context must release the bumped reference"
        );

        // Detaching again is a no-op.
        assert!(SharedThreadContext::detach().is_none());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn shared_attach_replaces_previous() {
        let first: Arc<ThreadContext> = Arc::new(ThreadContext::new(
            [7u8; 16],
            [8u8; 8],
            NO_TRACE_FLAGS,
            [9u8; 8],
            &[],
        ));
        let second: Arc<ThreadContext> = Arc::new(ThreadContext::new(
            [0xAu8; 16],
            [0xBu8; 8],
            NO_TRACE_FLAGS,
            [0xCu8; 8],
            &[],
        ));

        let prev = SharedThreadContext::from(Arc::clone(&first)).attach();
        assert!(prev.is_none(), "nothing was attached before");

        // Attaching a second context returns the first one and publishes the second.
        let prev = SharedThreadContext::from(Arc::clone(&second))
            .attach()
            .expect("must return the previously attached context");
        let record = unsafe { &*read_tls_context_ptr() };
        assert_eq!(record.trace_id, [0xAu8; 16]);
        assert_eq!(Arc::strong_count(&second), 2, "second is now in the slot");

        // The returned previous context still holds `first`'s slot reference.
        assert_eq!(Arc::strong_count(&first), 2);
        drop(prev);
        assert_eq!(Arc::strong_count(&first), 1);

        let _ = SharedThreadContext::detach();
        assert!(read_tls_context_ptr().is_null());
        assert_eq!(Arc::strong_count(&second), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn shared_arc_round_trip() {
        let cell: Arc<ThreadContext> = Arc::new(ThreadContext::default());
        let shared = SharedThreadContext::from(Arc::clone(&cell));
        let back: Arc<ThreadContext> = shared.into();
        assert!(
            Arc::ptr_eq(&cell, &back),
            "round-trip must preserve the allocation"
        );
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
            SharedThreadContext::from(Arc::new(ThreadContext::new(
                spawned_trace_id,
                spawned_span_id,
                0,
                spawned_root_span_id,
                &[],
            )))
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

            let _ = SharedThreadContext::detach();
            assert!(read_tls_context_ptr().is_null());
        });

        // Wait for the spawned thread to attach its record, then attach our own.
        barrier.wait();

        assert!(
            read_tls_context_ptr().is_null(),
            "main thread should see a null pointer and not another thread's context"
        );

        SharedThreadContext::from(Arc::new(ThreadContext::new(
            main_trace_id,
            main_span_id,
            0,
            main_root_span_id,
            &[],
        )))
        .attach();

        let ptr = read_tls_context_ptr();
        assert!(!ptr.is_null(), "main thread TLS must be set");
        let record = unsafe { &*ptr };
        assert_eq!(record.trace_id, main_trace_id);
        assert_eq!(record.span_id, main_span_id);
        assert_eq!(&record.attrs_data[2..18], b"33445566778899aa");

        barrier.wait();

        let _ = SharedThreadContext::detach();
        assert!(read_tls_context_ptr().is_null());

        handle.join().unwrap();
    }

    // A single shared context (`Arc<ThreadContext>`) attached on several threads at the same
    // time: every thread must observe the very same record through its own TLS slot.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn shared_context_attached_on_multiple_threads() {
        use std::sync::Barrier;

        const THREADS: usize = 4;

        let trace_id = [0x5Au8; 16];
        let span_id = [0x6Bu8; 8];
        let root_span_id = [0x7Cu8; 8];

        let cell: Arc<ThreadContext> = Arc::new(ThreadContext::new(
            trace_id,
            span_id,
            NO_TRACE_FLAGS,
            root_span_id,
            &[],
        ));

        // `ThreadContext` is `repr(transparent)` over the internal record, so the `Arc` data
        // pointer is also the record pointer each thread's TLS slot must hold.
        let record_addr = Arc::as_ptr(&cell) as usize;

        let barrier = Arc::new(Barrier::new(THREADS + 1));

        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let shared = SharedThreadContext::from(cell.clone());
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    assert!(shared.attach().is_none(), "nothing was attached before");

                    let ptr = read_tls_context_ptr();
                    assert!(!ptr.is_null(), "thread {i}: TLS must be set after attach");
                    // All threads attached clones of the same `Arc`: they must all observe the
                    // very same record.
                    assert_eq!(ptr as usize, record_addr, "thread {i}: same record");
                    // Safety: the record is immutable once initialized (not data race possible) and
                    // is alive as long as it is still attached.
                    let record = unsafe { &*ptr };
                    assert_eq!(record.trace_id, trace_id, "thread {i}");
                    assert_eq!(record.span_id, span_id, "thread {i}");
                    assert_eq!(record.valid.load(Ordering::Relaxed), 1, "thread {i}");
                    assert_eq!(&record.attrs_data[2..18], b"7c7c7c7c7c7c7c7c", "thread {i}");

                    // All threads are now attached concurrently. Wait for everyone to observe
                    // the record, then for the main thread to check the strong count, so that
                    // nobody detaches too early.
                    barrier.wait();
                    barrier.wait();

                    let detached = SharedThreadContext::detach().expect("a context to detach");
                    assert!(
                        read_tls_context_ptr().is_null(),
                        "thread {i}: slot must be empty"
                    );
                    // The detached handle still wraps the same shared record.
                    let back: Arc<ThreadContext> = detached.into();
                    assert_eq!(Arc::as_ptr(&back) as usize, record_addr, "thread {i}");
                    drop(back);
                })
            })
            .collect();

        // Wait until every thread has attached and observed the shared record.
        barrier.wait();

        // While all threads are attached, each slot holds its own strong reference to the
        // record, on top of the main one.
        assert_eq!(Arc::strong_count(&cell), 1 + THREADS);

        // Let the threads detach and drop their handles.
        barrier.wait();

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            Arc::strong_count(&cell),
            1,
            "only the main reference must remain once every thread detached"
        );
    }
}
