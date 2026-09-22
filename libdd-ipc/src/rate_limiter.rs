// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use libdd_common::rate_limiter::{Limiter, LocalLimiter};
use std::cell::UnsafeCell;
use std::ffi::CString;
use std::fmt::{Debug, Formatter};
use std::io;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;

#[repr(C)]
#[derive(Default)]
struct ShmLimiterData<'a, Inner> {
    next_free: AtomicU32, // free list
    rc: AtomicI32,
    limiter: LocalLimiter,
    inner: UnsafeCell<Inner>,
    _phantom: PhantomData<&'a ShmLimiterMemory<Inner>>,
}

pub struct ShmLimiterMemory<Inner> {
    mem: Arc<MappedMem<NamedShmHandle>>,
    _phantom: PhantomData<Inner>,
}

impl<Inner> Clone for ShmLimiterMemory<Inner> {
    fn clone(&self) -> Self {
        ShmLimiterMemory {
            mem: self.mem.clone(),
            _phantom: Default::default(),
        }
    }
}

impl<Inner> ShmLimiterMemory<Inner> {
    const START_OFFSET: u32 = align_of::<ShmLimiterData<Inner>>() as u32;
    const STRIDE: u32 = size_of::<ShmLimiterData<Inner>>() as u32;

    pub fn create(path: CString) -> io::Result<Self> {
        // Clean leftover shm
        unsafe { libc::unlink(path.as_ptr()) };
        let mem = Self::new(NamedShmHandle::create(path, 0x1000)?.map()?);
        mem.first_free_ref()
            .store(Self::START_OFFSET, Ordering::Relaxed);
        Ok(mem)
    }

    /// Opens the shared limiter. Users are expected to re-open this if their sidecar connection
    /// breaks.
    pub fn open(path: &CString) -> io::Result<Self> {
        Ok(Self::new(NamedShmHandle::open(path)?.map()?))
    }

    fn new(handle: MappedMem<NamedShmHandle>) -> Self {
        Self {
            mem: Arc::new(handle),
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
    fn slot(slice: &[u8], idx: u32) -> Option<&ShmLimiterData<Inner>> {
        // Slots start at START_OFFSET - the type's alignment - and sit back to back, so
        // anything off that grid would also be misaligned for the atomics inside.
        if idx < Self::START_OFFSET || (idx - Self::START_OFFSET) % Self::STRIDE != 0 {
            return None;
        }
        if idx.checked_add(Self::STRIDE)? as usize > slice.len() {
            return None;
        }
        // SAFETY: the slot lies wholly within the mapping and is correctly aligned, both
        // checked just above.
        Some(unsafe { &*slice.as_ptr().add(idx as usize).cast() })
    }

    /// The bytes currently backed by the segment.
    fn with_mapping<R>(&self, f: impl FnOnce(&[u8]) -> Option<R>) -> Option<R> {
        f(self.mem.as_slice())
    }

    /// Never extends the mapping. Scans rely on this refusing the first slot past the end in
    /// order to terminate, and no access other than an explicit lookup should enlarge
    /// anything.
    fn with_slot<R>(&self, idx: u32, f: impl FnOnce(&ShmLimiterData<Inner>) -> R) -> Option<R> {
        self.with_mapping(|slice| Self::slot(slice, idx).map(f))
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
        if self.with_mapping(|slice| Some(slice.len()))? < end {
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

    fn next_free(&mut self) -> Option<u32> {
        let mut first_free = self.first_free_ref().load(Ordering::Relaxed);
        loop {
            let mut target_next_free =
                match self.with_slot(first_free, |l| l.next_free.load(Ordering::Relaxed)) {
                    Some(next) => next,
                    None => {
                        self.ensure_mapped(first_free.checked_add(Self::STRIDE)? as usize)?;
                        self.with_slot(first_free, |l| l.next_free.load(Ordering::Relaxed))?
                    }
                };
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

    pub fn alloc(&mut self) -> Option<ShmLimiter<Inner>> {
        self.alloc_with_granularity(1)
    }

    /// Returns `None` when no memory left.
    pub fn alloc_with_granularity(&mut self, seconds: u32) -> Option<ShmLimiter<Inner>> {
        let reference = ShmLimiter {
            idx: self.next_free()?,
            memory: self.clone(),
        };
        reference.with_limiter(|limiter| {
            // Initialize the limiter before publishing it through the reference count.
            // SAFETY: this entry is not visible while its reference count is zero.
            unsafe {
                (*(limiter as *const _ as *mut ShmLimiterData<Inner>))
                    .limiter
                    // The seconds come from an RPC argument, and zero is a divisor.
                    .reset(seconds.max(1))
            };
            limiter.rc.store(1, Ordering::Release);
        })?;
        Some(reference)
    }

    pub fn get(&self, idx: u32) -> Option<ShmLimiter<Inner>> {
        let reference = ShmLimiter {
            idx,
            memory: self.clone(),
        };
        self.with_slot_extending(idx, |limiter| {
            let mut rc = limiter.rc.load(Ordering::Acquire);
            loop {
                // A peer can write this count. Zero means retired; a value that cannot be
                // incremented is not a count we are willing to join.
                let Some(next) = rc.checked_add(1).filter(|_| rc != 0) else {
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
        })?
        .then_some(reference)
    }

    pub fn find<F>(&self, cond: F) -> Option<ShmLimiter<Inner>>
    where
        F: Fn(&Inner) -> bool,
    {
        // Snapshot the extent once and stop there. Terminating on the slot accessor's
        // refusal instead would let a miss extend the mapping slot by slot, and a `next_free`
        // sentinel read out of the segment is a peer-writable stop condition.
        let limit = u32::try_from(self.with_mapping(|slice| Some(slice.len()))?).ok()?;
        let mut cur = Self::START_OFFSET;
        while cur
            .checked_add(Self::STRIDE)
            .is_some_and(|end| end <= limit)
        {
            let hit = self.with_slot(cur, |data| {
                data.next_free.load(Ordering::Relaxed) != 0
                    && data.rc.load(Ordering::Relaxed) > 0
                    && cond(unsafe { &*data.inner.get() })
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
        None
    }
}

pub struct ShmLimiter<Inner> {
    idx: u32,
    memory: ShmLimiterMemory<Inner>,
}

impl<Inner> Debug for ShmLimiter<Inner> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.idx.fmt(f)
    }
}

impl<Inner> ShmLimiter<Inner> {
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
        self.memory.with_mapping(|slice| {
            let header = ShmLimiterMemory::<Inner>::header(slice)?;
            let limiter = ShmLimiterMemory::<Inner>::slot(slice, self.idx)?;
            let mut next_free = header.load(Ordering::Relaxed);
            loop {
                // Whatever ends up in the link is bounds-checked before it is ever followed.
                limiter.next_free.store(next_free, Ordering::Relaxed);
                match header.compare_exchange(
                    next_free,
                    self.idx,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Some(()),
                    Err(found) => next_free = found,
                }
            }
        });
    }
}

impl<Inner> Limiter for ShmLimiter<Inner> {
    fn inc(&self, limit: u32) -> bool {
        self.with_limiter(|l| l.limiter.inc(limit)).unwrap_or(false)
    }

    fn rate(&self) -> f64 {
        self.with_limiter(|l| l.limiter.rate()).unwrap_or(0.)
    }

    fn update_rate(&self) -> f64 {
        self.with_limiter(|l| l.limiter.update_rate()).unwrap_or(0.)
    }
}

impl<Inner> Drop for ShmLimiter<Inner> {
    fn drop(&mut self) {
        if self.idx == 0 {
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

pub enum AnyLimiter {
    Local(LocalLimiter),
    Shm(ShmLimiter<()>),
}

impl AnyLimiter {
    fn limiter(&self) -> &dyn Limiter {
        match self {
            AnyLimiter::Local(local) => local as &dyn Limiter,
            AnyLimiter::Shm(shm) => shm as &dyn Limiter,
        }
    }
}

impl Limiter for AnyLimiter {
    fn inc(&self, limit: u32) -> bool {
        self.limiter().inc(limit)
    }

    fn rate(&self) -> f64 {
        self.limiter().rate()
    }

    fn update_rate(&self) -> f64 {
        self.limiter().update_rate()
    }
}

#[cfg(test)]
mod tests {
    use crate::rate_limiter::{ShmLimiterData, ShmLimiterMemory};
    use libdd_common::rate_limiter::Limiter;
    use std::ffi::CString;
    use std::sync::atomic::Ordering;
    use std::thread::sleep;
    use std::time::Duration;

    fn path() -> CString {
        CString::new("/ddlimiters-test".to_string()).unwrap()
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
        limiters.first_free_ref().store(8192, Ordering::Relaxed);

        // The offset may be honoured - it is the segment's own accounting, and the worker owns
        // that - but only ever by extending the mapping to cover it. What must not happen is
        // an access landing outside.
        let stride = size_of::<ShmLimiterData<()>>();
        if let Some(limiter) = limiters.alloc() {
            let mapped = limiters.mem.as_slice().len();
            assert!(
                limiter.idx as usize + stride <= mapped,
                "slot at {} escapes the {mapped}-byte mapping",
                limiter.idx
            );
            assert!(limiter.inc(2), "and it must be a working limiter");
        }

        // Far past the reservation: nothing to honour, and nothing allocated either.
        limiters
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

        let before = limiters.with_mapping(|slice| Some(slice.len())).unwrap();
        assert!(limiters.find(|_| false).is_none(), "nothing to find");
        let after = limiters.with_mapping(|slice| Some(slice.len())).unwrap();

        assert_eq!(
            before, after,
            "a miss must leave the mapping the size it was"
        );
    }

    /// The divisor and the reference count both live in peer-writable memory, so both can
    /// hold values that make arithmetic on them panic in whichever process does the limiting.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn malformed_limiter_values_do_not_panic() {
        let path = CString::new("/ddlimiters-malformed".to_string()).unwrap();
        let mut limiters = ShmLimiterMemory::<()>::create(path).unwrap();

        // Zero arrives straight from an RPC argument, and `inc` divides by it.
        let limiter = limiters.alloc_with_granularity(0).unwrap();
        let _ = limiter.inc(1);

        // A count a peer pushed to the edge cannot be joined - but must not overflow saying so.
        limiter.with_limiter(|l| l.rc.store(i32::MAX, Ordering::Relaxed));
        assert!(
            limiters.get(limiter.index()).is_none(),
            "a refcount that cannot be incremented must be refused, not wrapped"
        );
    }

    /// An index that never came from the allocator is refused rather than dereferenced - and
    /// not asserted on either, since it arrives from a peer.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn indices_outside_the_mapping_are_refused() {
        let path = CString::new("/ddlimiters-bounds".to_string()).unwrap();
        let limiters = ShmLimiterMemory::<()>::create(path).unwrap();
        let start = ShmLimiterMemory::<()>::START_OFFSET;
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
        // Two are allowed, then one more because a small amount of time passed since the first one
        assert!(limiter.inc(2));
        // Add a minimal amount of time to ensure the test doesn't run faster than timer precision
        sleep(Duration::from_micros(100));
        assert!(limiter.inc(2));
        sleep(Duration::from_micros(100));
        assert!(limiter.inc(2));
        sleep(Duration::from_micros(100));
        assert!(!limiter.inc(2));
        sleep(Duration::from_micros(100));
        assert!(!limiter.inc(2));

        // Now test the free list
        let limiter2 = limiters.alloc().unwrap();
        assert_eq!(
            limiter2.idx,
            limiter_idx + size_of::<ShmLimiterData<()>>() as u32
        );
        drop(limiter);

        let limiter = limiters.alloc().unwrap();
        assert_eq!(limiter.idx, limiter_idx);

        let limiter3 = limiters.alloc().unwrap();
        assert_eq!(
            limiter3.idx,
            limiter2.idx + size_of::<ShmLimiterData<()>>() as u32
        );
    }
}
