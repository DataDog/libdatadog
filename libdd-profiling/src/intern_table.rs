// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Append-only, deduplicating tables that hand out dense `u32` ids. The
//! profiles dictionary's strings and functions are stored in these.
//!
//! An [`InternTable<R>`] maps each distinct value of a [`Record`] type to an
//! [`Id<R>`], and resolves ids back to values. Equal values always get the
//! same id, and id 0 is always the record's zero value (`""` for strings),
//! matching OTLP's index-0 convention. Nothing is ever removed.
//!
//! The record type chooses how its values are stored, through its
//! [`Storage`]:
//!
//! | Storage | Used by | Hash bucket | Id-map slot | Extra memory |
//! |---|---|---|---|---|
//! | [`Inline<T>`] | [`Function`] | id | the record itself | none |
//! | [`Arena`] | [`Str`] | arena offset / 2, id | arena offset / 2 | byte arena |
//!
//! Everything else (probing, growth, locking, publication and recovery) is
//! written once, in this module.
//!
//! # Memory
//!
//! Each table owns one virtual-memory reservation. It is not an
//! [`Allocator`](crate::collections::alloc::Allocator) user: it relies on
//! reserving address space without backing it, on committing parts of it
//! later, and (on Unix) on the mapping being shared with forked children.
//!
//! An [`InternTable`] is a handle to a small private page holding the
//! process-local state: a count of this process's handles, the thread lock,
//! the process lock's descriptor and the commit marks. The last handle
//! dropped in a process unmaps that process's view and page; other
//! processes are unaffected.
//!
//! ```text
//! [ header page ][ region 0 ][ region 1 ] … [ region R-1 ][ id map ][ arena ]
//! ```
//!
//! - The **header** holds the published id count, the number of sealed regions, the dirty marker
//!   and the arena cursor.
//! - **Region** `k` is a [`HashTable`] filling `F(k)` blocks of [`BLOCK_BYTES`], where `F` = 1, 1,
//!   2, 3, 5, …. Regions fill in order; the insert that fills one seals it, and it is never written
//!   again. So region `k` holds a contiguous run of ids.
//! - The **id map** has one slot per possible id, indexed by id.
//! - The **arena** exists only for [`Arena`] storage.
//!
//! A fresh reservation is all zeros, which is an empty hash table, so no
//! region is ever zeroed except by recovery. Memory is made usable at three
//! points, each before anything is written:
//!
//! - **Regions:** region 0 when the table is created, and region `k + 1` by the insert that fills
//!   region `k`. The whole region is committed and populated (faulted in) at once.
//! - **Id map and arena:** committed only when an insert crosses into a new page, so most inserts
//!   make no system call.
//!
//! Committing is real only on Windows. Unix pages fault in on first touch,
//! so exhausted memory shows up as the OS's overcommit behavior, not an
//! error. Populating does report it, where the OS supports
//! `MADV_POPULATE_WRITE` (Linux 5.14).
//!
//! # Concurrency
//!
//! Every method takes `&self`, and the table is `Sync`.
//!
//! - [`InternTable::resolve`] is lock-free. It acquire-loads the published count and reads the
//!   id-map slot (and arena record) of a smaller id. That memory is written before the count is
//!   released, and never again.
//! - [`InternTable::intern`] first searches the sealed regions without a lock. Sealed regions never
//!   change. It then takes the writer locks and searches or inserts in the current region.
//!
//!   An insert writes the arena record and the id-map slot, then
//!   release-stores the count; that store is the commit point. Only then are
//!   the hash bucket and the arena cursor written, and the region sealed if
//!   it is now full.
//! - Writers take two locks, both without waiting: the table's **thread lock** (a bounded spin),
//!   then on Unix the **process lock**, an `fcntl` record lock on a private unlinked file in the
//!   caller's directory. Either being held gives [`InternError::Busy`].
//!
//! # Processes and recovery
//!
//! On Unix a table is always shared between processes: children forked
//! after it is created see every later insert, and ids agree across the
//! whole process tree. Windows tables serve the threads of one process.
//!
//! A writer marks the header dirty before changing anything and clears the
//! mark when done. If a process dies mid-insert, the kernel drops its
//! process lock, and the next writer finds the mark set. It rebuilds the
//! current region's hash table from the published ids, restores the arena
//! cursor from the last published record, and seals the region if it is full.
//! Published ids are never rolled back, and readers keep working throughout.
//!
//! The library registers no fork handlers. Hold [`InternTable::prepare_fork`]
//! across `fork` and finish it in both processes.

use crate::collections::hash_table::{self, Entry, HashTable, HashTableRef};
use core::ffi::CStr;
use core::fmt;
use core::hash::{BuildHasher, Hash, Hasher};
use core::marker::PhantomData;
use core::ptr::{self, NonNull};
use core::sync::atomic::{self as atomic, AtomicBool, AtomicU32, AtomicUsize, Ordering};
use foldhash::fast::FixedState;
use process_lock::{LockError, ProcessLock};
use spin_lock::{SpinLock, SpinLockGuard};
use storage::{ArenaMem, Mem};

mod os;
mod process_lock;
mod records;
mod spin_lock;
mod storage;
#[cfg(test)]
mod tests;

#[cfg(feature = "otel")]
pub use records::semconv;
pub use records::{Function, Str};
pub use storage::{Arena, Inline, Storage};

/// An id into a [`StringTable`].
pub type StringId = Id<Str>;
/// An id into a [`FunctionTable`].
pub type FunctionId = Id<Function>;
/// Interned UTF-8 strings, stored in an arena.
pub type StringTable = InternTable<Str>;

impl InternTable<Str> {
    /// A new string table that also holds `reserved`, such as placeholders a
    /// runtime uses when interning fails later: `ids[i]` gets `reserved[i]`'s
    /// id. They are interned while the table is private, so this cannot be
    /// busy.
    ///
    /// # Errors
    ///
    /// [`InitError::InvalidConfig`] if `reserved` and `ids` differ in length or
    /// the table cannot hold `reserved`; otherwise as for [`Self::new`]. On
    /// error `ids` is all zeros.
    pub fn with_reserved<S: AsRef<str>>(
        config: Config,
        lock_dir: Option<&CStr>,
        reserved: &[S],
        ids: &mut [StringId],
    ) -> Result<Self, InitError> {
        Self::with_reserved_by(config, lock_dir, reserved, ids, |s| s.as_ref())
    }
}
/// Interned functions, stored inline in the id map.
pub type FunctionTable = InternTable<Function>;

impl InternTable<Function> {
    /// A new function table that also holds `reserved`: `ids[i]` gets
    /// `reserved[i]`'s id. See [`StringTable::with_reserved`].
    ///
    /// # Errors
    ///
    /// As for [`StringTable::with_reserved`].
    pub fn with_reserved(
        config: Config,
        lock_dir: Option<&CStr>,
        reserved: &[Function],
        ids: &mut [FunctionId],
    ) -> Result<Self, InitError> {
        Self::with_reserved_by(config, lock_dir, reserved, ids, |&f| f)
    }
}

/// The most regions a table can have.
pub const MAX_REGIONS: usize = 32;

/// The unit of region sizes. OS pages must divide it.
pub const BLOCK_BYTES: usize = 16 * 1024;

/// Largest arena: offsets are stored halved in a `u32`.
pub const MAX_ARENA_BYTES: usize = 1 << 33;

/// A type of value that an [`InternTable`] stores.
pub trait Record {
    /// The value as callers pass and receive it.
    type Key<'a>: Copy + Hash;
    /// How values are laid out in the table's memory.
    type Storage: Storage;
    /// The value with id 0.
    const ZERO: Self::Key<'static>;
    /// Value `i` of those a new table interns right after [`Self::ZERO`],
    /// so it has id `i + 1` in every table; `None` past the last.
    #[must_use]
    fn reserved(index: usize) -> Option<Self::Key<'static>> {
        let _ = index;
        None
    }

    /// Converts a value to its stored form. Equal stored forms must mean
    /// equal values, and equal values must hash equally.
    fn encode(key: Self::Key<'_>) -> <Self::Storage as Storage>::Item<'_>;

    /// Converts a stored form back to a value.
    ///
    /// # Safety
    ///
    /// `item` must have been produced by [`Self::encode`].
    unsafe fn decode(item: <Self::Storage as Storage>::Item<'_>) -> Self::Key<'_>;
}

/// A dense id into an [`InternTable<R>`]. The type parameter keeps ids of
/// different tables apart; at runtime it is a `u32`.
#[repr(transparent)]
pub struct Id<R>(u32, PhantomData<fn() -> R>);

impl<R> Id<R> {
    /// The id of the zero value, present in every table.
    pub const ZERO: Self = Self::new(0);

    const fn new(index: u32) -> Self {
        Self(index, PhantomData)
    }

    /// An id from its dense index, such as one passed through FFI. Any
    /// index is safe: resolving one the table never handed out finds
    /// nothing.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self::new(index)
    }

    /// The id as a dense index, starting at 0.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl<R> Clone for Id<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for Id<R> {}

impl<R> PartialEq for Id<R> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<R> Eq for Id<R> {}

impl<R> PartialOrd for Id<R> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<R> Ord for Id<R> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl<R> Hash for Id<R> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<R> Default for Id<R> {
    fn default() -> Self {
        Self::ZERO
    }
}

impl<R> fmt::Debug for Id<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Id").field(&self.0).finish()
    }
}

/// The size of a table's reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Config {
    /// Number of hash regions, 1 to [`MAX_REGIONS`]. This fixes the maximum
    /// number of ids, which must fit in a `u32`.
    pub regions: u8,
    /// Arena bytes, for [`Arena`] storage only: from 2 to [`MAX_ARENA_BYTES`].
    /// Must be 0 for [`Inline`] storage.
    pub arena_bytes: usize,
}

/// Why a table cannot be created.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitError {
    /// The [`Config`] is out of range for this record type, the lock
    /// directory is missing (Unix) or present (Windows), or the lock path is
    /// too long.
    InvalidConfig,
    /// The OS page size does not divide [`BLOCK_BYTES`].
    UnsupportedPageSize,
    /// The OS refused the reservation.
    MappingFailed,
    /// The OS could not back the header or the first region.
    OutOfMemory,
    /// The lock file could not be created.
    LockFailed,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfig => "intern table configuration is invalid",
            Self::UnsupportedPageSize => "unsupported OS page size",
            Self::MappingFailed => "could not reserve memory for the intern table",
            Self::OutOfMemory => "could not commit memory for the intern table",
            Self::LockFailed => "could not create the intern table's lock file",
        })
    }
}

impl core::error::Error for InitError {}

/// Why a value could not be interned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InternError {
    /// Another thread or process held a writer lock. The table is unchanged.
    Busy,
    /// Every region is full. The table is unchanged.
    Full,
    /// The arena has no room for the value. The table is unchanged.
    ArenaFull,
    /// The value is longer than one arena record can hold (65,535 bytes). The
    /// table is unchanged.
    TooLong,
    /// The OS could not commit memory. The table is unchanged.
    OutOfMemory,
    /// The process lock could not be taken or released. After a failed
    /// release the value may have been added, and this process refuses
    /// further writes, because its lock state is unknown.
    LockFailed,
    /// The shared memory breaks an invariant; the table is not changed
    /// further.
    Corrupt,
}

impl fmt::Display for InternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Busy => "intern table is busy",
            Self::Full => "intern table is full",
            Self::ArenaFull => "intern table arena is full",
            Self::TooLong => "value is too long for an intern table record",
            Self::OutOfMemory => "could not commit memory for the intern table",
            Self::LockFailed => "intern table process lock failed",
            Self::Corrupt => "intern table memory is inconsistent",
        })
    }
}

impl core::error::Error for InternError {}

/// A value on its own 128-byte line, so readers polling one field do not
/// share a cache line with writers updating another.
#[repr(C, align(128))]
struct Line<T>(T);

/// What readers load: the published id count and the sealed region count.
#[repr(C, align(128))]
struct Publication {
    count: AtomicU32,
    sealed: AtomicU32,
}

/// The shared state at the start of the reservation.
#[repr(C)]
struct Header {
    publication: Publication,
    /// 1 from the start of a write until it completes; still 1 afterwards
    /// means a writer died mid-operation.
    dirty: Line<AtomicU32>,
    /// Arena bytes in use. Only writers use it.
    arena_used: Line<AtomicUsize>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Region {
    /// Byte offset from the reservation's start; a multiple of the page size.
    offset: usize,
    /// Byte length; a multiple of [`BLOCK_BYTES`].
    len: usize,
    groups: usize,
    /// Ids before this region; it holds ids from here on.
    first_id: u32,
    /// Ids it holds when full.
    capacity: u32,
}

impl Region {
    const fn end_id(&self) -> u32 {
        self.first_id.saturating_add(self.capacity)
    }
}

/// Where each part of a table lives in its reservation.
struct Plan {
    regions: [Region; MAX_REGIONS],
    region_count: usize,
    ids_offset: usize,
    max_ids: u32,
    arena_offset: usize,
    arena_len: usize,
    total: usize,
}

impl Plan {
    fn new<S: Storage>(config: Config, page: usize) -> Result<Self, InitError> {
        if !page.is_power_of_two()
            || page < size_of::<Header>()
            || !BLOCK_BYTES.is_multiple_of(page)
        {
            return Err(InitError::UnsupportedPageSize);
        }
        Self::regions::<S>(config, page).ok_or(InitError::InvalidConfig)
    }

    fn regions<S: Storage>(config: Config, page: usize) -> Option<Self> {
        let region_count = usize::from(config.regions);
        if region_count == 0 || region_count > MAX_REGIONS {
            return None;
        }
        let arena_ok = if S::ARENA {
            // Id 0's record needs 2 bytes.
            (2..=MAX_ARENA_BYTES).contains(&config.arena_bytes)
        } else {
            config.arena_bytes == 0
        };
        if !arena_ok {
            return None;
        }

        let mut regions = [Region::default(); MAX_REGIONS];
        // The header has the first page to itself.
        let mut offset = page;
        let mut first_id = 0_u32;
        let (mut blocks, mut next_blocks) = (1_usize, 1_usize);
        for region in regions.iter_mut().take(region_count) {
            let len = blocks.checked_mul(BLOCK_BYTES)?;
            let groups = hash_table::groups_for_bytes::<S::Bucket>(len);
            let capacity = u32::try_from(hash_table::capacity_for_groups(groups)).ok()?;
            *region = Region {
                offset,
                len,
                groups,
                first_id,
                capacity,
            };
            offset = offset.checked_add(len)?;
            first_id = first_id.checked_add(capacity)?;
            (blocks, next_blocks) = (next_blocks, blocks.saturating_add(next_blocks));
        }

        let ids_offset = offset;
        let ids_len = usize_from(first_id).checked_mul(size_of::<S::Slot>())?;
        let arena_offset = ids_offset
            .checked_add(ids_len)?
            .checked_next_multiple_of(page)?;
        let total = arena_offset
            .checked_add(config.arena_bytes)?
            .checked_next_multiple_of(page)?;
        if isize::try_from(total).is_err() {
            return None;
        }
        Some(Self {
            regions,
            region_count,
            ids_offset,
            max_ids: first_id,
            arena_offset,
            arena_len: config.arena_bytes,
            total,
        })
    }
}

/// A table's process-local state, on its own private page: the handle count,
/// the thread lock, the process lock's descriptor and the commit marks. A
/// forked child gets a copy-on-write copy, over the same shared reservation.
struct Local<R: Record> {
    /// Thread lock; first, for its alignment.
    thread_lock: SpinLock,
    /// Live handles in this process; the last one dropped unmaps.
    handles: AtomicUsize,
    /// Size of this page's mapping.
    page_len: usize,
    base: NonNull<u8>,
    total: usize,
    page: usize,
    regions: [Region; MAX_REGIONS],
    region_count: usize,
    ids_offset: usize,
    arena_offset: usize,
    arena_len: usize,
    mem: Mem<R::Storage>,
    /// Set when releasing the process lock failed: this process no longer
    /// knows whether it holds it, so it refuses to write.
    broken: AtomicBool,
    process_lock: ProcessLock,
    /// Reservation offsets up to which the id map and the arena are
    /// committed. Only writers use them. They are per process, which is
    /// enough: commits do real work only on Windows, which shares tables
    /// with no other process.
    ids_committed: AtomicUsize,
    arena_committed: AtomicUsize,
    _record: PhantomData<fn() -> R>,
}

// SAFETY: the reservation holds only plain data, read and written through
// atomics or under the writer locks, following the protocol in the module
// documentation. The table owns the reservation, which has no thread
// affinity.
unsafe impl<R: Record> Send for Local<R> {}
// SAFETY: as above.
unsafe impl<R: Record> Sync for Local<R> {}

impl<R: Record> Local<R> {
    /// Reserves the table's memory and writes its header. The result is not
    /// usable until it is on its page and [`Self::initialize`] has run.
    fn create(config: Config, lock_dir: Option<&CStr>, page: usize) -> Result<Self, InitError> {
        let plan = Plan::new::<R::Storage>(config, page)?;
        let process_lock = ProcessLock::new(lock_dir)?;
        let base = os::reserve(plan.total).ok_or(InitError::MappingFailed)?;
        let Plan {
            regions,
            region_count,
            ids_offset,
            max_ids,
            arena_offset,
            arena_len,
            total,
        } = plan;
        // SAFETY: the first page is the header's, and the reservation is new.
        if !unsafe { os::commit(base, page) } {
            // SAFETY: nothing uses the new reservation.
            unsafe { os::release(base, total) };
            return Err(InitError::OutOfMemory);
        }
        // SAFETY: the header page is committed, aligned and unused, and the
        // other offsets lie within the reservation (`Plan`).
        let (ids, arena) = unsafe {
            base.cast::<Header>().write(Header {
                publication: Publication {
                    count: AtomicU32::new(0),
                    sealed: AtomicU32::new(0),
                },
                dirty: Line(AtomicU32::new(0)),
                arena_used: Line(AtomicUsize::new(0)),
            });
            (
                base.add(ids_offset).cast(),
                ArenaMem {
                    base: base.add(arena_offset),
                    len: arena_len,
                },
            )
        };
        Ok(Self {
            thread_lock: SpinLock::new(),
            handles: AtomicUsize::new(1),
            page_len: size_of::<Self>().next_multiple_of(page),
            base,
            total,
            page,
            regions,
            region_count,
            ids_offset,
            arena_offset,
            arena_len,
            mem: Mem {
                ids,
                max_ids,
                extra: R::Storage::extra(arena),
            },
            broken: AtomicBool::new(false),
            process_lock,
            ids_committed: AtomicUsize::new(ids_offset),
            arena_committed: AtomicUsize::new(arena_offset),
            _record: PhantomData,
        })
    }

    /// Makes region 0 usable and adds the zero value, then the reserved
    /// values.
    fn initialize(&self) -> Result<(), InitError> {
        self.populate(0).map_err(|_| InitError::OutOfMemory)?;
        let reserved = (0..).map_while(R::reserved);
        for (index, value) in (0_u32..).zip(core::iter::once(R::ZERO).chain(reserved)) {
            match self.intern(value) {
                Ok(id) if id.index() == index => {}
                Err(InternError::OutOfMemory) => return Err(InitError::OutOfMemory),
                Err(InternError::LockFailed | InternError::Busy) => {
                    return Err(InitError::LockFailed);
                }
                _ => return Err(InitError::InvalidConfig),
            }
        }
        Ok(())
    }

    /// Unmaps the reservation and this page, after closing the lock file.
    ///
    /// # Safety
    ///
    /// `local` is a live page, and nothing in this process uses it again.
    unsafe fn unmap(local: NonNull<Self>) {
        // SAFETY: the caller hands over the page; it holds a valid `Local`.
        unsafe {
            let (base, total, len) = {
                let this = local.as_ref();
                (this.base, this.total, this.page_len)
            };
            ptr::drop_in_place(local.as_ptr());
            os::release(base, total);
            os::release(local.cast(), len);
        }
    }

    /// See [`InternTable::intern`].
    fn intern(&self, key: R::Key<'_>) -> Result<Id<R>, InternError> {
        let hash = FixedState::default().hash_one(key);
        let item = R::encode(key);
        let observed = self.header().publication.sealed.load(Ordering::Acquire);
        if let Some(id) = self.search_sealed(0, observed, hash, item) {
            return Ok(Id::new(id));
        }
        if usize_from(observed) >= self.region_count {
            return Err(InternError::Full);
        }
        let mut writer = self.writer()?;
        let result = self.insert(&mut writer, observed, hash, item);
        writer.finish().and(result)
    }

    /// See [`InternTable::resolve`].
    fn resolve(&self, id: Id<R>) -> Option<R::Key<'_>> {
        if id.0 >= self.header().publication.count.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: the slot of a published id was written before the count
        // was released and is never written again. So is its arena record.
        // The table wrote the item from `R::encode`.
        unsafe {
            let slot = self.mem.read_slot(id.0)?;
            Some(R::decode(R::Storage::load(&self.mem, slot)))
        }
    }

    fn len(&self) -> usize {
        usize_from(self.header().publication.count.load(Ordering::Acquire))
    }

    const fn header(&self) -> &Header {
        // SAFETY: `new` wrote the header at `base`, in committed memory. It
        // holds only atomics, so shared references are fine across threads.
        unsafe { self.base.cast::<Header>().as_ref() }
    }

    /// Takes both writer locks, and recovers if the last writer died.
    fn writer(&self) -> Result<Writer<'_, R>, InternError> {
        let thread_lock = self.thread_lock.try_lock().ok_or(InternError::Busy)?;
        if self.broken.load(Ordering::Acquire) {
            return Err(InternError::LockFailed);
        }
        self.process_lock
            .try_acquire()
            .map_err(|error| match error {
                LockError::Busy => InternError::Busy,
                LockError::Failed => InternError::LockFailed,
            })?;
        let mut writer = Writer {
            table: self,
            thread_lock: Some(thread_lock),
            clean: false,
        };
        // Acquire what the last writer published, and order every write
        // below after the mark.
        match self.header().dirty.0.swap(1, Ordering::AcqRel) {
            0 => {}
            // A failed recovery leaves the mark set for the next writer.
            1 => self.recover()?,
            _ => return Err(InternError::Corrupt),
        }
        writer.clean = true;
        Ok(writer)
    }

    /// Searches regions `from..to` (sealed region indices) for `item`.
    fn search_sealed(
        &self,
        from: u32,
        to: u32,
        hash: u64,
        item: <R::Storage as Storage>::Item<'_>,
    ) -> Option<u32> {
        let regions = self
            .regions
            .get(usize_from(from)..usize_from(to).min(self.region_count))?;
        regions.iter().find_map(|region| {
            // SAFETY: the region is sealed, so its bytes no longer change.
            let bytes = unsafe { self.bytes(region) };
            let view = HashTableRef::new(bytes, region.groups).ok()?;
            // SAFETY: sealed buckets name published records.
            let bucket = view.find(hash, |b| unsafe { R::Storage::matches(&self.mem, b, item) })?;
            Some(R::Storage::id(bucket))
        })
    }

    /// Finds or inserts `item` with the writer locks held.
    fn insert(
        &self,
        writer: &mut Writer<'_, R>,
        observed: u32,
        hash: u64,
        item: <R::Storage as Storage>::Item<'_>,
    ) -> Result<Id<R>, InternError> {
        let header = self.header();
        let publication = &header.publication;
        let sealed = publication.sealed.load(Ordering::Acquire);
        // Regions sealed while this writer waited may hold the key.
        if let Some(id) = self.search_sealed(observed, sealed, hash, item) {
            return Ok(Id::new(id));
        }
        let index = usize_from(sealed);
        let region = self.region(index).ok_or(InternError::Full)?;
        let count = publication.count.load(Ordering::Relaxed);
        if !(region.first_id..region.end_id()).contains(&count) {
            return Err(InternError::Corrupt);
        }
        // SAFETY: the locks are held and the region is current, so no one
        // else reads or writes its bytes.
        let bytes = unsafe { self.region_mut(index) };
        let len = usize_from(count - region.first_id);
        let mut index_table =
            HashTable::reopen(bytes, region.groups, len).map_err(|_| InternError::Corrupt)?;
        // SAFETY: the current region's buckets name published records.
        let vacant =
            match index_table.entry(hash, |b| unsafe { R::Storage::matches(&self.mem, b, item) }) {
                Entry::Occupied(bucket) => return Ok(Id::new(R::Storage::id(bucket))),
                Entry::Vacant(vacant) => vacant,
                // The insert that fills a region seals it.
                Entry::Full => return Err(InternError::Corrupt),
            };

        // Check and commit everything first, so a failure changes nothing.
        let record_size = R::Storage::record_size(item)?;
        let arena_start = header.arena_used.0.load(Ordering::Relaxed);
        let arena_end = arena_start
            .checked_add(record_size)
            .filter(|&end| end <= self.arena_len)
            .ok_or(InternError::ArenaFull)?;
        let fills_region = count + 1 == region.end_id();
        if fills_region && index + 1 < self.region_count {
            self.populate(index + 1)?;
        }
        let slot_end = usize_from(count + 1)
            .checked_mul(size_of::<<R::Storage as Storage>::Slot>())
            .and_then(|len| self.ids_offset.checked_add(len))
            .ok_or(InternError::Corrupt)?;
        self.commit_to(&self.ids_committed, slot_end)?;
        if record_size > 0 {
            self.commit_to(&self.arena_committed, self.arena_offset + arena_end)?;
        }

        writer.clean = false;
        // SAFETY: the locks are held, and the record's arena bytes are
        // committed and past the cursor, so no one reads them.
        let slot = unsafe { R::Storage::store(&self.mem, item, arena_start)? };
        // SAFETY: the slot's page is committed, and `count` is not yet
        // published, so no one reads it.
        unsafe { self.mem.write_slot(count, slot) };
        // The commit point: from here on the id is visible to readers.
        publication.count.store(count + 1, Ordering::Release);
        vacant.insert(R::Storage::bucket(slot, count));
        header.arena_used.0.store(arena_end, Ordering::Relaxed);
        if fills_region {
            // Readers search the region without locks once this is visible.
            publication.sealed.store(sealed + 1, Ordering::Release);
        }
        writer.clean = true;
        Ok(Id::new(count))
    }

    /// Rebuilds after a writer died: the current region's hash table from
    /// the published ids, the arena cursor from the last published record,
    /// and the seal of a full region.
    fn recover(&self) -> Result<(), InternError> {
        let header = self.header();
        let publication = &header.publication;
        // Read-modify-writes observe the latest values, so nothing a reader
        // may have seen is rolled back.
        let sealed = publication.sealed.fetch_add(0, Ordering::Acquire);
        let count = publication.count.fetch_add(0, Ordering::Acquire);
        let index = usize_from(sealed);
        let current = if let Some(region) = self.region(index) {
            if !(region.first_id..=region.end_id()).contains(&count) {
                return Err(InternError::Corrupt);
            }
            Some(region)
        } else if index == self.region_count && count == self.mem.max_ids {
            None
        } else {
            return Err(InternError::Corrupt);
        };

        if let Some(region) = current {
            // SAFETY: the locks are held and the region is not sealed, so no
            // one else reads or writes its bytes.
            let bytes = unsafe { self.region_mut(index) };
            let mut table =
                HashTable::<<R::Storage as Storage>::Bucket>::reopen(bytes, region.groups, 0)
                    .map_err(|_| InternError::Corrupt)?;
            table.clear();
            for id in region.first_id..count {
                // SAFETY: `id` is published, so its slot and record are
                // written and never change; the table wrote the item.
                let (slot, key) = unsafe {
                    let slot = self.mem.read_slot(id).ok_or(InternError::Corrupt)?;
                    (slot, R::decode(R::Storage::load(&self.mem, slot)))
                };
                let hash = FixedState::default().hash_one(key);
                table
                    .insert_unique(hash, R::Storage::bucket(slot, id))
                    .map_err(|_| InternError::Corrupt)?;
            }
        }

        let arena_used = match count.checked_sub(1) {
            None => 0,
            // SAFETY: the last id is published.
            Some(last) => unsafe {
                let slot = self.mem.read_slot(last).ok_or(InternError::Corrupt)?;
                R::Storage::record_end(&self.mem, slot).ok_or(InternError::Corrupt)?
            },
        };
        header.arena_used.0.store(arena_used, Ordering::Relaxed);

        if let Some(region) = current
            && count == region.end_id()
        {
            if index + 1 < self.region_count {
                self.populate(index + 1)?;
            }
            publication.sealed.store(sealed + 1, Ordering::Release);
        }
        Ok(())
    }

    fn region(&self, index: usize) -> Option<Region> {
        self.regions
            .get(index)
            .copied()
            .filter(|_| index < self.region_count)
    }

    /// Commits and faults in a whole region, which must never have been
    /// current.
    fn populate(&self, index: usize) -> Result<(), InternError> {
        let region = self.region(index).ok_or(InternError::Corrupt)?;
        // SAFETY: the region lies within the reservation, starts on a page
        // and has never been used, so its bytes are zero.
        let start = unsafe { self.base.add(region.offset) };
        // SAFETY: as above.
        if unsafe { os::populate(start, region.len, self.page) } {
            Ok(())
        } else {
            Err(InternError::OutOfMemory)
        }
    }

    /// Commits up to reservation offset `end`, a page at a time, if needed.
    fn commit_to(&self, mark: &AtomicUsize, end: usize) -> Result<(), InternError> {
        let committed = mark.load(Ordering::Relaxed);
        if end <= committed {
            return Ok(());
        }
        let new_mark = end
            .checked_next_multiple_of(self.page)
            .map_or(self.total, |mark| mark.min(self.total));
        // SAFETY: `committed..new_mark` lies within the reservation.
        let ok = unsafe { os::commit(self.base.add(committed), new_mark - committed) };
        if !ok {
            return Err(InternError::OutOfMemory);
        }
        mark.store(new_mark, Ordering::Relaxed);
        Ok(())
    }

    /// The bytes of a region.
    ///
    /// # Safety
    ///
    /// No one writes the region while the slice lives.
    const unsafe fn bytes(&self, region: &Region) -> &[u8] {
        // SAFETY: the region lies within the reservation (`Plan`); the caller
        // rules out writers.
        unsafe { core::slice::from_raw_parts(self.base.add(region.offset).as_ptr(), region.len) }
    }

    /// The bytes of region `index`, for writing.
    ///
    /// # Safety
    ///
    /// `index` is below `region_count`, and no one else reads or writes the
    /// region while the slice lives.
    #[expect(clippy::mut_from_ref, reason = "exclusivity comes from the locks")]
    unsafe fn region_mut(&self, index: usize) -> &mut [u8] {
        let region = self.region(index).unwrap_or_default();
        // SAFETY: the region lies within the reservation (`Plan`); the caller
        // guarantees exclusive access.
        unsafe {
            core::slice::from_raw_parts_mut(self.base.add(region.offset).as_ptr(), region.len)
        }
    }
}

/// An append-only table that interns values of `R` and hands out dense ids.
/// See the [module documentation](self).
///
/// This is a handle, counted per process: [`Self::try_clone`] adds one, and
/// dropping a handle removes it. The last handle in a process unmaps that
/// process's view; other processes keep theirs.
///
/// Dropping unmaps memory, so this type must not live in a `static` or a
/// thread-local of this crate's own. C callers hold the raw pointer from
/// [`Self::into_raw`] instead, which has no destructor.
#[repr(transparent)]
pub struct InternTable<R: Record>(NonNull<Local<R>>);

// SAFETY: the handle points to `Local`, which is `Send` and `Sync`, and the
// handle count is atomic.
unsafe impl<R: Record> Send for InternTable<R> {}
// SAFETY: as above.
unsafe impl<R: Record> Sync for InternTable<R> {}

/// [`InternTable::try_clone`] failed: the process has `isize::MAX` handles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TooManyHandles;

impl fmt::Display for TooManyHandles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("too many intern table handles")
    }
}

impl core::error::Error for TooManyHandles {}

/// The most handles a process can hold to one table.
const MAX_HANDLES: usize = isize::MAX.cast_unsigned();

impl<R: Record> InternTable<R> {
    /// Reserves and creates a table holding only the zero value, at id 0.
    ///
    /// `lock_dir` is where the process lock file is created and immediately
    /// unlinked. It is required on Unix and must be `None` on Windows. The
    /// table never looks at environment variables such as `TMPDIR`; the
    /// caller decides.
    ///
    /// # Errors
    ///
    /// See [`InitError`].
    pub fn new(config: Config, lock_dir: Option<&CStr>) -> Result<Self, InitError> {
        let page = os::page_size().ok_or(InitError::UnsupportedPageSize)?;
        let local = Local::<R>::create(config, lock_dir, page)?;
        let Some(at) = os::map_private(local.page_len) else {
            // SAFETY: nothing uses the new reservation.
            unsafe { os::release(local.base, local.total) };
            return Err(InitError::MappingFailed);
        };
        let at = at.cast::<Local<R>>();
        // SAFETY: the page is new, committed, page-aligned and large enough.
        unsafe { at.write(local) };
        let table = Self(at);
        if let Err(error) = table.local().initialize() {
            // SAFETY: this is the only handle.
            unsafe { table.destroy() };
            return Err(error);
        }
        Ok(table)
    }

    /// [`Self::new`], then interns `reserved` in order, writing value `i`'s id
    /// to `ids[i]`. The table is still private to this call, so nothing can
    /// hold its locks: interning cannot be busy.
    ///
    /// # Errors
    ///
    /// [`InitError::InvalidConfig`] if the slices' lengths differ or a value
    /// does not fit (the regions or arena are too small for `reserved`, or a
    /// string is too long), and otherwise as for [`Self::new`]. `ids` is then
    /// all zeros.
    fn with_reserved_by<T>(
        config: Config,
        lock_dir: Option<&CStr>,
        reserved: &[T],
        ids: &mut [Id<R>],
        key: impl Fn(&T) -> R::Key<'_>,
    ) -> Result<Self, InitError> {
        for id in ids.iter_mut() {
            *id = Id::ZERO;
        }
        if reserved.len() != ids.len() {
            return Err(InitError::InvalidConfig);
        }
        let table = Self::new(config, lock_dir)?;
        for (id, value) in ids.iter_mut().zip(reserved) {
            match table.intern(key(value)) {
                Ok(interned) => *id = interned,
                Err(error) => {
                    for id in ids.iter_mut() {
                        *id = Id::ZERO;
                    }
                    return Err(match error {
                        InternError::OutOfMemory => InitError::OutOfMemory,
                        InternError::LockFailed | InternError::Busy => InitError::LockFailed,
                        InternError::Full
                        | InternError::ArenaFull
                        | InternError::TooLong
                        | InternError::Corrupt => InitError::InvalidConfig,
                    });
                }
            }
        }
        Ok(table)
    }

    /// Another handle to the same table.
    ///
    /// # Errors
    ///
    /// [`TooManyHandles`] if the process already holds `isize::MAX` handles.
    pub fn try_clone(&self) -> Result<Self, TooManyHandles> {
        let handles = &self.local().handles;
        // Relaxed, as for `Arc`: a new handle is made from an existing one,
        // which already keeps the table alive.
        if handles.fetch_add(1, Ordering::Relaxed) >= MAX_HANDLES {
            handles.fetch_sub(1, Ordering::Relaxed);
            return Err(TooManyHandles);
        }
        Ok(Self(self.0))
    }

    /// Unmaps this process's view now, whatever the handle count.
    ///
    /// # Safety
    ///
    /// No other handle to this table in this process is used afterwards,
    /// including by dropping it.
    pub unsafe fn destroy(self) {
        let local = core::mem::ManuallyDrop::new(self).0;
        // SAFETY: the caller rules out every other use.
        unsafe { Local::unmap(local) };
    }

    /// Gives up the handle without dropping it, for FFI. Rebuild it with
    /// [`Self::from_raw`].
    #[must_use]
    pub fn into_raw(self) -> NonNull<core::ffi::c_void> {
        core::mem::ManuallyDrop::new(self).0.cast()
    }

    /// Takes back a handle given up by [`Self::into_raw`].
    ///
    /// # Safety
    ///
    /// `raw` came from `into_raw` on an `InternTable<R>` of the same `R`, in
    /// this process, and each `into_raw` is matched by at most one
    /// `from_raw`.
    #[must_use]
    pub const unsafe fn from_raw(raw: NonNull<core::ffi::c_void>) -> Self {
        Self(raw.cast())
    }

    /// Returns the id of `key`, adding it if it is new.
    ///
    /// A key found in a sealed region needs no lock. Otherwise this takes the
    /// writer locks, so even a key that is present can get
    /// [`InternError::Busy`].
    ///
    /// # Errors
    ///
    /// See [`InternError`]. Every error except [`InternError::LockFailed`]
    /// leaves the table unchanged.
    pub fn intern(&self, key: R::Key<'_>) -> Result<Id<R>, InternError> {
        self.local().intern(key)
    }

    /// The value with id `id`, or `None` if no such id has been published.
    #[must_use]
    pub fn resolve(&self, id: Id<R>) -> Option<R::Key<'_>> {
        self.local().resolve(id)
    }

    /// The number of published ids, including the zero value's.
    #[must_use]
    pub fn len(&self) -> usize {
        self.local().len()
    }

    /// Always false: every table holds the zero value.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The most ids the table can hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        usize_from(self.local().mem.max_ids)
    }

    /// The size of the reservation, in bytes of address space.
    #[must_use]
    pub const fn reserved_bytes(&self) -> usize {
        self.local().total
    }

    /// Takes this process's thread lock before a `fork`, waiting for it.
    /// Hold the guard across `fork`, then finish it in both the parent and
    /// the child before using the table; finish it in the parent even if
    /// `fork` fails.
    ///
    /// Without this, a child forked while another thread holds the thread
    /// lock would inherit it held, with no thread to release it. The process
    /// lock needs no such care: a child does not inherit record locks.
    ///
    /// With several tables, prepare them in a consistent order. Do not call
    /// this from a thread that is inside an operation on the same table.
    pub fn prepare_fork(&self) -> ForkGuard<'_> {
        ForkGuard {
            _thread_lock: self.local().thread_lock.lock(),
            _not_send: PhantomData,
        }
    }

    /// [`Self::prepare_fork`] for callers without a guard (FFI): takes this
    /// process's thread lock and keeps it held until
    /// [`Self::finish_fork`].
    pub fn prepare_fork_unguarded(&self) {
        core::mem::forget(self.prepare_fork());
    }

    /// Releases the thread lock taken by [`Self::prepare_fork_unguarded`]:
    /// call it once in the parent and once in the child after `fork`.
    ///
    /// # Safety
    ///
    /// This process holds the lock from `prepare_fork_unguarded` (directly,
    /// or inherited from its parent across `fork`), and releases it once.
    pub unsafe fn finish_fork(&self) {
        // SAFETY: the caller's contract.
        unsafe { self.local().thread_lock.force_unlock() };
    }

    const fn local(&self) -> &Local<R> {
        // SAFETY: a live handle keeps the page mapped.
        unsafe { self.0.as_ref() }
    }
}

impl<R: Record> Drop for InternTable<R> {
    fn drop(&mut self) {
        // Release, so this handle's uses happen before the unmap; the last
        // handle's acquire fence pairs with it.
        if self.local().handles.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        atomic::fence(Ordering::Acquire);
        // SAFETY: this was the last handle in this process.
        unsafe { Local::unmap(self.0) };
    }
}

impl<R: Record> fmt::Debug for InternTable<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let local = self.local();
        f.debug_struct("InternTable")
            .field("len", &local.len())
            .field("capacity", &self.capacity())
            .field("regions", &local.region_count)
            .field("reserved_bytes", &local.total)
            .finish_non_exhaustive()
    }
}

/// Holds a table's thread lock across a `fork`. See
/// [`InternTable::prepare_fork`].
#[must_use = "hold this across fork, then finish it in both processes"]
pub struct ForkGuard<'t> {
    _thread_lock: SpinLockGuard<'t>,
    _not_send: PhantomData<*mut ()>,
}

impl ForkGuard<'_> {
    /// Releases this process's thread lock.
    pub fn finish(self) {}
}

impl fmt::Debug for ForkGuard<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ForkGuard")
    }
}

/// Both writer locks. Releasing clears the dirty marker unless a mutation
/// was left unfinished.
struct Writer<'t, R: Record> {
    table: &'t Local<R>,
    thread_lock: Option<SpinLockGuard<'t>>,
    clean: bool,
}

impl<R: Record> Writer<'_, R> {
    fn finish(mut self) -> Result<(), InternError> {
        self.release()
    }

    fn release(&mut self) -> Result<(), InternError> {
        let Some(thread_lock) = self.thread_lock.take() else {
            return Ok(());
        };
        if self.clean {
            self.table.header().dirty.0.store(0, Ordering::Release);
        }
        let released = self.table.process_lock.release();
        if released.is_err() {
            // Fail closed: no thread of this process may write while its
            // process lock state is unknown.
            self.table.broken.store(true, Ordering::Release);
        }
        // Only now can another thread of this process take the thread lock.
        drop(thread_lock);
        released.map_err(|_| InternError::LockFailed)
    }
}

impl<R: Record> Drop for Writer<'_, R> {
    fn drop(&mut self) {
        // Early returns release here; `finish` reports release errors.
        let _ = self.release();
    }
}

/// Widens an id or count. Ids are `u32`, and the supported targets are
/// 64-bit, so this never truncates.
const fn usize_from(index: u32) -> usize {
    index as usize
}
