// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A single-writer / multiple-reader shared-memory channel.
//!
//! A writer publishes an opaque byte buffer into a shared-memory segment; any
//! number of readers (typically forked child processes that inherit the
//! mapping, or processes that open the same named segment) observe the latest
//! buffer. Consistency is provided by a generation counter (odd = mid-write,
//! even = stable) and a double-copy with an acquire fence.
//!
//! [`OneWayShmReader::wait_for_change`] lets a reader block until the writer
//! publishes new data, rather than busy-polling. On Linux this is a `futex`
//! wait/wake on the low 32 bits of the shared generation counter — an
//! inexpensive, signal-free cross-process notification. Because the generation
//! only ever increments, its low 32 bits are a sufficient wait word (no separate
//! notify field is needed). On other platforms the wait degrades to a timed
//! sleep so callers effectively poll. The wait always takes a timeout, so
//! callers still get periodic wakeups even when the data is unchanged.

use crate::platform::{FileBackedHandle, MappedMem, NamedShmHandle, ShmHandle};
use libdd_common::MutexExt;
use std::ffi::{CStr, CString};
use std::io;
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub struct OneWayShmWriter<T>
where
    T: FileBackedHandle + From<MappedMem<T>>,
{
    handle: Mutex<MappedMem<T>>,
}

pub type OneWayShmOpener<T, D> = fn(&D) -> Option<MappedMem<T>>;

pub struct OneWayShmReader<T, D>
where
    T: FileBackedHandle + From<MappedMem<T>>,
{
    handle: Option<MappedMem<T>>,
    current_data: Option<Vec<u64>>,
    last_wait_generation: u32,
    // Optional re-opener: lazily (re)maps the segment when the handle is absent
    // (e.g. a named segment opened by path). Readers over an inherited anonymous
    // mapping leave this `None`. A fn pointer keeps the reader `Send`/`Sync`
    // without a trait impl (which would be orphan-illegal for foreign `D`).
    opener: Option<OneWayShmOpener<T, D>>,
    pub extra: D,
}

#[repr(C)]
#[derive(Debug)]
struct RawMetaData {
    generation: AtomicU64,
    size: usize,
}

#[repr(C)]
#[derive(Debug)]
struct RawData {
    meta: RawMetaData,
    buf: [u8],
}

impl RawData {
    fn as_slice(&self) -> &[u8] {
        // Safety: size is expected to be truthful
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr(), self.meta.size) }
    }

    fn as_slice_mut(&mut self) -> &mut [u8] {
        // Safety: size is expected to be truthful
        unsafe { std::slice::from_raw_parts_mut(self.buf.as_mut_ptr(), self.meta.size) }
    }
}

impl From<&[u64]> for &RawData {
    fn from(value: &[u64]) -> Self {
        // Safety: MappedMem is supposed to be big enough
        // Safety: u64 is aligned
        unsafe { &*(value as *const [u64] as *const RawData) }
    }
}

// Safety: Caller needs to ensure the u8 is 8 byte aligned
unsafe fn reinterpret_u8_as_u64_slice(slice: &[u8]) -> &[u64] {
    // Safety: given 8 byte alignment, it's guaranteed to be readable
    std::slice::from_raw_parts(slice.as_ptr() as *const u64, slice.len().div_ceil(8))
}

// The `futex`-based wakeup is gated behind the `one_way_shm_futex` feature (and
// Linux, where cross-process `futex` on shared memory is supported). When it is
// disabled, `wait_for_change` falls back to a timed sleep (callers poll) and
// `write` skips the wake; for consumers like PHP that desire out of band
// notification, we can skip the futex_wake syscall overhead.
//
// `addr` points to the 32-bit wait word (the low 32 bits of the generation
// counter). It must be 4-byte aligned and live in shared memory.
#[cfg(all(
    feature = "one_way_shm_futex",
    target_os = "linux",
    target_endian = "little"
))]
fn futex_wake(addr: *const u32) {
    // FUTEX_WAKE (non-private) on a shared mapping wakes waiters across
    // processes. i32::MAX => wake all waiters.
    unsafe {
        libc::syscall(libc::SYS_futex, addr, libc::FUTEX_WAKE, i32::MAX);
    }
}

#[cfg(all(
    feature = "one_way_shm_futex",
    target_os = "linux",
    target_endian = "little"
))]
fn futex_wait(addr: *const u32, expected: u32, timeout: Duration) {
    let ts = libc::timespec {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_nsec: timeout.subsec_nanos() as libc::c_long,
    };
    // FUTEX_WAIT atomically checks `*addr == expected` and sleeps if so; returns
    // immediately (EAGAIN) otherwise. Spurious wakeups are fine — the caller
    // re-checks the generation.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr,
            libc::FUTEX_WAIT,
            expected as libc::c_int,
            &ts as *const libc::timespec,
        );
    }
}

#[cfg(not(all(
    feature = "one_way_shm_futex",
    target_os = "linux",
    target_endian = "little"
)))]
fn futex_wake(_addr: *const u32) {}

#[cfg(not(all(
    feature = "one_way_shm_futex",
    target_os = "linux",
    target_endian = "little"
)))]
fn futex_wait(_addr: *const u32, _expected: u32, timeout: Duration) {
    // No futex (feature disabled or unsupported platform); sleep so callers poll
    // the generation at the requested cadence.
    std::thread::sleep(timeout);
}

/// Create a writer backed by a fresh anonymous shared-memory segment, returning
/// the writer and a clonable [`ShmHandle`] to the same segment.
///
/// The handle can be mapped by readers in the same process or inherited by
/// forked children (an anonymous mapping survives `fork`), letting them build a
/// [`OneWayShmReader`] over what this writer publishes. The segment starts at one
/// page and grows on demand as larger buffers are written. Use
/// [`OneWayShmWriter::new`] instead when readers attach by name rather than
/// inheriting the mapping.
pub fn create_anon_pair() -> anyhow::Result<(OneWayShmWriter<ShmHandle>, ShmHandle)> {
    let handle = ShmHandle::new(0x1000)?;
    Ok((
        OneWayShmWriter {
            handle: Mutex::new(handle.clone().map()?),
        },
        handle,
    ))
}

impl<T: FileBackedHandle + From<MappedMem<T>>, D> OneWayShmReader<T, D> {
    /// Create a reader over an already-mapped segment.
    ///
    /// `handle` is the live mapping to read from — typically an anonymous segment
    /// inherited across a `fork`. Passing `None` leaves the reader without a
    /// mapping; since this constructor installs no opener, such a reader stays
    /// inert (empty reads, `wait_for_change` sleeps) until a handle is supplied —
    /// use [`Self::new_with_opener`] when the segment should be opened lazily.
    /// `extra` is arbitrary caller state carried alongside the reader.
    pub fn new(handle: MappedMem<T>, extra: D) -> OneWayShmReader<T, D> {
        OneWayShmReader {
            handle: Some(handle),
            current_data: None,
            last_wait_generation: 0,
            opener: None,
            extra,
        }
    }

    /// Like [`Self::new`], but with a re-opener used to (re)map the segment when
    /// the handle is absent (typically a named segment opened from `extra`).
    pub fn new_with_opener(
        handle: Option<MappedMem<T>>,
        extra: D,
        opener: OneWayShmOpener<T, D>,
    ) -> OneWayShmReader<T, D> {
        OneWayShmReader {
            handle,
            current_data: None,
            last_wait_generation: 0,
            opener: Some(opener),
            extra,
        }
    }

    fn try_open(&self) -> Option<MappedMem<T>> {
        self.opener.and_then(|open| open(&self.extra))
    }

    /// Returns the generation of the last successfully read data, or 0 if nothing has been read.
    pub fn last_read_generation(&self) -> u64 {
        self.current_data
            .as_ref()
            .map(|d| {
                let source_data: &RawData = d.as_slice().into();
                source_data.meta.generation.load(Ordering::Acquire)
            })
            .unwrap_or(0)
    }
}

impl OneWayShmWriter<ShmHandle> {
    /// Consume the writer, unmapping it and returning a handle to the segment —
    /// for a forked child (or any consumer) that no longer needs to write and
    /// just wants to hand the segment to a reader. No extra handle clones linger.
    pub fn into_handle(self) -> ShmHandle {
        self.handle
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .into()
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> OneWayShmWriter<T> {
    /// Create a writer backed by a named shared-memory segment at `path`.
    ///
    /// The segment is created and mapped under the given name so that unrelated
    /// processes can attach to it by opening the same path (see
    /// [`open_named_shm`]). Prefer [`create_anon_pair`] when readers inherit the
    /// mapping across a `fork` rather than opening it by name.
    pub fn new(path: CString) -> io::Result<OneWayShmWriter<NamedShmHandle>> {
        Ok(OneWayShmWriter {
            handle: Mutex::new(NamedShmHandle::create(path, 0x1000)?.map()?),
        })
    }
}

pub fn open_named_shm(path: &CStr) -> io::Result<MappedMem<NamedShmHandle>> {
    NamedShmHandle::open(path)?.map()
}

fn skip_last_byte(slice: &[u8]) -> &[u8] {
    if slice.is_empty() {
        slice
    } else {
        &slice[..slice.len() - 1]
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>, D> OneWayShmReader<T, D> {
    /// Read the latest published buffer.
    ///
    /// Returns `(changed, data)`. `changed` is `true` only when the writer
    /// published a newer generation than the previous `read` returned, so callers
    /// can cheaply skip re-parsing unchanged data. `data` always points at the
    /// most recent stable buffer (empty before the first successful read). A write
    /// observed mid-flight (odd generation) or one that races the double-copy
    /// yields the previously-read buffer with `changed == false`; the next `read`
    /// picks it up. If the reader has no mapping yet, it is opened lazily via the
    /// opener installed by [`Self::new_with_opener`] (if any).
    pub fn read<'a>(&'a mut self) -> (bool, &'a [u8]) {
        if let Some(ref handle) = self.handle {
            let source_data: &RawData =
                unsafe { reinterpret_u8_as_u64_slice(handle.as_slice()) }.into();
            let new_generation = source_data.meta.generation.load(Ordering::Acquire);

            let fetch_data = |reader: &'a mut OneWayShmReader<T, D>| {
                let size = std::mem::size_of::<RawMetaData>() + source_data.meta.size;

                #[allow(clippy::unwrap_used)]
                let handle = reader.handle.as_mut().unwrap();
                handle.ensure_space(size);

                // aligned on 8 byte boundary, round up to closest 8 byte boundary
                let mut new_mem = Vec::<u64>::with_capacity(size.div_ceil(8));
                new_mem.extend_from_slice(unsafe {
                    reinterpret_u8_as_u64_slice(&handle.as_slice()[0..size])
                });

                // refetch, might have been resized
                let source_data: &RawData =
                    unsafe { reinterpret_u8_as_u64_slice(handle.as_slice()) }.into();
                let copied_data: &RawData = new_mem.as_slice().into();

                // Ensure a new write hasn't started yet
                // Note that we actually care about is "dmb ishld" on ARM being emitted.
                fence(Ordering::Acquire); // prevent loads before from being reordered with gen load after
                if new_generation == source_data.meta.generation.load(Ordering::Relaxed) {
                    reader.current_data.replace(new_mem);
                    return Some((true, skip_last_byte(copied_data.as_slice())));
                }
                None
            };

            if let Some(cur_mem) = &self.current_data {
                let cur_data: &RawData = cur_mem.as_slice().into();

                if new_generation & 1 == 1 {
                    // mid-write
                    return (false, skip_last_byte(cur_data.as_slice()));
                }

                if new_generation > cur_data.meta.generation.load(Ordering::Relaxed) {
                    if let Some(success) = fetch_data(self) {
                        return success;
                    }
                }

                return (false, skip_last_byte(cur_data.as_slice()));
            } else {
                // first read

                if new_generation & 1 == 1 {
                    // mid-write
                    return (false, "".as_bytes());
                }

                if let Some(success) = fetch_data(self) {
                    return success;
                }
            }
        } else if let Some(handle) = self.try_open() {
            self.handle.replace(handle);
            return self.read();
        }

        (false, "".as_bytes())
    }

    /// Block until the writer publishes new data (advances the generation
    /// counter) or `timeout` elapses. Returns `true` if the generation advanced
    /// since the previous call, `false` on timeout.
    ///
    /// On Linux this is a `futex` wait on the low 32 bits of the shared
    /// generation counter; elsewhere it degrades to a `timeout` sleep (the caller
    /// then polls via [`Self::read`]).
    pub fn wait_for_change(&mut self, timeout: Duration) -> bool {
        if self.handle.is_none() {
            if let Some(handle) = self.try_open() {
                self.handle.replace(handle);
            } else {
                std::thread::sleep(timeout);
                return false;
            }
        }

        // Raw pointer to the generation atomic inside the live mapping. It lives
        // in the first page (before the resizable buffer), so its address is
        // stable for the duration of the wait. `wait_for_change` and `read` are
        // only ever called from the same (reader) thread, so no concurrent remap
        // can invalidate this pointer.
        let generation_ptr = {
            let Some(ref handle) = self.handle else {
                return false;
            };
            let data: &RawData = unsafe { reinterpret_u8_as_u64_slice(handle.as_slice()) }.into();
            data.meta.generation.as_ptr().cast::<u32>()
        };
        let generation = unsafe { AtomicU32::from_ptr(generation_ptr) };

        let current = generation.load(Ordering::Acquire);
        if current != self.last_wait_generation {
            self.last_wait_generation = current;
            return true;
        }

        futex_wait(generation_ptr, current, timeout);

        let after = generation.load(Ordering::Acquire);
        let changed = after != self.last_wait_generation;
        self.last_wait_generation = after;
        changed
    }

    /// Drop the current mapping.
    ///
    /// A subsequent [`Self::read`] or [`Self::wait_for_change`] re-maps the
    /// segment through the opener installed by [`Self::new_with_opener`] (if any);
    /// without an opener the reader becomes inert until a new handle is supplied.
    /// Useful when the backing segment may have been replaced and must be reopened
    /// from scratch.
    pub fn clear_reader(&mut self) {
        self.handle.take();
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> OneWayShmWriter<T> {
    /// Publish `contents` as the new current buffer, replacing the previous one.
    ///
    /// Writers are single-producer: the generation counter is bumped to odd
    /// before the copy and back to even afterwards (with release ordering) so a
    /// reader never observes a torn buffer — one racing the write either retries
    /// or keeps its prior copy. The segment grows if `contents` doesn't fit, and a
    /// trailing NUL is appended (to keep C consumers happy) that is not part of
    /// the data readers see. When built with the `one_way_shm_futex` feature on
    /// Linux this also wakes readers blocked in
    /// [`OneWayShmReader::wait_for_change`]; the wake is a cheap no-op syscall when
    /// there are no waiters.
    pub fn write(&self, contents: &[u8]) {
        let mut mapped = self.handle.lock_or_panic();

        let size = contents.len() + 1; // trailing zero byte, to keep some C code happy
        mapped.ensure_space(std::mem::size_of::<RawMetaData>() + size);

        // Safety: ShmHandle is always big enough
        // Actually &mut mapped.as_slice_mut() as RawData seems safe, but unsized locals are
        // unstable
        let data = unsafe { &mut *(mapped.as_slice_mut() as *mut [u8] as *mut RawData) };
        data.meta.generation.fetch_add(1, Ordering::Acquire);
        data.meta.size = size;

        data.as_slice_mut()[0..contents.len()].copy_from_slice(contents);
        data.as_slice_mut()[contents.len()] = 0;

        data.meta.generation.fetch_add(1, Ordering::Release);

        // Wake any readers blocked in `wait_for_change` on the generation word.
        // A wake with no waiters is a cheap no-op syscall.
        futex_wake((&data.meta.generation as *const AtomicU64).cast::<u32>());
    }

    /// Borrow the buffer currently published in the segment (excluding the
    /// trailing NUL), or an empty slice if nothing has been written yet.
    ///
    /// This reads the writer's own mapping directly and performs no
    /// generation/consistency handshake — unlike [`OneWayShmReader::read`] — so
    /// only call it from the writing side where no concurrent `write` is in
    /// flight.
    pub fn as_slice(&self) -> &[u8] {
        let mapped = self.handle.lock_or_panic();
        let data = unsafe { &*(mapped.as_slice() as *const [u8] as *const RawData) };
        if data.meta.size > 0 {
            let slice = data.as_slice();
            &slice[..slice.len() - 1] // ignore the trailing zero
        } else {
            b""
        }
    }

    /// The size in bytes of the writer's current mapping.
    ///
    /// This is the full mapped region (metadata header plus any slack left by
    /// growth), not the length of the published payload — use [`Self::as_slice`]
    /// for the latter.
    pub fn size(&self) -> usize {
        self.handle.lock_or_panic().as_slice().len()
    }

    /// The current value of the segment's generation counter.
    ///
    /// It advances on every [`Self::write`] (by two per completed write: odd while
    /// writing, even when stable), so it is mainly useful for diagnostics and
    /// tests. Its low 32 bits double as the `futex` wait word readers block on in
    /// [`OneWayShmReader::wait_for_change`].
    pub fn current_generation(&self) -> u64 {
        let mapped = self.handle.lock_or_panic();
        let data = unsafe { &*(mapped.as_slice() as *const [u8] as *const RawData) };
        data.meta.generation.load(Ordering::Acquire)
    }
}
