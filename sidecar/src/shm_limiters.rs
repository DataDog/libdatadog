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

pub trait Limiter {
    /// Takes the limit per interval.
    /// Returns false if the limit is exceeded, otherwise true.
    fn inc(&self, limit: u32) -> bool;
    /// Returns the effective rate per interval.
    fn rate(&self) -> f64;
}

/// A thread-safe limiter built on Atomics.
/// It's base unit is in seconds, i.e. the minimum allowed rate is 1 per second.
/// Internally the limiter works with the system time granularity, i.e. nanoseconds on unix and
/// milliseconds on windows.
/// The implementation is a sliding window: every time the limiter is increased, the as much time as
/// has passed is also refilled.
#[repr(C)]
pub struct LocalLimiter {
    hit_count: AtomicI64,
    last_update: AtomicU64,
    last_limit: AtomicU32,
    granularity: i64,
}

/// Returns nanoseconds on Unix, milliseconds on Windows (system time granularity is bad there).
#[cfg(windows)]
const TIME_PER_SECOND: i64 = 1_000; // milliseconds
#[cfg(not(windows))]
const TIME_PER_SECOND: i64 = 1_000_000_000; // nanoseconds

impl Default for LocalLimiter {
    fn default() -> Self {
        LocalLimiter {
            hit_count: Default::default(),
            last_update: Default::default(),
            last_limit: Default::default(),
            granularity: TIME_PER_SECOND,
        }
    }
}

impl LocalLimiter {
    /// Allows setting a custom time granularity. The default() implementation is 1 second.
    pub fn with_granularity(seconds: u32) -> LocalLimiter {
        let mut limiter = LocalLimiter::default();
        limiter.granularity *= seconds as i64;
        limiter
    }
}

fn now() -> u64 {
    #[cfg(windows)]
    let now = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
    #[cfg(not(windows))]
    let now = std::time::Duration::from(
        nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC).unwrap(),
    )
    .as_nanos() as u64;
    now
}

impl Limiter for LocalLimiter {
    fn inc(&self, limit: u32) -> bool {
        let now = now();
        let last = self.last_update.swap(now, Ordering::SeqCst);
        // Make sure reducing the limit doesn't stall for a long time
        let clear_limit = limit.max(self.last_limit.load(Ordering::Relaxed));
        let clear_counter = (now as i64 - last as i64) * (clear_limit as i64);
        let subtract = clear_counter - self.granularity;
        let mut previous_hits = self.hit_count.fetch_sub(subtract, Ordering::SeqCst);
        // Handle where the limiter goes below zero
        if previous_hits < subtract {
            let add = clear_counter - previous_hits.max(0);
            self.hit_count.fetch_add(add, Ordering::Acquire);
            previous_hits += add - clear_counter;
        }
        if previous_hits / self.granularity >= limit as i64 {
            self.hit_count
                .fetch_sub(self.granularity, Ordering::Acquire);
            false
        } else {
            // We don't care about race conditions here:
            // If the last limit was high enough to increase the previous_hits, we are anyway close
            // to a number realistic to decrease the count quickly; i.e. we won't stall the limiter
            // indefinitely when switching from a high to a low limit.
            self.last_limit.store(limit, Ordering::Relaxed);
            true
        }
    }

    fn rate(&self) -> f64 {
        let last_limit = self.last_limit.load(Ordering::Relaxed) as f64;
        let hit_count = self.hit_count.load(Ordering::Relaxed) as f64;
        (last_limit / hit_count * self.granularity as f64).clamp(0., 1.)
    }
}

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
        limiter.limiter.last_update.store(now(), Ordering::Relaxed);
        limiter.rc.store(1, Ordering::Relaxed);
        unsafe {
            // SAFETY: we initialize the struct here
            (*(limiter as *const _ as *mut ShmLimiterData))
                .limiter
                .granularity = TIME_PER_SECOND;
        }
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
    use crate::shm_limiters::{Limiter, ShmLimiterData, ShmLimiterMemory, TIME_PER_SECOND};
    use std::sync::atomic::Ordering;

    #[test]
    #[cfg_attr(miri, ignore)]
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
            .limiter
            .last_update
            .fetch_sub(3 * TIME_PER_SECOND as u64, Ordering::Relaxed);
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
