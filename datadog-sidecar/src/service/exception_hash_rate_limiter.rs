// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::{MutexExt, rate_limiter::Limiter};
use libdd_ipc::rate_limiter::{ShmLimiter, ShmLimiterMemory};
use std::ffi::CString;
use std::io;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

pub(crate) static EXCEPTION_HASH_LIMITER: LazyLock<
    Option<Mutex<ManuallyDrop<ManagedExceptionHashRateLimiter>>>,
> = LazyLock::new(|| match ManagedExceptionHashRateLimiter::create() {
    Ok(limiter) => {
        unsafe { libc::atexit(drop_exception_hash_limiter) };
        Some(Mutex::new(ManuallyDrop::new(limiter)))
    }
    Err(e) => {
        tracing::error!(
            "Could not create the exception hash rate limiter: {e}. Continuing without rate limiting."
        );
        None
    }
});

extern "C" fn drop_exception_hash_limiter() {
    if let Some(limiter) = EXCEPTION_HASH_LIMITER.as_ref() {
        let mut guard = limiter.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: atexit runs once at program exit; no code accesses this static afterward.
        unsafe { ManuallyDrop::drop(&mut *guard) };
    }
}

pub(crate) struct ManagedExceptionHashRateLimiter {
    limiter: ExceptionHashRateLimiter,
    active: Vec<HashLimiter>,
    _drop: tokio::sync::oneshot::Sender<()>,
}

impl ManagedExceptionHashRateLimiter {
    fn create() -> io::Result<Self> {
        let limiter = ExceptionHashRateLimiter::create()?;
        let (send, recv) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            async fn do_loop() {
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    let Some(limiter) = EXCEPTION_HASH_LIMITER.as_ref() else {
                        return;
                    };
                    let mut this = limiter.lock_or_panic();
                    this.active.retain_mut(|limiter| {
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
            limiter,
            active: vec![],
            _drop: send,
        })
    }

    pub fn add(&mut self, hash: u64, granularity: Duration) {
        match self.limiter.add(hash, granularity) {
            Some(limiter) => self.active.push(limiter),
            None => tracing::warn!(
                "No exception hash rate limiter slot available for {hash:#x}; not rate limited"
            ),
        }
    }
}

#[derive(Clone)]
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
    #[allow(clippy::unwrap_used)]
    CString::new(format!("/ddexhlimit-{}", crate::shm_namespace())).unwrap()
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

    pub fn new_reader() -> Self {
        Self {
            mem: ShmLimiterMemory::new_reader(path()),
        }
    }

    fn add(&mut self, hash: u64, granularity: Duration) -> Option<HashLimiter> {
        let allocated = self
            .mem
            .alloc_with_granularity(granularity.as_secs() as u32, |data| {
                data.hash.store(hash, Ordering::Relaxed)
            })?;
        allocated.inc(1);
        Some(HashLimiter { shm: allocated })
    }

    pub fn find(&self, hash: u64) -> Option<HashLimiter> {
        Some(HashLimiter {
            shm: self
                .mem
                .find(|data| data.hash.load(Ordering::Relaxed) == hash)?,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)] // Spawns a process and uses OS shared memory.
    fn startup_replaces_arenas_before_any_acquisition() {
        const CHILD: &str = "DD_TEST_LIMITER_STARTUP_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Fresh statics and a private namespace, without changing the other tests.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "service::exception_hash_rate_limiter::tests::startup_replaces_arenas_before_any_acquisition",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }

        crate::use_thread_sidecar_shm_namespace(Some(std::process::id()));
        let mut old = ExceptionHashRateLimiter::create().unwrap();
        let old_slot = old.add(123, Duration::from_secs(60)).unwrap();
        let reader = ExceptionHashRateLimiter::new_reader();
        assert!(reader.find(123).is_some());
        let old_probe_arena =
            ShmLimiterMemory::<()>::create(crate::tracer::shm_limiter_path()).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        crate::tracer::init_shm_limiters();

        assert!(old_probe_arena.is_retired());
        assert!(old.mem.is_retired());
        assert!(reader.find(123).is_none());
        assert!(ExceptionHashRateLimiter::new_reader().find(123).is_none());
        assert_eq!(
            old_slot
                .shm
                .with_data(|data| data.hash.load(Ordering::Relaxed)),
            Some(123)
        );

        let mut current = EXCEPTION_HASH_LIMITER.as_ref().unwrap().lock().unwrap();
        assert!(current.active.is_empty());
        current.add(456, Duration::from_secs(60));
        assert!(reader.find(456).is_some());
    }
}
