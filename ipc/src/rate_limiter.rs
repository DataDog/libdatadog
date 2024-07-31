// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use ddcommon::rate_limiter::{Limiter, LocalLimiter};
use std::ffi::CString;
use std::fmt::{Debug, Formatter};
use std::io;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

#[repr(C)]
#[derive(Default)]
struct ShmLimiterData<'a> {
    next_free: AtomicU32, // free list
    rc: AtomicI32,
    limiter: LocalLimiter,
    _phantom: PhantomData<&'a ShmLimiterMemory>,
}

#[derive(Clone)]
pub struct ShmLimiterMemory(Arc<RwLock<MappedMem<NamedShmHandle>>>);

impl ShmLimiterMemory {
    const START_OFFSET: u32 = std::mem::align_of::<ShmLimiterData>() as u32;

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
        Self(Arc::new(RwLock::new(handle)))
    }

    /// The start of the ShmLimiter memory has 4 bytes indicating an offset to the first free
    /// element in the free list. It is zero if there is no element on the free list.
    fn first_free_ref(&self) -> &AtomicU32 {
        unsafe { &*self.0.read().unwrap().as_slice().as_ptr().cast() }
    }

    fn next_free(&mut self) -> u32 {
        let mut first_free = self.first_free_ref().load(Ordering::Relaxed);
        loop {
            let mut target_next_free = ShmLimiter {
                idx: first_free,
                memory: self.clone(),
            }
            .limiter()
            .next_free
            .load(Ordering::Relaxed);
            // Not yet used memory will always be 0. The next free entry will then be just above.
            if target_next_free == 0 {
                target_next_free = first_free + std::mem::size_of::<ShmLimiterData>() as u32;
                // target_next_free is the end of the current entry - but we need one more
                self.0.write().unwrap().ensure_space(
                    target_next_free as usize + std::mem::size_of::<ShmLimiterData>(),
                );
            }
            match self.first_free_ref().compare_exchange(
                first_free,
                target_next_free,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return first_free,
                Err(found) => first_free = found,
            }
        }
    }

    pub fn alloc(&mut self) -> ShmLimiter {
        let reference = ShmLimiter {
            idx: self.next_free(),
            memory: self.clone(),
        };
        let limiter = reference.limiter();
        limiter.rc.store(1, Ordering::Relaxed);
        // SAFETY: we initialize the struct here
        unsafe {
            (*(limiter as *const _ as *mut ShmLimiterData))
                .limiter
                .reset(1)
        };
        reference
    }

    pub fn get(&self, idx: u32) -> Option<ShmLimiter> {
        assert_eq!(
            idx % std::mem::size_of::<ShmLimiterData>() as u32,
            Self::START_OFFSET
        );
        let reference = ShmLimiter {
            idx,
            memory: self.clone(),
        };
        let limiter = reference.limiter();
        let mut rc = limiter.rc.load(Ordering::Relaxed);
        loop {
            if rc == 0 {
                return None;
            }
            match limiter
                .rc
                .compare_exchange(rc, rc + 1, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => return Some(reference),
                Err(found) => rc = found,
            }
        }
    }
}

pub struct ShmLimiter {
    idx: u32,
    memory: ShmLimiterMemory,
}

impl Debug for ShmLimiter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.idx.fmt(f)
    }
}

impl ShmLimiter {
    fn limiter(&self) -> &ShmLimiterData {
        unsafe {
            &*self
                .memory
                .0
                .read()
                .unwrap()
                .as_slice()
                .as_ptr()
                .offset(self.idx as isize)
                .cast()
        }
    }

    pub fn index(&self) -> u32 {
        self.idx
    }
}

impl Limiter for ShmLimiter {
    fn inc(&self, limit: u32) -> bool {
        self.limiter().limiter.inc(limit)
    }

    fn rate(&self) -> f64 {
        self.limiter().limiter.rate()
    }
}

impl Drop for ShmLimiter {
    fn drop(&mut self) {
        let limiter = self.limiter();
        if limiter.rc.fetch_sub(1, Ordering::SeqCst) == 1 {
            let next_free_ref = self.memory.first_free_ref();
            let mut next_free = next_free_ref.load(Ordering::Relaxed);
            loop {
                limiter.next_free.store(next_free, Ordering::Relaxed);
                match next_free_ref.compare_exchange(
                    next_free,
                    self.idx,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(found) => next_free = found,
                }
            }
        }
    }
}

pub enum AnyLimiter {
    Local(LocalLimiter),
    Shm(ShmLimiter),
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
}

#[cfg(test)]
mod tests {
    use crate::rate_limiter::{ShmLimiterData, ShmLimiterMemory};
    use ddcommon::rate_limiter::Limiter;
    use std::ffi::CString;

    fn path() -> CString {
        CString::new("/ddlimiters-test".to_string()).unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_limiters() {
        let mut limiters = ShmLimiterMemory::create(path()).unwrap();
        let limiter = limiters.alloc();
        let limiter_idx = limiter.idx;
        // Two are allowed, then one more because a small amount of time passed since the first one
        assert!(limiter.inc(2));
        assert!(limiter.inc(2));
        assert!(limiter.inc(2));
        assert!(!limiter.inc(2));
        assert!(!limiter.inc(2));

        // Now test the free list
        let limiter2 = limiters.alloc();
        assert_eq!(
            limiter2.idx,
            limiter_idx + std::mem::size_of::<ShmLimiterData>() as u32
        );
        drop(limiter);

        let limiter = limiters.alloc();
        assert_eq!(limiter.idx, limiter_idx);

        let limiter3 = limiters.alloc();
        assert_eq!(
            limiter3.idx,
            limiter2.idx + std::mem::size_of::<ShmLimiterData>() as u32
        );
    }
}
