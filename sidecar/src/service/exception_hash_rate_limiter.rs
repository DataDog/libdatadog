// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use datadog_ipc::rate_limiter::{ShmLimiter, ShmLimiterMemory};
use ddcommon::rate_limiter::Limiter;
use std::ffi::CString;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static EXCEPTION_HASH_LIMITER: OnceLock<Mutex<ManagedExceptionHashRateLimiter>> = OnceLock::new();

pub(crate) fn get_exception_hash_limiter() -> &'static Mutex<ManagedExceptionHashRateLimiter> {
    EXCEPTION_HASH_LIMITER
        .get_or_init(|| Mutex::new(ManagedExceptionHashRateLimiter::create().unwrap()))
}

pub(crate) struct ManagedExceptionHashRateLimiter {
    limiter: ExceptionHashRateLimiter,
    active: Vec<HashLimiter>,
    _drop: tokio::sync::oneshot::Sender<()>,
}

impl ManagedExceptionHashRateLimiter {
    fn create() -> io::Result<Self> {
        let (send, recv) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            async fn do_loop() {
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    let mut this = get_exception_hash_limiter().lock().unwrap();
                    this.active.retain_mut(|limiter| {
                        // This technically could discard
                        limiter.shm.update_rate() > 0. || !unsafe { limiter.shm.drop_if_rc_1() }
                    });
                }
            }

            tokio::select! {
                _ = do_loop() => {}
                _ = recv => { }
            }
        });

        Ok(ManagedExceptionHashRateLimiter {
            limiter: ExceptionHashRateLimiter::create()?,
            active: vec![],
            _drop: send,
        })
    }

    pub fn add(&mut self, hash: u64, granularity: Duration) {
        let limiter = self.limiter.add(hash, granularity);
        self.active.push(limiter);
    }
}

pub struct ExceptionHashRateLimiter {
    mem: ShmLimiterMemory<EntryData>,
}

struct EntryData {
    pub hash: AtomicU64,
}

pub struct HashLimiter {
    shm: ShmLimiter<EntryData>,
}

impl HashLimiter {
    pub fn inc(&self) -> bool {
        self.shm.inc(1)
    }
}

fn path() -> CString {
    CString::new(format!("/ddexhlimit-{}", primary_sidecar_identifier())).unwrap()
}

impl ExceptionHashRateLimiter {
    pub fn create() -> io::Result<Self> {
        Ok(ExceptionHashRateLimiter {
            mem: ShmLimiterMemory::create(path())?,
        })
    }

    pub fn open() -> io::Result<Self> {
        Ok(ExceptionHashRateLimiter {
            mem: ShmLimiterMemory::open(&path())?,
        })
    }

    fn add(&mut self, hash: u64, granularity: Duration) -> HashLimiter {
        let allocated = self
            .mem
            .alloc_with_granularity(granularity.as_secs() as u32);
        let data = allocated.data();
        data.hash.store(hash, Ordering::Relaxed);
        allocated.inc(1);
        HashLimiter { shm: allocated }
    }

    pub fn find(&self, hash: u64) -> Option<HashLimiter> {
        Some(HashLimiter {
            shm: self
                .mem
                .find(|data| data.hash.load(Ordering::Relaxed) == hash)?,
        })
    }
}
