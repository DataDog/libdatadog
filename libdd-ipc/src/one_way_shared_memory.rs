// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A single-writer / multiple-reader shared-memory channel.
//!
//! Readers copy the latest buffer and check the generation before and after copying, with
//! an acquire fence between the copy and the second check. Odd generations mean a write is
//! in progress, even generations are stable, and 0 means nothing has been published.
//!
//! Named segments have two terminal generations:
//! - [`RETIRED`]: a fresh segment replaced it under the same name. Readers reopen the name.
//! - [`ENDED`]: its writer went away. Readers take its last payload, then reopen the name.
//!
//! The first publication seeds the counter from monotonic microseconds.
//! Later writes increment it by two.
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
use libdd_common::{MutexExt, rate_limiter::now};
use std::ffi::{CStr, CString};
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::time::Duration;

/// Set after the replacement's first publication. The old payload may be incomplete.
/// Odd so readers that do not recognize this marker also refuse to read the payload.
pub const RETIRED: u64 = u64::MAX;

/// The writer has shut down. Its final payload is complete and must still be read,
/// including an empty payload that clears remote config or telemetry state.
pub const ENDED: u64 = u64::MAX - 1;

pub struct OneWayShmWriter<T>
where
    T: FileBackedHandle,
{
    state: Mutex<WriterState<T>>,
}

struct WriterState<T: FileBackedHandle> {
    mapped: MappedMem<T>,
    /// Retired after the first publication, or on drop if nothing was published.
    replaced: Vec<MappedMem<NamedShmHandle>>,
    /// Only the creator may end a named segment; forked copies must leave it alone.
    /// Anonymous segments have no owner PID.
    owner_pid: Option<u32>,
}

impl<T: FileBackedHandle> WriterState<T> {
    fn retire_replaced(&mut self) {
        for old in self.replaced.drain(..) {
            if let Some(meta) = meta(old.as_slice()) {
                meta.generation.store(RETIRED, Ordering::Release);
                futex_wake(meta.generation.as_ptr().cast());
            }
        }
    }
}

impl<T: FileBackedHandle> Drop for OneWayShmWriter<T> {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        if state.owner_pid != Some(std::process::id()) {
            return;
        }
        state.retire_replaced();
        let Some(meta) = meta(state.mapped.as_slice()) else {
            return;
        };
        // Retirement is final, including when it races this drop.
        let current = meta.generation.load(Ordering::Acquire);
        if current != RETIRED
            && meta
                .generation
                .compare_exchange(current, ENDED, Ordering::Release, Ordering::Relaxed)
                .is_ok()
        {
            futex_wake(meta.generation.as_ptr().cast());
        }
    }
}

pub type OneWayShmOpener<T, D> = fn(&D) -> Option<MappedMem<T>>;

pub struct OneWayShmReader<T, D>
where
    T: FileBackedHandle,
{
    handle: Option<MappedMem<T>>,
    current_data: Option<Vec<u64>>,
    /// The cached payload came from another segment, or from before [`Self::clear_reader`].
    resync: bool,
    /// Reports a segment change through `take_replaced`.
    replaced: bool,
    last_wait_generation: u32,
    // Anonymous readers have no opener and stay on the inherited mapping.
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

/// The header of a mapped segment, if the mapping is long enough to hold one.
fn meta(slice: &[u8]) -> Option<&RawMetaData> {
    if slice.len() < std::mem::size_of::<RawMetaData>() {
        return None;
    }
    // SAFETY: long enough as just checked, and mappings are page-aligned. Both fields are plain
    // integers (one of them atomic), valid for any bit pattern.
    Some(unsafe { &*slice.as_ptr().cast::<RawMetaData>() })
}

fn generation_of<T: FileBackedHandle>(mapped: &MappedMem<T>) -> Option<u64> {
    Some(meta(mapped.as_slice())?.generation.load(Ordering::Acquire))
}

// Safety: Caller needs to ensure the u8 is 8 byte aligned
unsafe fn reinterpret_u8_as_u64_slice(slice: &[u8]) -> &[u64] {
    unsafe {
        // Safety: given 8 byte alignment, it's guaranteed to be readable
        std::slice::from_raw_parts(slice.as_ptr() as *const u64, slice.len().div_ceil(8))
    }
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
        tv_sec: timeout.as_secs() as _,
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
    Ok((OneWayShmWriter::from_mapping(handle.clone().map()?), handle))
}

impl<T: FileBackedHandle, D> OneWayShmReader<T, D> {
    /// Create a reader over an already-mapped segment.
    ///
    /// `handle` is the live mapping to read from — typically an anonymous segment
    /// inherited across a `fork`. There is no opener, so the reader stays on this segment;
    /// use [`Self::new_with_opener`] for a named segment that may be replaced.
    /// `extra` is arbitrary caller state carried alongside the reader.
    pub fn new(handle: MappedMem<T>, extra: D) -> OneWayShmReader<T, D> {
        OneWayShmReader {
            handle: Some(handle),
            current_data: None,
            resync: false,
            replaced: false,
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
            resync: false,
            replaced: false,
            last_wait_generation: 0,
            opener: Some(opener),
            extra,
        }
    }

    fn try_open(&self) -> Option<MappedMem<T>> {
        self.opener.and_then(|open| open(&self.extra))
    }

    /// Start reading `handle`, a segment other than the one the cached payload came from.
    fn switch_to(&mut self, handle: MappedMem<T>) {
        self.handle = Some(handle);
        self.resync = self.current_data.is_some();
        self.last_wait_generation = 0;
    }

    fn attach(&mut self) -> bool {
        match self.try_open() {
            Some(handle) => {
                self.switch_to(handle);
                true
            }
            None => false,
        }
    }

    fn mapped_generation(&self) -> Option<u64> {
        generation_of(self.handle.as_ref()?)
    }

    /// Try the replacement once, keeping the cached payload if it is not ready.
    fn follow_retirement(&mut self) -> Option<u64> {
        self.handle = None;
        if !self.attach() {
            return None;
        }
        let generation = self.mapped_generation();
        if generation == Some(RETIRED) {
            self.handle = None;
            return None;
        }
        generation
    }

    /// Whether the cached payload is the final one of the mapped, [`ENDED`] segment.
    fn consumed_end(&self) -> bool {
        !self.resync
            && self.current_data.as_ref().is_some_and(|cur_mem| {
                let cur_data: &RawData = cur_mem.as_slice().into();
                cur_data.meta.generation.load(Ordering::Relaxed) == ENDED
            })
    }

    /// After reading the final payload, try to open an active successor.
    fn follow_end(&mut self) -> Option<u64> {
        let handle = self.try_open()?;
        let generation = generation_of(&handle).filter(|&g| g != RETIRED && g != ENDED)?;
        self.switch_to(handle);
        Some(generation)
    }

    /// Returns the generation of the last successfully read data, or 0 if nothing has been read.
    ///
    /// An [`ENDED`] segment reports 0 because it has no writer.
    pub fn last_read_generation(&self) -> u64 {
        self.current_data
            .as_ref()
            .map(|d| {
                let source_data: &RawData = d.as_slice().into();
                source_data.meta.generation.load(Ordering::Acquire)
            })
            .filter(|&generation| generation != ENDED)
            .unwrap_or(0)
    }

    /// Whether a payload was read from another segment since the last call.
    /// Consumers may need to rebuild derived state even if the payload is unchanged.
    pub fn take_replaced(&mut self) -> bool {
        std::mem::take(&mut self.replaced)
    }
}

impl OneWayShmWriter<ShmHandle> {
    /// Consume the writer, unmapping it and returning a handle to the segment —
    /// for a forked child (or any consumer) that no longer needs to write and
    /// just wants to hand the segment to a reader. No extra handle clones linger.
    pub fn into_handle(self) -> ShmHandle {
        let state = self.state.lock_or_panic();
        ShmHandle {
            handle: state.mapped.mem.handle.clone(),
            size: state.mapped.get_size(),
        }
    }
}

impl<T: FileBackedHandle> OneWayShmWriter<T> {
    fn from_mapping(mapped: MappedMem<T>) -> Self {
        OneWayShmWriter {
            state: Mutex::new(WriterState {
                mapped,
                replaced: vec![],
                owner_pid: None,
            }),
        }
    }
}

impl OneWayShmWriter<NamedShmHandle> {
    /// Create a writer backed by a named shared-memory segment at `path`.
    ///
    /// The segment is created and mapped under the given name so that unrelated
    /// processes can attach to it by opening the same path (see
    /// [`open_named_shm`]). Prefer [`create_anon_pair`] when readers inherit the
    /// mapping across a `fork` rather than opening it by name.
    ///
    /// A segment already at `path` is replaced by a fresh one and [`RETIRED`] once this writer
    /// has published. Dropping the writer marks its own segment [`ENDED`].
    pub fn new(path: CString) -> io::Result<Self> {
        let (handle, replaced) = NamedShmHandle::create_replacing(path, 0x1000)?;
        Ok(OneWayShmWriter {
            state: Mutex::new(WriterState {
                mapped: handle.map()?,
                replaced: replaced
                    .into_iter()
                    .filter_map(|old| old.map().ok())
                    .collect(),
                owner_pid: Some(std::process::id()),
            }),
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

impl<T: FileBackedHandle, D> OneWayShmReader<T, D> {
    /// Read the latest published buffer.
    ///
    /// Returns `(changed, data)`. A newer generation or a replacement's first payload counts
    /// as changed. An incomplete or concurrent write leaves the cached buffer unchanged;
    /// before the first successful read, that buffer is empty.
    ///
    /// If an opener was provided, missing mappings are opened lazily and terminal segments
    /// ([`RETIRED`], [`ENDED`]) are followed to their replacement at most once per call.
    pub fn read(&mut self) -> (bool, &[u8]) {
        if self.handle.is_none() && !self.attach() {
            return self.cached();
        }
        let Some(mut generation) = self.mapped_generation() else {
            return self.cached();
        };
        if generation & 1 == 1 {
            if generation != RETIRED {
                return self.cached(); // mid-write
            }
            match self.follow_retirement() {
                Some(new) if new & 1 == 0 => generation = new,
                _ => return self.cached(),
            }
        } else if generation == ENDED && self.consumed_end() {
            if let Some(new) = self.follow_end() {
                generation = new;
            }
        }

        let wanted = generation != 0
            && match &self.current_data {
                Some(cur_mem) if !self.resync => {
                    let cur_data: &RawData = cur_mem.as_slice().into();
                    generation > cur_data.meta.generation.load(Ordering::Relaxed)
                }
                _ => true,
            };
        if wanted && self.fetch(generation) {
            if std::mem::take(&mut self.resync) {
                self.replaced = true;
            }
            #[allow(clippy::unwrap_used)] // fetch() just stored it
            let copied: &RawData = self.current_data.as_ref().unwrap().as_slice().into();
            return (true, skip_last_byte(copied.as_slice()));
        }
        self.cached()
    }

    /// The payload read last, reported as unchanged.
    fn cached(&self) -> (bool, &[u8]) {
        match &self.current_data {
            Some(cur_mem) => {
                let cur_data: &RawData = cur_mem.as_slice().into();
                (false, skip_last_byte(cur_data.as_slice()))
            }
            None => (false, b""),
        }
    }

    /// Copy out the payload published at `generation`, and keep it if it stayed consistent.
    fn fetch(&mut self, generation: u64) -> bool {
        let Some(handle) = self.handle.as_mut() else {
            return false;
        };
        let Some(size) = meta(handle.as_slice())
            .and_then(|m| std::mem::size_of::<RawMetaData>().checked_add(m.size))
        else {
            return false;
        };

        // The shared size may exceed the backing; keep the cached payload on failure.
        if !handle.ensure_space(size) {
            return false;
        }

        // Keep the snapshot aligned for RawData; the mapping's backing is page-aligned.
        let new_mem = unsafe { reinterpret_u8_as_u64_slice(&handle.as_slice()[..size]) }.to_vec();

        // Finish copying before checking the generation again (dmb ishld on ARM).
        fence(Ordering::Acquire);
        if Some(generation) != meta(handle.as_slice()).map(|m| m.generation.load(Ordering::Relaxed))
        {
            return false;
        }
        self.current_data.replace(new_mem);
        true
    }

    /// Block until the writer publishes new data (advances the generation
    /// counter) or `timeout` elapses. Returns `true` if the generation advanced
    /// since the previous call, `false` on timeout.
    ///
    /// On Linux this is a `futex` wait on the low 32 bits of the shared
    /// generation counter; elsewhere it degrades to a `timeout` sleep (the caller
    /// then polls via [`Self::read`]). A segment no longer written is not waited on: the
    /// reader moves to its successor, like [`Self::read`] does.
    pub fn wait_for_change(&mut self, timeout: Duration) -> bool {
        if self.handle.is_none() && !self.attach() {
            std::thread::sleep(timeout);
            return false;
        }
        let moved = match self.mapped_generation() {
            Some(RETIRED) => Some(self.follow_retirement().is_some()),
            // The final payload is news until read; after that only a successor is.
            Some(ENDED) => Some(!self.consumed_end() || self.follow_end().is_some()),
            _ => None,
        };
        if let Some(moved) = moved {
            if !moved {
                std::thread::sleep(timeout);
            }
            return moved;
        }

        // The mapping stays at a fixed address and &mut self prevents concurrent replacement.
        let generation_ptr = {
            let Some(meta) = self.handle.as_ref().and_then(|h| meta(h.as_slice())) else {
                return false;
            };
            meta.generation.as_ptr().cast::<u32>()
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
    /// The next read or wait uses the opener to remap and accepts its first payload regardless
    /// of generation. Without an opener, the reader keeps only its cached payload.
    pub fn clear_reader(&mut self) {
        self.handle.take();
        self.resync = self.current_data.is_some();
        self.last_wait_generation = 0;
    }
}

impl<T: FileBackedHandle> OneWayShmWriter<T> {
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
    ///
    /// Returns `false` without publishing anything if the segment cannot be grown to hold
    /// `contents` - the previously published buffer stays current, and this payload is lost -
    /// or if the segment is no longer in service.
    pub fn write(&self, contents: &[u8]) -> bool {
        let mut state = self.state.lock_or_panic();
        let mapped = &mut state.mapped;

        let size = contents.len() + 1; // trailing zero byte, to keep some C code happy
        let needed = std::mem::size_of::<RawMetaData>() + size;
        if !mapped.ensure_space(needed) {
            tracing::warn!(
                "Dropping a {size} byte shared memory payload: {needed} bytes were needed and \
                 the segment could not be grown past {}",
                mapped.as_slice().len()
            );
            return false;
        }

        // Safety: the segment holds `needed` bytes, as just ensured
        // Actually &mut mapped.as_slice_mut() as RawData seems safe, but unsized locals are
        // unstable
        let data = unsafe { &mut *(mapped.as_slice_mut() as *mut [u8] as *mut RawData) };
        let current = data.meta.generation.load(Ordering::Relaxed);
        let start = if current == 0 {
            (now() / 1_000) << 1
        } else {
            current
        };
        // CAS preserves terminal markers even if retirement races this write.
        if current == RETIRED
            || current == ENDED
            || data
                .meta
                .generation
                .compare_exchange(
                    current,
                    start.wrapping_add(1),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_err()
        {
            tracing::debug!("Not publishing into a shared memory segment out of service");
            return false;
        }
        data.meta.size = size;

        data.as_slice_mut()[0..contents.len()].copy_from_slice(contents);
        data.as_slice_mut()[contents.len()] = 0;

        // Preserve retirement if it occurred during the copy.
        let published = data
            .meta
            .generation
            .compare_exchange(
                start.wrapping_add(1),
                start.wrapping_add(2),
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok();

        futex_wake(data.meta.generation.as_ptr().cast());

        if published {
            state.retire_replaced();
        }
        published
    }

    /// Borrow the buffer currently published in the segment (excluding the
    /// trailing NUL), or an empty slice if nothing has been written yet.
    ///
    /// This reads the writer's own mapping directly and performs no
    /// generation/consistency handshake — unlike [`OneWayShmReader::read`] — so
    /// only call it from the writing side where no concurrent `write` is in
    /// flight.
    pub fn as_slice(&self) -> &[u8] {
        let state = self.state.lock_or_panic();
        let data = unsafe { &*(state.mapped.as_slice() as *const [u8] as *const RawData) };
        skip_last_byte(data.as_slice())
    }

    /// The size in bytes of the writer's current mapping.
    ///
    /// This is the full mapped region (metadata header plus any slack left by
    /// growth), not the length of the published payload — use [`Self::as_slice`]
    /// for the latter.
    pub fn size(&self) -> usize {
        self.state.lock_or_panic().mapped.as_slice().len()
    }

    /// The current generation, or 0 before the first publication.
    ///
    /// After the initial time-based seed, each completed write advances it by two:
    /// odd while writing, even when stable. Its low 32 bits are the `futex` wait word
    /// used by [`OneWayShmReader::wait_for_change`].
    pub fn current_generation(&self) -> u64 {
        generation_of(&self.state.lock_or_panic().mapped).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::NamedShmHandle;

    /// Remove a name whose owner went away without doing so (a forgotten or crashed writer).
    #[cfg(unix)]
    fn unlink_test_name(path: &CStr) {
        _ = crate::platform::sys_shm_unlink(path);
    }

    fn test_path(name: &str) -> CString {
        #[allow(clippy::unwrap_used)]
        CString::new(format!("/ddtest-1way-{}-{name}", unsafe { libc::getpid() })).unwrap()
    }

    /// A payload the segment cannot hold must be refused outright: writing it would run off
    /// the end of the mapping, and bumping the generation first would leave readers waiting on
    /// a write that never lands. What was published before stays published.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn an_oversized_payload_is_refused_and_the_previous_one_stays() {
        let path = test_path("big");
        let writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(writer.write(b"published"));

        // Larger than any platform's reservation. The pages are never touched - `write` reads
        // only the length before refusing - so this costs no resident memory.
        let oversized = vec![0u8; 160 * 1024 * 1024];
        assert!(
            !writer.write(&oversized),
            "a payload beyond the reservation must be refused"
        );
        assert_eq!(
            writer.as_slice(),
            b"published",
            "the refused write must not have replaced the payload"
        );

        let mut reader = OneWayShmReader::new(open_named_shm(&path).unwrap(), ());
        let (changed, data) = reader.read();
        assert!(changed, "the generation must not have been left mid-write");
        assert_eq!(data, b"published");
    }

    /// The published size is the writer's, and the writer is another process. One that cannot
    /// be read - too large for the segment, or large enough to overflow the header arithmetic -
    /// must come back as a failed read rather than an index past the mapping.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_forged_payload_size_does_not_take_the_reader_past_the_mapping() {
        let path = test_path("forged");
        let writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(writer.write(b"published"));

        let mut reader = OneWayShmReader::new(open_named_shm(&path).unwrap(), ());
        assert_eq!(reader.read().1, b"published");

        for forged in [usize::MAX, 1 << 30] {
            // Stand in for a peer publishing an impossible size, with an even generation so
            // the reader takes it for a complete write.
            let mut mapped = open_named_shm(&path).unwrap();
            let data = unsafe { &mut *(mapped.as_slice_mut() as *mut [u8] as *mut RawData) };
            data.meta.size = forged;
            data.meta.generation.fetch_add(2, Ordering::Release);

            let (changed, data) = reader.read();
            assert!(!changed, "a size of {forged} cannot count as a new payload");
            assert_eq!(
                data, b"published",
                "the reader must keep the last payload it could read"
            );
        }
    }

    fn named_reader(path: &CString) -> OneWayShmReader<NamedShmHandle, CString> {
        OneWayShmReader::new_with_opener(open_named_shm(path).ok(), path.clone(), |path| {
            open_named_shm(path).ok()
        })
    }

    fn generation_of(path: &CStr) -> u64 {
        let mapped = open_named_shm(path).unwrap();
        meta(mapped.as_slice())
            .unwrap()
            .generation
            .load(Ordering::Acquire)
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn generation_is_seeded_once_then_incremented() {
        let path = test_path("clock");
        let writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        let mut reader = named_reader(&path);
        assert_eq!(writer.current_generation(), 0);
        assert_eq!(reader.read(), (false, &b""[..]));

        let before = now() / 1_000;
        assert!(writer.write(b"first"));
        let first = writer.current_generation();
        assert!(first >> 1 >= before);
        assert_eq!(first & 1, 0);
        assert_eq!(reader.read(), (true, &b"first"[..]));

        std::thread::sleep(Duration::from_millis(2));
        assert!(writer.write(b"second"));
        assert_eq!(writer.current_generation(), first + 2);
        assert_eq!(reader.read(), (true, &b"second"[..]));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn replacement_generation_ignores_the_predecessors_counter() {
        let path = test_path("ahead");
        let old = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old.write(b"old"));
        let old_segment = open_named_shm(&path).unwrap();
        let ahead = 1 << 62;
        meta(old_segment.as_slice())
            .unwrap()
            .generation
            .store(ahead, Ordering::Release);
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"old"[..]));
        assert_eq!(reader.last_read_generation(), ahead);

        let new = OneWayShmWriter::<NamedShmHandle>::new(path).unwrap();
        assert!(new.write(b"new"));
        assert!(new.current_generation() < ahead);
        assert!(new.current_generation() >> 1 <= now() / 1_000 + 1);
        assert_eq!(reader.read(), (true, &b"new"[..]));
    }

    /// Readers also accept replacements from writers using the old per-segment counter.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn an_existing_reader_follows_a_replacement_with_a_lower_generation() {
        let path = test_path("follow");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        for _ in 0..3 {
            assert!(old_writer.write(b"old"));
        }
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"old"[..]));
        assert_eq!(
            reader.last_read_generation(),
            old_writer.current_generation()
        );

        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        // Nothing published yet: the old segment is not retired, and the reader keeps it.
        assert_eq!(reader.read(), (false, &b"old"[..]));
        assert!(!reader.take_replaced());

        assert!(new_writer.write(b"new"));
        let new_segment = open_named_shm(&path).unwrap();
        meta(new_segment.as_slice())
            .unwrap()
            .generation
            .store(2, Ordering::Release);
        assert_eq!(reader.read(), (true, &b"new"[..]));
        assert_eq!(reader.last_read_generation(), 2);
        assert!(reader.take_replaced(), "the switch is reported once");
        assert!(!reader.take_replaced());
        assert_eq!(reader.read(), (false, &b"new"[..]));

        assert!(new_writer.write(b"newer"));
        assert_eq!(reader.read(), (true, &b"newer"[..]));
        assert!(
            !reader.take_replaced(),
            "later payloads are ordinary changes"
        );
    }

    /// A segment change counts as new data even if a writer reuses the old generation.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_replacement_with_an_equal_generation_is_taken() {
        let path = test_path("equal");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old_writer.write(b"old"));
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"old"[..]));

        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(new_writer.write(b"new"));
        let new_segment = open_named_shm(&path).unwrap();
        meta(new_segment.as_slice())
            .unwrap()
            .generation
            .store(reader.last_read_generation(), Ordering::Release);
        assert_eq!(reader.read(), (true, &b"new"[..]));
    }

    /// The old owner is only retired once its replacement holds a payload: readers moving over
    /// find something to use immediately, and never trade their last payload for nothing.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn retirement_waits_for_the_first_publication() {
        let path = test_path("notify-last");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old_writer.write(b"old"));
        let before = old_writer.current_generation();
        let old_segment = open_named_shm(&path).unwrap();

        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        let old_generation = || {
            meta(old_segment.as_slice())
                .unwrap()
                .generation
                .load(Ordering::Acquire)
        };
        assert_eq!(
            old_generation(),
            before,
            "not retired before the successor published"
        );

        assert!(new_writer.write(b"new"));
        assert_eq!(old_generation(), RETIRED);

        // A reader coming in during the gap: the fresh segment is unwritten, which must not
        // displace a payload it already had.
        let fresh = test_path("notify-gap");
        let first = OneWayShmWriter::<NamedShmHandle>::new(fresh.clone()).unwrap();
        assert!(first.write(b"kept"));
        let mut reader = named_reader(&fresh);
        assert_eq!(reader.read(), (true, &b"kept"[..]));
        let _unwritten = OneWayShmWriter::<NamedShmHandle>::new(fresh.clone()).unwrap();
        reader.clear_reader();
        assert_eq!(reader.read(), (false, &b"kept"[..]));
    }

    /// `clear_reader` exists for "the segment may have been replaced": what it reopens must be
    /// taken at face value, not compared against the old segment's counter.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn clear_reader_takes_the_reopened_segment_whatever_its_generation() {
        let path = test_path("clear");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        for _ in 0..3 {
            assert!(old_writer.write(b"old"));
        }
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"old"[..]));

        // Replaced by somebody who never retires the old one - a crash between unlink and
        // notification, say.
        let old_segment = open_named_shm(&path).unwrap();
        let new_segment = NamedShmHandle::create(path.clone(), 0x1000).unwrap();
        let new_writer = OneWayShmWriter::from_mapping(new_segment.map().unwrap());
        assert!(new_writer.write(b"new"));
        assert_eq!(reader.read(), (false, &b"old"[..]), "nothing tells it");

        reader.clear_reader();
        assert_eq!(reader.read(), (true, &b"new"[..]));
        assert_eq!(
            reader.last_read_generation(),
            new_writer.current_generation()
        );
        drop(old_segment);
    }

    /// A retired segment stays retired: its former writer must not bump the counter back into
    /// an even, "stable" value, which readers would take for a valid payload.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_retired_segment_is_not_written_back_into_service() {
        let path = test_path("no-resurrect");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old_writer.write(b"old"));
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"old"[..]));

        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(new_writer.write(b"new"));

        assert!(
            !old_writer.write(b"zombie"),
            "a retired segment refuses writes"
        );
        assert_eq!(old_writer.current_generation(), RETIRED);
        assert_eq!(reader.read(), (true, &b"new"[..]));
        assert_eq!(reader.read(), (false, &b"new"[..]));
    }

    /// A writer going away for good ends its segment, so its readers do not wait on it forever:
    /// they pick up whoever publishes under the name next, keeping the last payload meanwhile.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_dropped_writer_ends_its_segment() {
        let path = test_path("evicted");
        let writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        for _ in 0..3 {
            assert!(writer.write(b"first"));
        }
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"first"[..]));
        let segment = open_named_shm(&path).unwrap();

        drop(writer);
        assert_eq!(
            meta(segment.as_slice())
                .unwrap()
                .generation
                .load(Ordering::Acquire),
            ENDED
        );
        assert!(open_named_shm(&path).is_err(), "and its name is gone");
        assert_eq!(
            reader.read(),
            (true, &b"first"[..]),
            "the final payload is the last one"
        );
        assert_eq!(reader.last_read_generation(), 0, "with no writer behind it");
        assert_eq!(reader.read(), (false, &b"first"[..]));
        assert!(!reader.wait_for_change(Duration::from_millis(1)));

        let writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        // Not taken while unwritten: an empty segment does not displace the last payload.
        assert_eq!(reader.read(), (false, &b"first"[..]));
        assert!(writer.write(b"second"));
        assert_eq!(reader.read(), (true, &b"second"[..]));
        assert!(reader.take_replaced());
    }

    /// A writer that clears its data on the way out - remote config for a target no longer
    /// fetched, telemetry for a client going away - must have that final, empty payload seen,
    /// rather than readers keeping what was there before.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn the_final_payload_of_an_ended_segment_is_read() {
        let path = test_path("cleared");
        let writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(writer.write(b"configs"));
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"configs"[..]));

        assert!(writer.write(b""));
        drop(writer);
        assert!(reader.wait_for_change(Duration::from_millis(1)));
        assert_eq!(reader.read(), (true, &b""[..]));
        assert_eq!(reader.read(), (false, &b""[..]));
    }

    /// A segment that was replaced is not read from any more - its writer may have died halfway
    /// through a payload - not even when its successor has gone away too.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_retired_payload_is_never_read() {
        let path = test_path("torn");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old_writer.write(b"old"));
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"old"[..]));
        let old_segment = open_named_shm(&path).unwrap();

        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(new_writer.write(b"new"));
        drop(new_writer);
        let header = meta(old_segment.as_slice()).unwrap();
        assert_eq!(header.generation.load(Ordering::Acquire), RETIRED);
        assert!(!old_writer.write(b"torn"));

        // The successor's name went with it, so there is nothing to move to: the reader keeps
        // the last payload it could trust, and does not fall back to the retired one.
        assert_eq!(reader.read(), (false, &b"old"[..]));
        assert_eq!(reader.read(), (false, &b"old"[..]));

        let next = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(next.write(b"next"));
        assert_eq!(reader.read(), (true, &b"next"[..]));
    }

    /// Dropping an owner that was already replaced must neither remove the successor's name nor
    /// retire the successor.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_late_old_owner_leaves_its_successor_alone() {
        let path = test_path("late");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old_writer.write(b"old"));
        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(new_writer.write(b"new"));
        let generation = new_writer.current_generation();

        drop(old_writer);
        assert_eq!(
            generation_of(&path),
            generation,
            "the successor is reachable and in service"
        );
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"new"[..]));
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn dropping_an_inherited_writer_does_not_retire_its_predecessor() {
        let path = test_path("fork-drop");
        let old = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old.write(b"old"));
        let generation = old.current_generation();
        let replacement = OneWayShmWriter::<NamedShmHandle>::new(path).unwrap();

        // The child drops only its inherited writer, then exits without other destructors.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                drop(replacement);
                unsafe { libc::_exit(0) }
            }
            pid => {
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                assert!(libc::WIFEXITED(status));
                assert_eq!(libc::WEXITSTATUS(status), 0);
            }
        }

        assert_eq!(old.current_generation(), generation);
        assert_eq!(replacement.current_generation(), 0);
        assert!(replacement.write(b"new"));
        assert_eq!(old.current_generation(), RETIRED);
    }

    /// A writer that died mid-write leaves an odd counter behind forever. Its replacement must
    /// still be found by the readers stuck on it.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn readers_of_a_writer_killed_mid_write_follow_the_replacement() {
        let path = test_path("killed");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old_writer.write(b"old"));
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"old"[..]));

        let segment = open_named_shm(&path).unwrap();
        meta(segment.as_slice())
            .unwrap()
            .generation
            .fetch_add(1, Ordering::AcqRel);
        // The crashed writer never runs its destructor.
        std::mem::forget(old_writer);
        assert_eq!(reader.read(), (false, &b"old"[..]));

        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(new_writer.write(b"new"));
        assert_eq!(
            meta(segment.as_slice())
                .unwrap()
                .generation
                .load(Ordering::Acquire),
            RETIRED
        );
        assert_eq!(reader.read(), (true, &b"new"[..]));
    }

    /// A waiter must not sleep out its timeout on a segment that was retired: retirement wakes
    /// it, and the next wait finds the replacement.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn wait_for_change_follows_retirement() {
        let path = test_path("wait");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old_writer.write(b"old"));
        let mut reader = named_reader(&path);
        assert!(reader.wait_for_change(Duration::from_millis(1)));
        assert_eq!(reader.read(), (true, &b"old"[..]));
        assert!(!reader.wait_for_change(Duration::from_millis(1)));

        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(new_writer.write(b"new"));

        let start = std::time::Instant::now();
        assert!(reader.wait_for_change(Duration::from_secs(10)));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "retirement must not be waited out"
        );
        assert_eq!(reader.read(), (true, &b"new"[..]));
    }

    /// With the futex feature, a reader blocked on the old segment is woken by the retirement
    /// itself rather than by its timeout.
    #[test]
    #[cfg(all(
        feature = "one_way_shm_futex",
        target_os = "linux",
        target_endian = "little"
    ))]
    #[cfg_attr(miri, ignore)]
    fn retirement_wakes_a_blocked_waiter() {
        let path = test_path("futex");
        let old_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old_writer.write(b"old"));
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (true, &b"old"[..]));
        assert!(reader.wait_for_change(Duration::from_millis(1)));

        let waiter = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let changed = reader.wait_for_change(Duration::from_secs(30));
            (changed, start.elapsed())
        });
        std::thread::sleep(Duration::from_millis(100));
        let new_writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(new_writer.write(b"new"));
        let (changed, elapsed) = waiter.join().unwrap();
        assert!(changed);
        assert!(
            elapsed < Duration::from_secs(10),
            "woken by the retirement, not the timeout"
        );
    }

    /// A segment nothing was published into yet has nothing to report: taking it for an empty
    /// payload would make consumers clear what they have.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn an_unwritten_segment_is_not_a_change() {
        let path = test_path("unwritten");
        let writer = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        let mut reader = named_reader(&path);
        assert_eq!(reader.read(), (false, &b""[..]));
        assert!(writer.write(b""));
        assert_eq!(
            reader.read(),
            (true, &b""[..]),
            "an empty publication is one"
        );
    }

    /// The owner that was replaced from another process cannot learn so from its own process's
    /// bookkeeping - but it must still leave the successor's name alone.
    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn an_owner_replaced_from_another_process_leaves_the_name() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let path = test_path("cross-process");
        let old = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
        assert!(old.write(b"old"));
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        // SAFETY: the child only uses this library and its own socket, then leaves via _exit.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                let replacement = OneWayShmWriter::<NamedShmHandle>::new(path.clone()).unwrap();
                let ok = replacement.write(b"new")
                    && child.write_all(b"R").is_ok()
                    && child.read_exact(&mut [0]).is_ok()
                    && NamedShmHandle::open(&path).is_ok();
                std::mem::forget(replacement);
                unsafe { libc::_exit(if ok { 0 } else { 1 }) }
            }
            pid => {
                parent.read_exact(&mut [0]).unwrap();
                drop(old);
                parent.write_all(b"D").unwrap();
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                assert_eq!(
                    libc::WEXITSTATUS(status),
                    0,
                    "the successor's name was removed"
                );
                let mut reader = named_reader(&path);
                assert_eq!(reader.read(), (true, &b"new"[..]));
                unlink_test_name(&path);
            }
        }
    }
}
