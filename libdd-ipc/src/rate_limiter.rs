// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
use crate::platform::lock_shm;
use crate::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use arc_swap::ArcSwapOption;
use libdd_common::rate_limiter::LocalLimiter;
use std::borrow::Cow;
use std::cell::UnsafeCell;
use std::ffi::{CStr, CString};
use std::fmt::{Debug, Formatter};
use std::io;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::Duration;

#[repr(C)]
#[derive(Default)]
struct ShmLimiterData<'a, Inner> {
    next_free: AtomicU32, // free list
    rc: AtomicI32,
    limiter: LocalLimiter,
    inner: UnsafeCell<Inner>,
    _phantom: PhantomData<&'a ShmLimiterArena<Inner>>,
}

impl<Inner> ShmLimiterData<'_, Inner> {
    /// Bring this slot into service: reset its limiter, let `init` write the payload, then
    /// publish it.
    ///
    /// Everything a searcher may observe has to be in place before the count goes positive,
    /// which is what makes that store a release and the matching load in `find` an acquire.
    /// Until then the slot is reserved but invisible - off the free list, and skipped by
    /// every scan.
    fn initialise_and_publish(&self, init: impl FnOnce(&Inner)) {
        self.limiter.reset();
        init(unsafe { &*self.inner.get() });
        self.rc.store(1, Ordering::Release);
    }
}

pub struct ShmLimiterMemory<Inner> {
    mem: ArcSwapOption<ShmLimiterArena<Inner>>,
    path: Option<ReaderPath>,
}

#[derive(Clone)]
enum ReaderPath {
    Fixed(CString),
    Dynamic(fn() -> CString),
}

impl ReaderPath {
    fn get(&self) -> Cow<'_, CStr> {
        match self {
            Self::Fixed(path) => Cow::Borrowed(path),
            Self::Dynamic(path) => Cow::Owned(path()),
        }
    }
}

struct ShmLimiterArena<Inner> {
    mem: MappedMem<NamedShmHandle>,
    _phantom: PhantomData<Inner>,
}

impl<Inner> Clone for ShmLimiterMemory<Inner> {
    fn clone(&self) -> Self {
        ShmLimiterMemory {
            mem: ArcSwapOption::new(self.mem.load_full()),
            path: self.path.clone(),
        }
    }
}

impl<Inner> ShmLimiterMemory<Inner> {
    /// Create a fresh arena at `path`.
    ///
    /// Retire previous arenas after initialization. This owner stays on its arena;
    /// readers follow replacements by name.
    pub fn create(path: CString) -> io::Result<Self> {
        let (handle, replaced) = NamedShmHandle::create_replacing(path, 0x1000)?;
        let mem = ShmLimiterArena::new(handle.map()?);
        mem.first_free_ref()
            .store(ShmLimiterArena::<Inner>::START_OFFSET, Ordering::Relaxed);
        for old in replaced {
            if let Ok(old) = old.map() {
                if let Some(retired) = ShmLimiterArena::<Inner>::retired_flag(old.as_slice()) {
                    retired.store(1, Ordering::Release);
                }
            }
        }
        Ok(Self {
            mem: ArcSwapOption::from_pointee(mem),
            path: None,
        })
    }

    /// Open an arena and follow replacements on subsequent lookups.
    pub fn open(path: &CString) -> io::Result<Self> {
        Ok(Self {
            mem: ArcSwapOption::from_pointee(ShmLimiterArena::new(
                NamedShmHandle::open(path)?.map()?,
            )),
            path: Some(ReaderPath::Fixed(path.clone())),
        })
    }

    /// Open on the first lookup, retrying failed opens and following replacements.
    pub fn new_reader(path: CString) -> Self {
        Self {
            mem: ArcSwapOption::empty(),
            path: Some(ReaderPath::Fixed(path)),
        }
    }

    /// Recompute the name whenever the reader needs to open an arena.
    pub const fn new_reader_with_path(path: fn() -> CString) -> Self {
        Self {
            mem: ArcSwapOption::const_empty(),
            path: Some(ReaderPath::Dynamic(path)),
        }
    }

    /// Retire a cached arena from an old namespace so the next lookup reopens it.
    /// On Unix, retire only this process's view.
    pub fn reconnect(&self) {
        if let (Some(mem), Some(path)) = (self.mem.load().as_ref(), &self.path) {
            // Reader handles are never unlinked through this cache.
            if unsafe { mem.mem.get_path() } != path.get().to_bytes() {
                #[cfg(unix)]
                let _guard = lock_shm();
                if mem.is_retired() {
                    return;
                }
                #[cfg(unix)]
                if let Err(error) = mem.mem.make_private() {
                    tracing::warn!("Failed to detach SHM limiter: {error}");
                    return;
                }
                if let Some(retired) = ShmLimiterArena::<Inner>::retired_flag(mem.mem.as_slice()) {
                    retired.store(1, Ordering::Release);
                }
            }
        }
    }

    /// Whether the cached arena was replaced. Existing slots remain valid.
    pub fn is_retired(&self) -> bool {
        self.mem.load().as_ref().is_some_and(|mem| mem.is_retired())
    }

    /// Unlink this arena if it still belongs to the current process.
    pub fn unlink(&self) {
        if self.path.is_none() {
            if let Some(mem) = self.mem.load().as_ref() {
                mem.mem.unlink();
            }
        }
    }

    fn with_current<R>(
        &self,
        f: impl FnOnce(&Arc<ShmLimiterArena<Inner>>) -> Option<R>,
    ) -> Option<R> {
        let current = self.mem.load();
        if let Some(mem) = current.as_ref().filter(|mem| !mem.is_retired()) {
            return f(mem);
        }
        let path = self.path.as_ref()?.get();
        let mem = Arc::new(ShmLimiterArena::new(
            NamedShmHandle::open(&path).ok()?.map().ok()?,
        ));
        // A slow opener must not overwrite another thread's replacement.
        self.mem.compare_and_swap(&current, Some(mem));
        let current = self.mem.load();
        current.as_ref().filter(|mem| !mem.is_retired()).and_then(f)
    }

    /// Allocate a slot whose payload needs no initialization.
    pub fn alloc(&mut self) -> Option<ShmLimiter<Inner>> {
        self.alloc_with(|_| ())
    }

    /// Initialize a slot before publishing it to readers. Returns `None` if the arena
    /// is retired or has no room.
    pub fn alloc_with(&mut self, init: impl FnOnce(&Inner)) -> Option<ShmLimiter<Inner>> {
        self.with_current(|mem| mem.alloc(init))
    }

    pub fn get(&self, idx: u32) -> Option<ShmLimiter<Inner>> {
        self.with_current(|mem| mem.get(idx))
    }

    pub fn find(&self, cond: impl Fn(&Inner) -> bool) -> Option<ShmLimiter<Inner>> {
        self.with_current(|mem| mem.find(cond))
    }
}

impl<Inner> ShmLimiterArena<Inner> {
    /// Slot alignment must leave room for the free-list head and retirement flag.
    const START_OFFSET: u32 = {
        assert!(align_of::<ShmLimiterData<Inner>>() >= 2 * size_of::<AtomicU32>());
        align_of::<ShmLimiterData<Inner>>() as u32
    };
    const STRIDE: u32 = size_of::<ShmLimiterData<Inner>>() as u32;
    const RETIRED_OFFSET: usize = size_of::<AtomicU32>();

    fn is_retired(&self) -> bool {
        Self::retired_flag(self.mem.as_slice())
            .is_some_and(|retired| retired.load(Ordering::Acquire) != 0)
    }

    fn retired_flag(slice: &[u8]) -> Option<&AtomicU32> {
        if slice.len() < Self::RETIRED_OFFSET + size_of::<AtomicU32>() {
            return None;
        }
        // SAFETY: in bounds as just checked, and the offset is a multiple of the word size in a
        // page-aligned mapping.
        Some(unsafe { &*slice.as_ptr().add(Self::RETIRED_OFFSET).cast() })
    }

    fn new(handle: MappedMem<NamedShmHandle>) -> Self {
        Self {
            mem: handle,
            _phantom: Default::default(),
        }
    }

    /// The header's `AtomicU32`, if the mapping is long enough to hold one.
    fn header(slice: &[u8]) -> Option<&AtomicU32> {
        if slice.len() < size_of::<AtomicU32>() {
            return None;
        }
        // SAFETY: the mapping is at least one `AtomicU32` long, as just checked, and
        // page-aligned mappings are aligned for it.
        Some(unsafe { &*slice.as_ptr().cast() })
    }

    /// The slot at `idx`, if it lies wholly inside `slice`.
    ///
    /// `idx` can come from the free-list head, which any worker holding the segment may
    /// write, so it is checked against the mapping's actual length - never against a size
    /// recorded inside the mapping.
    fn slot(slice: &[u8], idx: u32) -> Option<&ShmLimiterData<'_, Inner>> {
        // Slots start at START_OFFSET - the type's alignment - and sit back to back, so
        // anything off that grid would also be misaligned for the atomics inside.
        if idx < Self::START_OFFSET || !(idx - Self::START_OFFSET).is_multiple_of(Self::STRIDE) {
            return None;
        }
        if idx.checked_add(Self::STRIDE)? as usize > slice.len() {
            return None;
        }
        // SAFETY: the slot lies wholly within the mapping and is correctly aligned, both
        // checked just above.
        Some(unsafe { &*slice.as_ptr().add(idx as usize).cast() })
    }

    /// Never extends the mapping. Scans rely on this refusing the first slot past the end in
    /// order to terminate, and no access other than an explicit lookup should enlarge
    /// anything.
    fn with_slot<R>(&self, idx: u32, f: impl FnOnce(&ShmLimiterData<Inner>) -> R) -> Option<R> {
        Self::slot(self.mem.as_slice(), idx).map(f)
    }

    /// As [`Self::with_slot`], but first extends this process's view to reach `idx`.
    ///
    /// Only for looking up one specific index: an index outside our mapping is usually growth
    /// another process performed, and a reader that cannot follow it silently stops limiting.
    /// `ensure_mapped` bounds how far that goes. Scans must not use this - reaching the end of
    /// the mapping is how they terminate, not a reason to allocate.
    fn with_slot_extending<R>(
        &self,
        idx: u32,
        f: impl FnOnce(&ShmLimiterData<Inner>) -> R,
    ) -> Option<R> {
        let end = idx.checked_add(Self::STRIDE)? as usize;
        if self.mem.as_slice().len() < end {
            self.ensure_mapped(end)?;
        }
        self.with_slot(idx, f)
    }

    /// The free-list head, in the mapping's first word.
    ///
    /// Safe to hand out as a reference: the mapping outlives this handle and never moves, so
    /// growing the arena cannot invalidate it.
    fn first_free_ref(&self) -> &AtomicU32 {
        // SAFETY: a segment is created a page long, so its first word is always mapped, and
        // page-aligned mappings are aligned for an AtomicU32.
        unsafe { &*self.mem.as_slice().as_ptr().cast() }
    }

    /// Make sure the segment covers `needed` bytes.
    ///
    /// An offset past the end of our view is the ordinary consequence of another process
    /// having grown the segment, so following one must be possible. What stops a forged offset
    /// from turning into an arbitrary allocation is the mapping's own reservation: it is fixed
    /// when the segment is mapped, and this refuses anything beyond it.
    fn ensure_mapped(&self, needed: usize) -> Option<()> {
        self.mem.ensure_space(needed).then_some(())
    }

    fn next_free(&self) -> Option<u32> {
        let mut first_free = self.first_free_ref().load(Ordering::Relaxed);
        loop {
            let mut target_next_free =
                self.with_slot_extending(first_free, |l| l.next_free.load(Ordering::Relaxed))?;
            // Not yet used memory will always be 0. The next free entry will then be just above.
            if target_next_free == 0 {
                target_next_free = first_free.checked_add(Self::STRIDE)?;

                // Ensure target_next_free points to valid memory
                self.ensure_mapped(target_next_free.checked_add(Self::STRIDE)? as usize)?;
            }

            match self.first_free_ref().compare_exchange(
                first_free,
                target_next_free,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(first_free),
                Err(found) => first_free = found,
            }
        }
    }

    fn alloc(self: &Arc<Self>, init: impl FnOnce(&Inner)) -> Option<ShmLimiter<Inner>> {
        let reference = ShmLimiter::owning(self.next_free()?, self.clone());
        reference.with_limiter(|slot| slot.initialise_and_publish(init))?;
        Some(reference)
    }

    fn get(self: &Arc<Self>, idx: u32) -> Option<ShmLimiter<Inner>> {
        let acquired = self.with_slot_extending(idx, |limiter| {
            let mut rc = limiter.rc.load(Ordering::Acquire);
            loop {
                // A peer can write this count, so only a positive one describes a slot with
                // owners to join: zero is retired, and negative is accounting we cannot
                // trust. A count that cannot be incremented is not one we join either.
                let Some(next) = rc.checked_add(1).filter(|_| rc > 0) else {
                    return false;
                };
                match limiter
                    .rc
                    .compare_exchange(rc, next, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => return true,
                    Err(found) => rc = found,
                }
            }
        })?;
        // Only now: `ShmLimiter`'s `Drop` subtracts one, so a handle built before the
        // compare-exchange landed would release a reference on every refusal.
        acquired.then(|| ShmLimiter::owning(idx, self.clone()))
    }

    fn find<F>(self: &Arc<Self>, cond: F) -> Option<ShmLimiter<Inner>>
    where
        F: Fn(&Inner) -> bool,
    {
        let mut cur = Self::START_OFFSET;
        // Refresh only after a miss, then scan the newly backed range.
        let limits = std::iter::once(self.mem.get_size())
            .chain(std::iter::once_with(|| self.mem.refresh_size()));
        for limit in limits {
            let limit = u32::try_from(limit).ok()?;
            while cur
                .checked_add(Self::STRIDE)
                .is_some_and(|end| end <= limit)
            {
                let hit = self.with_slot(cur, |data| {
                    // `rc` alone decides liveness. `next_free` is the free-list link, and a slot
                    // allocated from untouched tail space still has the zero it was mapped with,
                    // so gating on it would hide every slot that has not been recycled yet. It
                    // is not needed as a terminator either: `limit` bounds the scan.
                    //
                    // Acquire, to pair with the release-store that publishes an allocation: it
                    // is what makes "the count is positive" mean "the payload below is this
                    // allocation's, and not the one before it".
                    data.rc.load(Ordering::Acquire) > 0 && cond(unsafe { &*data.inner.get() })
                })?;
                if hit {
                    if let Some(limiter) = self.get(cur) {
                        if limiter.with_data(&cond).unwrap_or(false) {
                            return Some(limiter);
                        }
                    }
                }
                cur = cur.checked_add(Self::STRIDE)?;
            }
        }
        None
    }
}

pub struct ShmLimiter<Inner> {
    idx: u32,
    /// The process that acquired the reference this handle stands for.
    ///
    /// `fork` copies the handle without copying the reference: one acquisition ends up with
    /// two destructors. Recording who took it is what lets the other one decline to give it
    /// back. See [`Drop`].
    owner_pid: u32,
    // Slot indices and reference counts belong to this arena, even after replacement.
    memory: Arc<ShmLimiterArena<Inner>>,
}

impl<Inner> Debug for ShmLimiter<Inner> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.idx.fmt(f)
    }
}

impl<Inner> ShmLimiter<Inner> {
    /// A handle owning one reference to `idx`, stamped with the process taking it.
    fn owning(idx: u32, memory: Arc<ShmLimiterArena<Inner>>) -> Self {
        ShmLimiter {
            idx,
            owner_pid: std::process::id(),
            memory,
        }
    }

    /// Run `f` on this slot.
    ///
    /// `None` means the offset does not describe a slot wholly inside the mapping. Contents
    /// are not trusted and do not need to be: a worker owning this segment may put anything in
    /// it, including in the free-list head that chose this offset.
    fn with_limiter<R>(&self, f: impl FnOnce(&ShmLimiterData<Inner>) -> R) -> Option<R> {
        self.memory.with_slot(self.idx, f)
    }

    pub fn with_data<R>(&self, f: impl FnOnce(&Inner) -> R) -> Option<R> {
        self.with_limiter(|limiter| f(unsafe { &*limiter.inner.get() }))
    }

    pub fn index(&self) -> u32 {
        self.idx
    }

    pub fn is_active(&self) -> bool {
        self.with_limiter(|limiter| limiter.limiter.is_active())
            .unwrap_or(false)
    }

    pub fn inc(&self, limit: u32, granularity: Duration) -> bool {
        self.with_limiter(|limiter| limiter.limiter.inc(limit, granularity))
            .unwrap_or(false)
    }

    pub fn rate(&self, granularity: Duration) -> f64 {
        self.with_limiter(|limiter| limiter.limiter.rate(granularity))
            .unwrap_or(0.0)
    }

    /// # Safety
    /// Callers MUST NOT do any other operations on this instance if dropping was successful.
    pub unsafe fn drop_if_rc_1(&mut self) -> bool {
        let dropped = self
            .with_limiter(|limiter| {
                limiter
                    .rc
                    .compare_exchange(1, 0, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
            })
            .unwrap_or(false);
        if dropped {
            self.actual_free();
            self.idx = 0;
        }
        dropped
    }

    fn actual_free(&self) {
        // Header, slot link and compare-exchange all against one view of the segment, so a
        // growth between the steps cannot leave the link and the head describing different
        // extents.
        let slice = self.memory.mem.as_slice();
        let (Some(header), Some(limiter)) = (
            ShmLimiterArena::<Inner>::header(slice),
            ShmLimiterArena::<Inner>::slot(slice, self.idx),
        ) else {
            return;
        };
        let mut next_free = header.load(Ordering::Relaxed);
        loop {
            // Whatever ends up in the link is bounds-checked before it is ever followed.
            limiter.next_free.store(next_free, Ordering::Relaxed);
            match header.compare_exchange(next_free, self.idx, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(found) => next_free = found,
            }
        }
    }
}

impl<Inner> Drop for ShmLimiter<Inner> {
    fn drop(&mut self) {
        if self.idx == 0 {
            return;
        }

        if self.owner_pid != std::process::id() {
            return;
        }

        if self
            .with_limiter(|limiter| limiter.rc.fetch_sub(1, Ordering::SeqCst) == 1)
            .unwrap_or(false)
        {
            self.actual_free();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::rate_limiter::{ShmLimiter, ShmLimiterArena, ShmLimiterData, ShmLimiterMemory};
    use std::ffi::CString;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    const ONE_SECOND: Duration = Duration::from_secs(1);

    fn path() -> CString {
        CString::new("/ddlimiters-test".to_string()).unwrap()
    }

    fn arena<Inner>(memory: &ShmLimiterMemory<Inner>) -> Arc<ShmLimiterArena<Inner>> {
        memory.mem.load_full().unwrap()
    }

    /// The mapping's first word is the free-list head, and every peer holding the segment can
    /// write it. Its contents are theirs to corrupt; what must hold is that nothing derived
    /// from them reaches outside the mapping.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_forged_free_list_head_stays_inside_the_mapping() {
        let path = CString::new("/ddlimiters-forged".to_string()).unwrap();
        let mut limiters = ShmLimiterMemory::<()>::create(path).unwrap();

        // An offset well past the end of the one-page segment.
        arena(&limiters)
            .first_free_ref()
            .store(8192, Ordering::Relaxed);

        // The offset may be honoured - it is the segment's own accounting, and the worker owns
        // that - but only ever by extending the mapping to cover it. What must not happen is
        // an access landing outside.
        let stride = size_of::<ShmLimiterData<()>>();
        if let Some(limiter) = limiters.alloc() {
            let mapped = arena(&limiters).mem.as_slice().len();
            assert!(
                limiter.idx as usize + stride <= mapped,
                "slot at {} escapes the {mapped}-byte mapping",
                limiter.idx
            );
            assert!(
                limiter.inc(2, ONE_SECOND),
                "and it must be a working limiter"
            );
        }

        // Far past the reservation: nothing to honour, and nothing allocated either.
        arena(&limiters)
            .first_free_ref()
            .store(u32::MAX - 64, Ordering::Relaxed);
        assert!(
            limiters.alloc().is_none(),
            "an offset beyond the reservation must be refused, not allocated"
        );
    }

    /// A scan that finds nothing must not allocate: exception-hash lookup scans before
    /// requesting a new limiter, so a miss is the common case, not an error path.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn an_unsuccessful_scan_does_not_grow_the_arena() {
        let path = CString::new("/ddlimiters-scan".to_string()).unwrap();
        let limiters = ShmLimiterMemory::<()>::create(path).unwrap();

        let before = arena(&limiters).mem.as_slice().len();
        assert!(limiters.find(|_| false).is_none(), "nothing to find");
        let after = arena(&limiters).mem.as_slice().len();

        assert_eq!(
            before, after,
            "a miss must leave the mapping the size it was"
        );
    }

    /// Malformed process-local configuration and peer-writable reference counts must not panic.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn malformed_limiter_values_do_not_panic() {
        let path = CString::new("/ddlimiters-malformed".to_string()).unwrap();
        let mut limiters = ShmLimiterMemory::<()>::create(path).unwrap();

        // Zero arrives straight from an RPC argument and must be rejected before arithmetic.
        let limiter = limiters.alloc().unwrap();
        assert!(!limiter.inc(1, Duration::ZERO));

        // A count a peer pushed to the edge cannot be joined - but must not overflow saying so.
        limiter.with_limiter(|l| l.rc.store(i32::MAX, Ordering::Relaxed));
        assert!(
            limiters.get(limiter.index()).is_none(),
            "a refcount that cannot be incremented must be refused, not wrapped"
        );
        // Checking only the returned Option would miss a refusal that still moved the count.
        assert_eq!(
            limiter.with_limiter(|l| l.rc.load(Ordering::SeqCst)),
            Some(i32::MAX),
            "refusing the acquisition must have left the count untouched"
        );

        // Put the count back where `Drop` can retire the slot, so the test leaves the arena
        // as it found it.
        limiter.with_limiter(|l| l.rc.store(1, Ordering::Relaxed));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn granularity_is_supplied_by_the_caller() {
        let path = CString::new("/ddlimiters-granularity".to_string()).unwrap();
        let mut limiters = ShmLimiterMemory::<()>::create(path).unwrap();
        let allocated = limiters.alloc().unwrap();
        let acquired = limiters.get(allocated.index()).unwrap();

        assert_eq!(allocated.index(), acquired.index());
        assert!(allocated.inc(1, Duration::from_secs(60 * 60)));
        assert!(!acquired.inc(1, Duration::from_secs(7 * 60 * 60)));
    }

    /// An index that never came from the allocator is refused rather than dereferenced - and
    /// not asserted on either, since it arrives from a peer.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn indices_outside_the_mapping_are_refused() {
        let path = CString::new("/ddlimiters-bounds".to_string()).unwrap();
        let limiters = ShmLimiterMemory::<()>::create(path).unwrap();
        let start = ShmLimiterArena::<()>::START_OFFSET;
        for idx in [0, 1, start - 1, start + 1, 8192, u32::MAX] {
            assert!(
                limiters.get(idx).is_none(),
                "index {idx} is not one of ours and must be refused"
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_limiters() {
        let mut limiters = ShmLimiterMemory::<()>::create(path()).unwrap();
        let limiter = limiters.alloc().unwrap();
        let limiter_idx = limiter.idx;
        assert!(limiter.inc(1, ONE_SECOND));

        // Now test the free list
        let limiter2 = limiters.alloc().unwrap();
        assert_eq!(
            limiter2.idx,
            limiter_idx + size_of::<ShmLimiterData<()>>() as u32
        );
        drop(limiter);

        let limiter = limiters.alloc().unwrap();
        assert_eq!(limiter.idx, limiter_idx);
        assert!(
            limiter.inc(1, ONE_SECOND),
            "reusing a slot must clear its deadline"
        );

        let limiter3 = limiters.alloc().unwrap();
        assert_eq!(
            limiter3.idx,
            limiter2.idx + size_of::<ShmLimiterData<()>>() as u32
        );
    }

    /// Concurrent allocators must never be handed the same slot.
    ///
    /// The free list is a CAS loop over a head word in shared memory, and nothing serializes
    /// the clones that drive it - the fixed mapping removed the lock that used to stand
    /// between them, so the head CAS is now the only thing keeping two allocators apart.
    /// Contend for far more slots than the initial page holds, so growth happens repeatedly
    /// underneath the contention, and require every index to come back distinct and usable.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn concurrent_allocation_across_arena_growth() {
        const THREADS: usize = 4;

        let path = CString::new("/ddlimiters-grow".to_string()).unwrap();
        let limiters = ShmLimiterMemory::<()>::create(path).unwrap();
        let initial_slots =
            (0x1000 - align_of::<ShmLimiterData<()>>()) / size_of::<ShmLimiterData<()>>();
        let per_thread = (2 * initial_slots).div_ceil(THREADS);
        assert!(
            THREADS * per_thread > initial_slots,
            "the test must outgrow the initial mapping to exercise growth at all"
        );

        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let mut mine = limiters.clone();
                std::thread::spawn(move || {
                    // Held until every thread is done, so no slot is recycled and the indices
                    // below have to be distinct.
                    (0..per_thread)
                        .map(|_| {
                            let limiter = mine.alloc().expect("the arena has room");
                            assert!(
                                limiter.inc(1000, ONE_SECOND),
                                "a fresh limiter must admit a hit"
                            );
                            limiter
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();

        let allocated: Vec<ShmLimiter<()>> = threads
            .into_iter()
            .flat_map(|t| t.join().expect("no allocator thread may panic"))
            .collect();

        let mut indices: Vec<u32> = allocated.iter().map(|l| l.idx).collect();
        let total = indices.len();
        assert_eq!(total, THREADS * per_thread);
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(
            indices.len(),
            total,
            "two allocations returned the same slot"
        );
    }

    /// Bringing a slot into service must not mutate anything through a shared reference.
    ///
    /// The allocator only ever holds a `&ShmLimiterData`: slots live in a mapping shared
    /// with other processes, so no exclusive borrow exists to be taken. Rust's rule is that
    /// bytes reachable through a live shared reference do not change unless they sit in an
    /// `UnsafeCell`, and the limiter's own state is not payload, so it is not in one - every
    /// field of it therefore has to be individually interior-mutable.
    ///
    /// This drives the real publication path against a slot in ordinary memory, with no
    /// mapping involved, so Miri can see it:
    /// `cargo +nightly miri test -p libdd-ipc publishing_a_slot`.
    #[test]
    fn publishing_a_slot_does_not_mutate_through_a_shared_reference() {
        let slot = ShmLimiterData::<()>::default();

        // Exactly what the allocator gets, and it stays live across the call.
        let shared: &ShmLimiterData<()> = &slot;
        shared.initialise_and_publish(|_| ());

        assert_eq!(
            shared.rc.load(Ordering::SeqCst),
            1,
            "the slot must come back published"
        );
        assert!(
            shared.limiter.inc(1, Duration::from_secs(7)),
            "with a working limiter"
        );
    }

    /// A recycled slot must not be searchable until the allocation that took it has finished
    /// writing it.
    ///
    /// Entries are found by their payload, and freeing a slot does not scrub it, so a slot
    /// published before its new payload is written matches under its *previous* tenant's
    /// contents - and whoever matches it there is handed a budget meant for something else.
    ///
    /// The assertions run from inside the initialisation callback, because that is the only
    /// moment the window exists: reserved, being written, not yet published. Publishing
    /// before returning to the caller, and letting the caller fill the payload afterwards,
    /// is what turns that window into one an unrelated process can observe.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_slot_is_not_searchable_until_its_payload_is_written() {
        const OLD: u64 = 0xbad;
        const NEW: u64 = 0x900d;

        fn payload(mem: &ShmLimiterMemory<AtomicU64>, idx: u32) -> Option<u64> {
            arena(mem).with_slot(idx, |slot| {
                unsafe { &*slot.inner.get() }.load(Ordering::Relaxed)
            })
        }

        let path = CString::new("/ddlimiters-publish".to_string()).unwrap();
        let mut limiters = ShmLimiterMemory::<AtomicU64>::create(path).unwrap();

        let first = limiters
            .alloc_with(|hash| hash.store(OLD, Ordering::Relaxed))
            .unwrap();
        let idx = first.index();
        drop(first);

        assert_eq!(
            payload(&limiters, idx),
            Some(OLD),
            "freeing leaves the payload where it was - which is what makes the window matter"
        );

        // A second handle on the same arena scans while the first allocates: the same thing
        // a client process does, minus the process boundary, and with nothing between them.
        let searcher = limiters.clone();
        let second = limiters
            .alloc_with(|hash| {
                assert!(
                    searcher
                        .find(|h| h.load(Ordering::Relaxed) == OLD)
                        .is_none(),
                    "a slot being initialised must not still match its previous tenant"
                );
                hash.store(NEW, Ordering::Relaxed);
                assert!(
                    searcher
                        .find(|h| h.load(Ordering::Relaxed) == NEW)
                        .is_none(),
                    "nor match its new payload before that payload is published"
                );
            })
            .unwrap();

        assert_eq!(
            second.index(),
            idx,
            "the free list hands the same slot back"
        );
        assert_eq!(
            limiters
                .find(|h| h.load(Ordering::Relaxed) == NEW)
                .map(|found| found.index()),
            Some(idx),
            "and once published it is findable under the payload it was given"
        );
        assert!(
            limiters
                .find(|h| h.load(Ordering::Relaxed) == OLD)
                .is_none(),
            "and never again under the one it replaced"
        );
    }

    /// A handle inherited through `fork` must not release a reference it never took.
    ///
    /// The handle is ordinary process memory: `fork` copies its bytes into the child, while
    /// the count it stands for is one increment in shared memory and stays one. Both copies
    /// still run the same destructor, so an orderly parent and child between them give a
    /// single acquisition back twice - retiring a slot its real owner still holds, and
    /// handing it to the next allocation while that owner goes on using it.
    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn a_handle_inherited_through_fork_is_not_released_twice() {
        fn rc(mem: &ShmLimiterMemory<()>, idx: u32) -> i32 {
            #[allow(clippy::unwrap_used)]
            arena(mem)
                .with_slot(idx, |slot| slot.rc.load(Ordering::SeqCst))
                .unwrap()
        }

        let path = CString::new("/ddlimiters-fork".to_string()).unwrap();
        let mut limiters = ShmLimiterMemory::<()>::create(path).unwrap();

        // Stands in for the worker that allocated the slot: its reference is what keeps the
        // slot alive while anybody else holds one.
        let owner = limiters.alloc().unwrap();
        let idx = owner.index();

        let client = limiters.get(idx).expect("the slot is live");
        assert_eq!(rc(&limiters, idx), 2, "owner plus client");

        // SAFETY: the child only drops a handle - an atomic load and at most an atomic
        // subtract, no allocation, no locks, no I/O - and leaves through `_exit`, so it runs
        // no destructors of its own and never touches state the harness's other threads may
        // have been holding when it forked.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed: {}", std::io::Error::last_os_error()),
            0 => {
                drop(client);
                unsafe { libc::_exit(0) }
            }
            child => {
                let mut status = 0;
                assert_eq!(
                    unsafe { libc::waitpid(child, &mut status, 0) },
                    child,
                    "waiting for the child must succeed"
                );
                assert_eq!(status, 0, "the child must have exited cleanly");
                assert_eq!(
                    rc(&limiters, idx),
                    2,
                    "the inherited copy must not have released the parent's reference"
                );

                drop(client);
                assert_eq!(
                    rc(&limiters, idx),
                    1,
                    "while the acquirer's own release still counts, exactly once"
                );

                // The owner still holds the slot it allocated, not one reissued to somebody
                // else in the meantime.
                assert_eq!(owner.index(), idx);
                assert!(
                    owner.inc(2, ONE_SECOND),
                    "the owner's limiter must still be its own"
                );
            }
        }
    }

    /// A worker that creates the arena must get a pristine one, even when an earlier worker
    /// left its segment behind by dying without running a destructor.
    ///
    /// Adopting the segment would keep its old slots: counts, links, limiter state and payloads
    /// would all survive into an arena that believes it is empty, and the first allocation
    /// would store `rc = 1` over a slot an older process may still hold a handle to.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_restarted_owner_does_not_adopt_a_stale_arena() {
        fn payload(mem: &ShmLimiterMemory<AtomicU64>, idx: u32) -> Option<u64> {
            arena(mem).with_slot(idx, |slot| {
                unsafe { &*slot.inner.get() }.load(Ordering::Relaxed)
            })
        }

        let path = CString::new(format!("/ddlimiters-restart-{}", std::process::id())).unwrap();

        let mut first = ShmLimiterMemory::<AtomicU64>::create(path.clone()).unwrap();
        let stale = first
            .alloc_with(|hash| hash.store(42, Ordering::Relaxed))
            .unwrap();
        let idx = stale.index();
        let client = ShmLimiterMemory::<AtomicU64>::open(&path).unwrap();
        assert!(!client.is_retired());

        // The replacement worker starts while the old segment is still there and still
        // mapped, which is what an abrupt exit leaves behind.
        let replacement = ShmLimiterMemory::<AtomicU64>::create(path.clone()).unwrap();

        assert_eq!(
            arena(&replacement).with_slot(idx, |slot| slot.rc.load(Ordering::SeqCst)),
            Some(0),
            "a fresh arena must not inherit an owner count"
        );
        assert_eq!(
            payload(&replacement, idx),
            Some(0),
            "nor a payload from the arena it replaced"
        );
        assert!(
            replacement
                .find(|hash| hash.load(Ordering::Relaxed) == 42)
                .is_none(),
            "and the stale entry must not be findable in it"
        );
        assert!(!replacement.is_retired());

        // Whoever still holds the old arena is told to move on...
        assert!(
            client.is_retired(),
            "a client of the old arena must see it retired"
        );
        assert!(first.is_retired());
        // ... but keeps a working one meanwhile, rather than sharing slots with the new owner.
        assert_eq!(
            stale.with_data(|hash| hash.load(Ordering::Relaxed)),
            Some(42),
            "the old arena must be undisturbed"
        );
        assert!(stale.inc(1, ONE_SECOND), "and its limiter must still work");

        let reopened = ShmLimiterMemory::<AtomicU64>::open(&path).unwrap();
        assert!(
            !reopened.is_retired(),
            "reopening by name reaches the replacement"
        );
        assert!(
            reopened
                .find(|hash| hash.load(Ordering::Relaxed) == 42)
                .is_none()
        );
    }

    /// Dropping a client of an arena rewrites the free-list head, so that word cannot double as
    /// the retirement signal: a client that let go of its last slot must not make the arena
    /// look retired.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn releasing_a_slot_does_not_retire_the_arena() {
        let path = CString::new(format!("/ddlimiters-release-{}", std::process::id())).unwrap();
        let mut limiters = ShmLimiterMemory::<()>::create(path.clone()).unwrap();
        let client = ShmLimiterMemory::<()>::open(&path).unwrap();
        let limiter = limiters.alloc().unwrap();
        let shared = client.get(limiter.index()).unwrap();
        drop(limiter);
        drop(shared);
        assert!(!client.is_retired());
        assert!(!limiters.is_retired());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_reader_follows_replacement_without_moving_existing_slots() {
        let path = CString::new(format!("/ddlim-swap-{}", std::process::id())).unwrap();
        let mut old = ShmLimiterMemory::<AtomicU64>::create(path.clone()).unwrap();
        let old_slot = old
            .alloc_with(|hash| hash.store(11, Ordering::Relaxed))
            .unwrap();
        let reader = ShmLimiterMemory::<AtomicU64>::open(&path).unwrap();
        let pinned = reader.get(old_slot.index()).unwrap();

        let mut new = ShmLimiterMemory::<AtomicU64>::create(path).unwrap();
        let new_slot = new
            .alloc_with(|hash| hash.store(22, Ordering::Relaxed))
            .unwrap();
        assert_eq!(old_slot.index(), new_slot.index());
        let current = reader.get(new_slot.index()).unwrap();
        assert_eq!(
            current.with_data(|hash| hash.load(Ordering::Relaxed)),
            Some(22)
        );
        assert_eq!(
            pinned.with_data(|hash| hash.load(Ordering::Relaxed)),
            Some(11)
        );

        assert!(new_slot.inc(10, ONE_SECOND));
        let new_rate = new_slot.rate(ONE_SECOND);
        assert!(pinned.inc(1, ONE_SECOND));
        assert!(new_slot.rate(ONE_SECOND) <= new_rate);
        assert!(old_slot.rate(ONE_SECOND) > 0.0);
        drop(pinned);
        assert_eq!(
            old_slot.with_limiter(|slot| slot.rc.load(Ordering::Relaxed)),
            Some(1)
        );
        assert_eq!(
            new_slot.with_limiter(|slot| slot.rc.load(Ordering::Relaxed)),
            Some(2)
        );
        drop(current);
        assert_eq!(
            new_slot.with_limiter(|slot| slot.rc.load(Ordering::Relaxed)),
            Some(1)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_reader_follows_replacement_when_searching() {
        let path = CString::new(format!("/ddlim-find-{}", std::process::id())).unwrap();
        let mut old = ShmLimiterMemory::<AtomicU64>::create(path.clone()).unwrap();
        let old_slot = old
            .alloc_with(|hash| hash.store(11, Ordering::Relaxed))
            .unwrap();
        let reader = ShmLimiterMemory::<AtomicU64>::open(&path).unwrap();
        assert!(
            reader
                .find(|hash| hash.load(Ordering::Relaxed) == 11)
                .is_some()
        );

        let mut new = ShmLimiterMemory::<AtomicU64>::create(path).unwrap();
        let new_slot = new
            .alloc_with(|hash| hash.store(22, Ordering::Relaxed))
            .unwrap();
        assert!(
            reader
                .find(|hash| hash.load(Ordering::Relaxed) == 11)
                .is_none()
        );
        assert_eq!(
            reader
                .find(|hash| hash.load(Ordering::Relaxed) == 22)
                .map(|slot| slot.index()),
            Some(new_slot.index())
        );
        assert_eq!(
            old_slot.with_data(|hash| hash.load(Ordering::Relaxed)),
            Some(11)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_reader_retries_after_a_failed_open() {
        let path = CString::new(format!("/ddlim-late-{}", std::process::id())).unwrap();
        let reader = ShmLimiterMemory::<()>::new_reader(path.clone());
        assert!(reader.get(ShmLimiterArena::<()>::START_OFFSET).is_none());
        assert!(reader.find(|_| true).is_none());

        let mut owner = ShmLimiterMemory::<()>::create(path).unwrap();
        let slot = owner.alloc().unwrap();
        assert_eq!(reader.get(slot.index()).unwrap().index(), slot.index());
        assert_eq!(reader.find(|_| true).unwrap().index(), slot.index());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn reconnect_follows_the_current_name() {
        static NAMESPACE: AtomicU64 = AtomicU64::new(0);
        fn path() -> CString {
            CString::new(format!(
                "/ddlim-name-{}-{}",
                std::process::id(),
                NAMESPACE.load(Ordering::Relaxed)
            ))
            .unwrap()
        }

        let reader = ShmLimiterMemory::<AtomicU64>::new_reader_with_path(path);
        let mut old = ShmLimiterMemory::create(path()).unwrap();
        let old_slot = old
            .alloc_with(|hash: &AtomicU64| hash.store(11, Ordering::Relaxed))
            .unwrap();
        assert!(
            reader
                .find(|hash| hash.load(Ordering::Relaxed) == 11)
                .is_some()
        );
        reader.reconnect();
        assert!(!old.is_retired());

        let cloned_reader = reader.clone();
        NAMESPACE.store(1, Ordering::Relaxed);
        reader.reconnect();
        assert!(reader.is_retired());
        assert!(cloned_reader.is_retired());
        assert_eq!(old.is_retired(), cfg!(windows));
        assert!(reader.get(old_slot.index()).is_none());
        let mut new = ShmLimiterMemory::create(path()).unwrap();
        let new_slot = new
            .alloc_with(|hash: &AtomicU64| hash.store(22, Ordering::Relaxed))
            .unwrap();
        assert_eq!(old_slot.index(), new_slot.index());
        assert_eq!(
            reader
                .find(|hash| hash.load(Ordering::Relaxed) == 22)
                .unwrap()
                .index(),
            new_slot.index()
        );
        reader.reconnect();
        assert!(!new.is_retired());
        reader.unlink();
        drop(reader);
        assert!(ShmLimiterMemory::<AtomicU64>::open(&path()).is_ok());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_search_keeps_one_arena_when_replaced_mid_scan() {
        let path = CString::new(format!("/ddlim-scan-{}", std::process::id())).unwrap();
        let mut old = ShmLimiterMemory::<AtomicU64>::create(path.clone()).unwrap();
        let old_slot = old
            .alloc_with(|hash| hash.store(11, Ordering::Relaxed))
            .unwrap();
        let reader = ShmLimiterMemory::<AtomicU64>::open(&path).unwrap();
        let replacement = std::cell::RefCell::new(None);

        let found = reader
            .find(|hash| {
                let mut replacement = replacement.borrow_mut();
                if replacement.is_none() {
                    let mut new = ShmLimiterMemory::<AtomicU64>::create(path.clone()).unwrap();
                    let slot = new
                        .alloc_with(|hash| hash.store(22, Ordering::Relaxed))
                        .unwrap();
                    *replacement = Some((new, slot));
                }
                hash.load(Ordering::Relaxed) == 11
            })
            .unwrap();

        assert_eq!(
            found.with_data(|hash| hash.load(Ordering::Relaxed)),
            Some(11)
        );
        assert_eq!(
            reader
                .get(old_slot.index())
                .unwrap()
                .with_data(|hash| hash.load(Ordering::Relaxed)),
            Some(22)
        );
        assert!(
            old.alloc().is_none(),
            "a retired owner must not allocate in its successor"
        );
        drop(found);
        assert_eq!(
            old_slot.with_limiter(|slot| slot.rc.load(Ordering::Relaxed)),
            Some(1)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn concurrent_readers_follow_replacement() {
        let path = CString::new(format!("/ddlim-race-{}", std::process::id())).unwrap();
        let mut old = ShmLimiterMemory::<AtomicU64>::create(path.clone()).unwrap();
        let old_slot = old
            .alloc_with(|hash| hash.store(11, Ordering::Relaxed))
            .unwrap();
        let reader = ShmLimiterMemory::<AtomicU64>::open(&path).unwrap();
        let mut new = ShmLimiterMemory::<AtomicU64>::create(path).unwrap();
        let new_slot = new
            .alloc_with(|hash| hash.store(22, Ordering::Relaxed))
            .unwrap();
        let barrier = std::sync::Barrier::new(8);

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    let slot = reader.get(new_slot.index()).unwrap();
                    assert_eq!(
                        slot.with_data(|hash| hash.load(Ordering::Relaxed)),
                        Some(22)
                    );
                });
            }
        });
        assert!(!reader.is_retired());
        assert_eq!(
            old_slot.with_limiter(|slot| slot.rc.load(Ordering::Relaxed)),
            Some(1)
        );
        assert_eq!(
            new_slot.with_limiter(|slot| slot.rc.load(Ordering::Relaxed)),
            Some(1)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn find_sees_slots_another_opener_added() {
        let path = CString::new("/ddlimiters-grown".to_string()).unwrap();
        let mut owner = ShmLimiterMemory::<AtomicU64>::create(path.clone()).unwrap();
        let near = owner
            .alloc_with(|hash| hash.store(42, Ordering::Relaxed))
            .unwrap();

        // A second opener maps the arena while it is still one page long, and keeps that
        // view - it never allocates, so nothing on its side ever extends it.
        let observer = ShmLimiterMemory::<AtomicU64>::open(&path).unwrap();
        let mapped_at_open = arena(&observer).mem.as_slice().len();

        // The owner grows it past that. Every handle is held, so nothing is recycled and the
        // slot below is reached from untouched tail space.
        let mut held = Vec::new();
        let far = loop {
            let limiter = owner.alloc().expect("the arena has room");
            if limiter.index() as usize >= mapped_at_open {
                break limiter;
            }
            held.push(limiter);
        };
        far.with_data(|hash| hash.store(1234, Ordering::Relaxed))
            .expect("the slot the owner just allocated is mapped");
        // (written after publication on purpose: this test is about scan extent, and the
        // owner is the only writer, so nothing else can observe the gap.)

        assert_eq!(
            observer
                .find(|hash| hash.load(Ordering::Relaxed) == 42)
                .map(|found| found.index()),
            Some(near.index())
        );
        assert_eq!(
            arena(&observer).mem.get_size(),
            mapped_at_open,
            "a match in the cached extent must not refresh the mapping"
        );

        assert_eq!(
            observer
                .find(|hash| hash.load(Ordering::Relaxed) == 1234)
                .map(|found| found.index()),
            Some(far.index()),
            "a scan must see a slot added beyond the extent this process mapped"
        );
    }

    /// A lookup that refuses the slot must leave its count exactly as it found it.
    ///
    /// `get` cannot know whether it will succeed until its compare-exchange lands, so the
    /// owning handle has to be built afterwards: `ShmLimiter`'s `Drop` always subtracts one,
    /// and a handle built for an acquisition that never happened releases a reference nobody
    /// took.
    ///
    /// Both consumers reach this with an ordinary index rather than a forged one. A
    /// live-debugging config can be retired between the worker publishing its slot index and
    /// a client opening it, and `find`'s own `get` races the idle-slot cleanup that retires
    /// what it just matched.
    ///
    /// Nothing recovers a count that has gone negative: `drop_if_rc_1`'s CAS(1, 0) can never
    /// match again, so the slot is pinned for the arena's lifetime - and a `get` that tests
    /// only for non-zero would increment -1 to 0 and hand out an owning handle to a slot
    /// sitting on the free list.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_refused_acquisition_leaves_the_refcount_alone() {
        fn rc(mem: &ShmLimiterMemory<()>, idx: u32) -> i32 {
            #[allow(clippy::unwrap_used)]
            arena(mem)
                .with_slot(idx, |slot| slot.rc.load(Ordering::SeqCst))
                .unwrap()
        }

        let path = CString::new("/ddlimiters-refused".to_string()).unwrap();
        let mut limiters = ShmLimiterMemory::<()>::create(path).unwrap();

        // Allocated and dropped in one statement: the count goes 1 -> 0 and the slot returns
        // to the free list, which is exactly the state a stale index names.
        let idx = limiters.alloc().unwrap().index();
        assert_eq!(rc(&limiters, idx), 0, "the only handle went away");

        assert!(
            limiters.get(idx).is_none(),
            "a retired slot must not be acquirable"
        );
        assert_eq!(
            rc(&limiters, idx),
            0,
            "a refused acquisition must not decrement a count it never acquired"
        );

        // A refusal that moved the count would let the next one through; it must stay refused
        // however many times it is asked for.
        for attempt in 0..3 {
            assert!(
                limiters.get(idx).is_none(),
                "attempt {attempt} must still be refused"
            );
            assert_eq!(rc(&limiters, idx), 0, "and still not have moved the count");
        }

        // The slot is still the allocator's to hand out, with one owner and no leftovers.
        let reused = limiters.alloc().unwrap();
        assert_eq!(
            reused.index(),
            idx,
            "a refused lookup must not have consumed the slot"
        );
        assert_eq!(
            rc(&limiters, idx),
            1,
            "the fresh allocation owns exactly one reference"
        );
    }

    /// A slot is findable as soon as it is allocated, not once it has been recycled.
    ///
    /// `next_free` is the free-list link and fresh mapped bytes are zero, so a slot taken
    /// from untouched tail space is live with a zero link. Requiring a non-zero link before
    /// evaluating the predicate conflates "on the free list" with "has ever been used", and
    /// hides precisely the slot a caller allocated a moment ago. `rc > 0` is the liveness
    /// test; the scan is bounded by the mapped extent, so the link is not a terminator
    /// either.
    ///
    /// The exception-hash consumer is the one that feels it: its acquire RPC does not dedupe,
    /// so a hash whose slot cannot be found allocates another slot on every occurrence and is
    /// never rate limited.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_freshly_allocated_slot_is_findable() {
        let path = CString::new("/ddlimiters-fresh".to_string()).unwrap();
        let mut limiters = ShmLimiterMemory::<AtomicU64>::create(path).unwrap();

        let limiter = limiters
            .alloc_with(|hash| hash.store(42, Ordering::Relaxed))
            .unwrap();

        assert_eq!(
            limiters
                .find(|hash| hash.load(Ordering::Relaxed) == 42)
                .map(|found| found.index()),
            Some(limiter.index()),
            "a slot allocated from untouched tail space must still be findable"
        );

        // Recycling is the case that always worked, because `actual_free` leaves a non-zero
        // link behind. It has to keep working.
        let idx = limiter.index();
        drop(limiter);
        let reused = limiters
            .alloc_with(|hash| hash.store(7, Ordering::Relaxed))
            .unwrap();
        assert_eq!(reused.index(), idx, "the free list hands the slot back");

        assert_eq!(
            limiters
                .find(|hash| hash.load(Ordering::Relaxed) == 7)
                .map(|found| found.index()),
            Some(idx),
            "and a recycled slot is findable under its new payload"
        );
        assert!(
            limiters
                .find(|hash| hash.load(Ordering::Relaxed) == 42)
                .is_none(),
            "while the payload it was retired with no longer matches"
        );
    }
}
