// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    convert::TryInto,
    marker::PhantomData,
    mem::replace,
    ptr::{self, NonNull},
    sync::atomic::{fence, AtomicPtr, AtomicU32, AtomicU64, Ordering},
};
use std::{
    io,
    sync::{Mutex, MutexGuard},
};

use libdd_trace_protobuf::opentelemetry::proto::common::v1::ProcessContext;
use prost::Message;

use super::{PROCESS_CTX_VERSION, SIGNATURE, UNPUBLISHED_OR_UPDATING};

#[cfg(target_os = "linux")]
pub(super) mod linux;

/// The header structure written at the start of the mapping. This must match the C
/// layout of the specification.
///
/// The atomic fields have the same size and alignment as their corresponding C fields. They
/// provide the aligned word-sized accesses required by the publication protocol, while explicit
/// fences constrain store/load ordering.
#[repr(C)]
pub(super) struct MappingHeader {
    pub(super) signature: [u8; 8],
    pub(super) version: u32,
    pub(super) payload_size: AtomicU32,
    pub(super) monotonic_published_at_ns: AtomicU64,
    pub(super) payload_ptr: AtomicPtr<u8>,
}

// Compile-time verification that MappingHeader matches the field offsets and total size
// mandated by the OTel process context spec:
// https://github.com/open-telemetry/opentelemetry-specification/blob/main/oteps/profiles/4719-process-ctx.md
const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(offset_of!(MappingHeader, signature) == 0);
    assert!(offset_of!(MappingHeader, version) == 8);
    assert!(offset_of!(MappingHeader, payload_size) == 12);
    assert!(offset_of!(MappingHeader, monotonic_published_at_ns) == 16);
    assert!(offset_of!(MappingHeader, payload_ptr) == 24);
    assert!(size_of::<MappingHeader>() == 32);
    assert!(core::mem::align_of::<MappingHeader>() == 8);
    assert!(size_of::<*const u8>() == size_of::<usize>());
};

#[cfg(target_os = "linux")]
type HeaderMemory = linux::MemMapping;

#[cfg(target_os = "linux")]
type PlatformMonotonicClock = linux::MonotonicClock;

type ProcessContextHandle = ProcessContextHandleGen<HeaderMemory, PlatformMonotonicClock>;

/// The global instance of the context for the current process.
///
/// We need a mutex to put the handle in a static and avoid bothering the users of this API
/// with storing the handle, but we don't expect this mutex to actually be contended. Ideally a
/// single thread should handle context updates, even if it's not strictly required.
static PROCESS_CONTEXT_HANDLER: Mutex<Option<ProcessContextHandle>> = Mutex::new(None);

pub(super) trait HeaderMemoryHolder: Sized {
    fn new() -> io::Result<Self>;
    fn as_ptr(&self) -> Option<NonNull<MappingHeader>>;
    fn make_discoverable(&mut self);
    fn unpublish_and_release(self) -> io::Result<()>;
}

pub(super) trait MonotonicTime {
    fn monotonic_time_ns() -> io::Result<u64>;
}

/// Handle for future updates of a published process context.
struct ProcessContextHandleGen<M: HeaderMemoryHolder, T: MonotonicTime> {
    mapping: M,
    /// Once published, and until the next update is complete, the backing allocation of
    /// `payload` might be read and thus must not move (e.g. by resizing or drop).
    payload: Vec<u8>,
    monotonic_clock: PhantomData<T>,
}

impl<M: HeaderMemoryHolder, T: MonotonicTime> ProcessContextHandleGen<M, T> {
    /// Initial publication of the process context. Creates an appropriate header allocation.
    fn publish(payload: Vec<u8>) -> io::Result<Self> {
        let payload_size: u32 = payload
            .len()
            .try_into()
            .map_err(|_| io::Error::other("payload size overflowed"))?;

        let mut mapping = M::new()?;
        let published_at_ns = T::monotonic_time_ns()?;

        let header = mapping
            .as_ptr()
            // should never happen; as_ptr should only return None after a fork
            .ok_or_else(|| io::Error::other("new process context header mapping is unavailable"))?
            .as_ptr();

        // SAFETY: header points to a zero-filled, writable allocation of at least
        // mapping_size() bytes with MappingHeader alignment; field projections are in-bounds.
        // The pointer writes do not happen while there are live &MappingHeader references
        // and, to the extent the atomic stores do, this is fine because the mutated bytes
        // are inside UnsafeCells.
        unsafe {
            ptr::addr_of_mut!((*header).signature).write(*SIGNATURE);
            ptr::addr_of_mut!((*header).version).write(PROCESS_CTX_VERSION);
            (*header)
                .payload_ptr
                .store(payload.as_ptr().cast_mut(), Ordering::Relaxed);
            (*header)
                .payload_size
                .store(payload_size, Ordering::Relaxed);

            fence(Ordering::SeqCst);
            (*header)
                .monotonic_published_at_ns
                .store(published_at_ns, Ordering::Relaxed);
        }

        mapping.make_discoverable();

        Ok(ProcessContextHandleGen {
            mapping,
            payload,
            monotonic_clock: PhantomData,
        })
    }

    /// Updates the context after initial publication.
    fn update(&mut self, payload: Vec<u8>) -> io::Result<()> {
        let header = self
            .mapping
            .as_ptr()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "process context header mapping is unavailable after fork",
                )
            })?
            .as_ptr();

        let monotonic_published_at_ns = T::monotonic_time_ns()?;
        let payload_size: u32 = payload.len().try_into().map_err(|_| {
            io::Error::other("couldn't update process context: new payload too large")
        })?;
        // A process shouldn't try to concurrently update its own context.
        //
        // `UNPUBLISHED_OR_UPDATING` is an out-of-band sentinel, not a value that
        // the monotonic clock is expected to produce after publication. Published non-zero
        // timestamp values must advance monotonically; the field may temporarily hold the sentinel
        // while an update is in progress.
        //
        // Note: be careful of early return while `monotonic_published_at` is still zero, as
        // subsequent updates would get stuck.
        let last_monotonic_published_at_ns = unsafe {
            (*header)
                .monotonic_published_at_ns
                .swap(UNPUBLISHED_OR_UPDATING, Ordering::Relaxed)
        };
        if last_monotonic_published_at_ns == UNPUBLISHED_OR_UPDATING {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "context is already being updated",
            ));
        }

        let monotonic_published_at_ns =
            monotonic_published_at_ns.max(last_monotonic_published_at_ns.saturating_add(1));

        // Prevent the payload metadata and payload replacement below from moving above the
        // unavailable marker. In particular, if a reader starts from the previous non-zero
        // timestamp but copies data after this update begins, it must not accept that copy as the
        // previous version: its final timestamp check should see `UNPUBLISHED_OR_UPDATING` or the
        // later published timestamp.
        fence(Ordering::SeqCst);
        self.payload = payload;

        unsafe {
            (*header)
                .payload_ptr
                .store(self.payload.as_ptr().cast_mut(), Ordering::Relaxed);
            (*header)
                .payload_size
                .store(payload_size, Ordering::Relaxed);
        }

        // Prevent the final timestamp publication from moving above either the payload metadata
        // writes or the payload bytes written before this method was called. Readers fence after
        // observing this non-zero timestamp before copying both.
        fence(Ordering::SeqCst);

        unsafe {
            (*header)
                .monotonic_published_at_ns
                .store(monotonic_published_at_ns, Ordering::Relaxed);
        }

        Ok(())
    }
}

// The returned size is guaranteed to be larger or equal to the size of `MappingHeader`.
#[cfg(target_os = "linux")]
pub(super) const fn mapping_size() -> usize {
    core::mem::size_of::<MappingHeader>()
}

/// Locks the context handle. Returns a uniform error if the lock has been poisoned.
fn lock_context_handle() -> io::Result<MutexGuard<'static, Option<ProcessContextHandle>>> {
    PROCESS_CONTEXT_HANDLER.lock().map_err(|_| {
        io::Error::other("a thread panicked while operating on the process context handler")
    })
}

/// Publishes or updates the process context for it to be visible by external readers.
///
/// If any of the following condition holds:
///
/// - this is the first publication
/// - [unpublish] has been called last
/// - the previous header mapping is unavailable after `fork()`
///
/// Then we follow the Publish protocol of the OTel process context specification (allocating a
/// fresh header).
///
/// Otherwise, if a context has been previously published from the same process and hasn't been
/// unpublished since, we follow the Update protocol.
///
/// # Fork safety
///
/// If we're a forked children of the original publisher, we are extremely restricted in the
/// set of operations that we can do (we must be async-signal-safe). On paper, heap allocation
/// is Undefined Behavior, for example. We assume that a forking runtime (such as Python or
/// Ruby) that doesn't follow with an immediate `exec` is already "taking that risk", so to
/// speak (typically, if no thread is ever spawned before the fork, things are mostly fine).
#[inline]
pub fn publish(context: &ProcessContext) -> io::Result<()> {
    publish_raw_payload(context.encode_to_vec())
}

pub(super) fn publish_raw_payload(payload: Vec<u8>) -> io::Result<()> {
    let mut guard = lock_context_handle()?;

    match &mut *guard {
        Some(handler) if handler.mapping.as_ptr().is_some() => handler.update(payload),
        Some(handler) => {
            let new_handler = ProcessContextHandleGen::publish(payload)?;
            let _old_handler = replace(handler, new_handler);

            Ok(())
        }
        None => {
            *guard = Some(ProcessContextHandleGen::publish(payload)?);
            Ok(())
        }
    }
}

/// Removes the process context publication and releases its header allocation. If no context has
/// ever been published, this is a no-op.
///
/// A call to [publish] following an [unpublish] will create a new mapping.
pub fn unpublish() -> io::Result<()> {
    let mut guard = lock_context_handle()?;

    if let Some(ProcessContextHandleGen {
        mapping, payload, ..
    }) = guard.take()
    {
        if let Some(header) = mapping.as_ptr() {
            // Mark the context as unavailable before freeing the mapping/payload. The fence
            // forces the writing CPU not to reorder the unavailable timestamp store and the
            // deallocation stores. This gives readers more of a chance (but no guarantee) to
            // observe an unavailable context before the publication is removed.
            //
            // SAFETY: the mapping is still live and valid, and the global mutex prevents
            // concurrent in-process writers from mutating the plain header fields.
            let header = header.as_ptr();
            unsafe {
                (*header)
                    .monotonic_published_at_ns
                    .store(UNPUBLISHED_OR_UPDATING, Ordering::Relaxed);
            }
            fence(Ordering::SeqCst);
        }

        // The payload will still drop if this fails, leaving a zero timestamp behind.
        mapping.unpublish_and_release()?;
        drop(payload);
    }

    Ok(())
}
