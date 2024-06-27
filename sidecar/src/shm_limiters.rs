// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use std::ffi::CString;
use std::fmt::{Debug, Formatter};
use std::io;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub trait Limiter {
    fn inc(&self, limit: u32) -> bool;
}

#[repr(C)]
#[derive(Default)]
struct LimiterData<T> {
    next_free: AtomicU32, // free list
    rc: AtomicI32,
    hit_count: AtomicI64,
    last_update: AtomicU64,
    _phantom: PhantomData<T>,
}

type ShmLimiterData<'a> = LimiterData<&'a ShmLimiterMemory>;

impl<T> LimiterData<T> {
    /// Returns nanosecons on Unix, milliseconds on Windows.
    fn now() -> u64 {
        #[cfg(windows)]
        let now = windows_sys::Win32::System::SystemInformation::GetTickCount64();
        #[cfg(not(windows))]
        let now =
            Duration::from(nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC).unwrap())
                .as_nanos() as u64;
        now
    }

    #[cfg(windows)]
    const TIME_PER_SEC: i64 = 1_000; // milliseconds
    #[cfg(not(windows))]
    const TIME_PER_SEC: i64 = 1_000_000_000; // nanoseconds

    pub fn inc(&self, limit: u32) -> bool {
        let now = Self::now();
        let last = self.last_update.swap(now, Ordering::SeqCst);
        let clear_counter = (now as i64 - last as i64) * (limit as i64);
        let mut previous_hits = self
            .hit_count
            .fetch_sub(clear_counter - Self::TIME_PER_SEC, Ordering::SeqCst);
        if previous_hits < clear_counter - Self::TIME_PER_SEC {
            let add = clear_counter - previous_hits.max(0);
            self.hit_count.fetch_add(add, Ordering::Acquire);
            previous_hits += add - clear_counter;
        }
        if previous_hits / Self::TIME_PER_SEC >= limit as i64 {
            self.hit_count
                .fetch_sub(Self::TIME_PER_SEC, Ordering::Acquire);
            false
        } else {
            true
        }
    }
}

#[derive(Clone)]
pub struct ShmLimiterMemory(Arc<RwLock<MappedMem<NamedShmHandle>>>);

impl ShmLimiterMemory {
    fn path() -> CString {
        CString::new(format!("/ddlimiters-{}", primary_sidecar_identifier())).unwrap()
    }

    const START_OFFSET: u32 = std::mem::align_of::<ShmLimiterData>() as u32;

    pub fn create() -> io::Result<Self> {
        let path = Self::path();
        // Clean leftover shm
        unsafe { libc::unlink(path.as_ptr()) };
        let mem = Self::new(NamedShmHandle::create(path, 0x1000)?.map()?);
        mem.first_free_ref()
            .store(Self::START_OFFSET, Ordering::Relaxed);
        Ok(mem)
    }

    /// Opens the shared limiter. Users are expected to re-open this if their sidecar connection
    /// breaks.
    pub fn open() -> io::Result<Self> {
        Ok(Self::new(NamedShmHandle::open(&Self::path())?.map()?))
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
        limiter
            .last_update
            .store(ShmLimiterData::now(), Ordering::Relaxed);
        limiter.rc.store(1, Ordering::Relaxed);
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
        self.limiter().inc(limit)
    }
}

#[derive(Default)]
pub struct LocalLimiter(LimiterData<()>);

impl Limiter for LocalLimiter {
    fn inc(&self, limit: u32) -> bool {
        self.0.inc(limit)
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

impl Limiter for AnyLimiter {
    fn inc(&self, limit: u32) -> bool {
        match self {
            AnyLimiter::Local(local) => local as &dyn Limiter,
            AnyLimiter::Shm(shm) => shm as &dyn Limiter,
        }
        .inc(limit)
    }
}

#[cfg(test)]
mod tests {
    use crate::shm_limiters::{Limiter, ShmLimiterData, ShmLimiterMemory};
    use std::sync::atomic::Ordering;

    #[test]
    fn test_limiters() {
        let mut limiters = ShmLimiterMemory::create().unwrap();
        let limiter = limiters.alloc();
        let limiter_idx = limiter.idx;
        // Two are allowed, then one more because a small amount of time passed since the first one
        assert!(limiter.inc(2));
        assert!(limiter.inc(2));
        assert!(limiter.inc(2));
        assert!(!limiter.inc(2));
        assert!(!limiter.inc(2));

        // reduce 4 times, we're going into negative territory. Next increment will reset to zero.
        limiter
            .limiter()
            .last_update
            .fetch_sub(3 * ShmLimiterData::TIME_PER_SEC as u64, Ordering::Relaxed);
        assert!(limiter.inc(2));
        assert!(limiter.inc(2));
        assert!(limiter.inc(2));
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
