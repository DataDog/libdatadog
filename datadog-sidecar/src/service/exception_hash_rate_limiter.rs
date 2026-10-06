// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::MutexExt;
use libdd_ipc::rate_limiter::{ShmLimiter, ShmLimiterMemory};
use std::ffi::CString;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) struct ManagedExceptionHashRateLimiter {
    limiter: ExceptionHashRateLimiter,
    active: Vec<HashLimiter>,
}

impl ManagedExceptionHashRateLimiter {
    pub(crate) fn create() -> io::Result<Arc<Mutex<Self>>> {
        let limiter = Arc::new(Mutex::new(Self {
            limiter: ExceptionHashRateLimiter::create()?,
            active: vec![],
        }));
        let weak = Arc::downgrade(&limiter);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let Some(limiter) = weak.upgrade() else {
                    return;
                };
                limiter.lock_or_panic().active.retain_mut(|limiter| {
                    limiter.shm.is_active() || !unsafe { limiter.shm.drop_if_rc_1() }
                });
            }
        });
        Ok(limiter)
    }

    pub(crate) fn unlink(&self) {
        self.limiter.mem.unlink();
    }

    pub fn add(&mut self, hash: u64, granularity: Duration) {
        if granularity.is_zero() {
            return;
        }
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
    granularity: Duration,
}

impl HashLimiter {
    pub fn inc(&self) -> bool {
        self.shm.inc(1, self.granularity)
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

    pub const fn new_reader() -> Self {
        Self {
            mem: ShmLimiterMemory::new_reader_with_path(path),
        }
    }

    pub fn reconnect(&self) {
        self.mem.reconnect();
    }

    fn add(&mut self, hash: u64, granularity: Duration) -> Option<HashLimiter> {
        let allocated = self
            .mem
            .alloc_with(|data| data.hash.store(hash, Ordering::Relaxed))?;
        let limiter = HashLimiter {
            shm: allocated,
            granularity,
        };
        limiter.inc();
        Some(limiter)
    }

    pub fn find(&self, hash: u64, granularity: Duration) -> Option<HashLimiter> {
        Some(HashLimiter {
            shm: self
                .mem
                .find(|data| data.hash.load(Ordering::Relaxed) == hash)?,
            granularity,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::service::InstanceId;
    use crate::service::telemetry::{
        InternalTelemetryActions, get_telemetry_action_sender, init_telemetry_sender,
    };

    fn telemetry_action() -> InternalTelemetryActions {
        InternalTelemetryActions {
            instance_id: InstanceId::new("session", "runtime"),
            service_name: "fork-test".into(),
            env_name: "test".into(),
            actions: vec![],
        }
    }

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
        assert!(reader.find(123, Duration::from_secs(60)).is_some());
        let old_probe_arena =
            ShmLimiterMemory::<()>::create(crate::tracer::shm_limiter_path()).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let server = crate::service::sidecar_server::SidecarServer::default();
        server.shm_limiters();

        assert!(old_probe_arena.is_retired());
        assert!(old.mem.is_retired());
        assert!(reader.find(123, Duration::from_secs(60)).is_none());
        assert!(
            ExceptionHashRateLimiter::new_reader()
                .find(123, Duration::from_secs(60))
                .is_none()
        );
        assert_eq!(
            old_slot
                .shm
                .with_data(|data| data.hash.load(Ordering::Relaxed)),
            Some(123)
        );

        let mut current = server
            .shm_limiters()
            .exceptions
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        assert!(current.active.is_empty());
        current.add(456, Duration::from_secs(60));
        assert!(reader.find(456, Duration::from_secs(60)).is_some());

        let (_telemetry, mut telemetry_rx) = init_telemetry_sender();
        let probes = server
            .shm_limiters()
            .probes
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        // Both inherited locks remain held while the child starts its own sidecar.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                unsafe { crate::setup::MasterListener::clear_inherited_state() }.unwrap();
                crate::use_thread_sidecar_shm_namespace(Some(std::process::id()));
                assert!(get_telemetry_action_sender().is_err());
                let (_telemetry, mut telemetry_rx) = init_telemetry_sender();
                get_telemetry_action_sender()
                    .unwrap()
                    .try_send(telemetry_action())
                    .unwrap();
                assert!(telemetry_rx.try_recv().is_ok());
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _entered = runtime.enter();
                let child = crate::service::sidecar_server::SidecarServer::default();
                let limiters = child.shm_limiters();
                let mut child_probes = limiters.probes.as_ref().unwrap().lock().unwrap();
                let slot = child_probes.alloc().unwrap();
                assert!(
                    ShmLimiterMemory::<()>::open(&crate::tracer::shm_limiter_path())
                        .unwrap()
                        .get(slot.index())
                        .is_some()
                );
                let mut exceptions = limiters.exceptions.as_ref().unwrap().lock().unwrap();
                assert!(exceptions.active.is_empty());
                exceptions.add(789, Duration::from_secs(60));
                let child_reader = ExceptionHashRateLimiter::new_reader();
                assert!(child_reader.find(789, Duration::from_secs(60)).is_some());
                assert!(child_reader.find(456, Duration::from_secs(60)).is_none());
                assert!(!probes.is_retired());
                assert!(reader.find(456, Duration::from_secs(60)).is_some());
                child_probes.unlink();
                exceptions.unlink();
            }));
            unsafe { libc::_exit(if result.is_ok() { 0 } else { 1 }) };
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut status = 0;
        while unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == 0 {
            if std::time::Instant::now() >= deadline {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, &mut status, 0);
                }
                panic!("child blocked on inherited limiter state");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        assert!(
            ExceptionHashRateLimiter::new_reader()
                .find(456, Duration::from_secs(60))
                .is_some()
        );
        get_telemetry_action_sender()
            .unwrap()
            .try_send(telemetry_action())
            .unwrap();
        assert!(telemetry_rx.try_recv().is_ok());
    }
}
