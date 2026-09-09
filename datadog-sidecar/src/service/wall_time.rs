// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::MutexExt;
use libdd_ipc::platform::{FileBackedHandle, MappedMem, ShmHandle};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::mem::{align_of, size_of};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::debug;

#[repr(C)]
pub struct WallTimeShmRegion {
    pub pid: libc::pid_t,
    pub wall_sample_pending: u32,
    pub config_reread_pending: u32,
}

const _: () = assert!(size_of::<WallTimeShmRegion>() == 12);
const _: () = assert!(align_of::<WallTimeShmRegion>() == 4);

struct Registration {
    mapping: MappedMem<ShmHandle>,
    owner: u64,
}

impl Registration {
    fn region(&self) -> &WallTimeShmRegion {
        // The mapping was size/alignment checked during registration and remains mapped.
        unsafe { &*self.mapping.as_slice().as_ptr().cast::<WallTimeShmRegion>() }
    }

    fn wall_pending(&self) -> &AtomicU32 {
        unsafe { &*std::ptr::addr_of!(self.region().wall_sample_pending).cast() }
    }

    fn config_pending(&self) -> &AtomicU32 {
        unsafe { &*std::ptr::addr_of!(self.region().config_reread_pending).cast() }
    }

    fn mark_wall_pending(&self) -> bool {
        self.wall_pending()
            .compare_exchange(0, 1, Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }
}

#[derive(Default, Clone)]
pub struct WallTimeRegistrations {
    registrations: Arc<Mutex<HashMap<libc::pid_t, Arc<Registration>>>>,
    scheduler_started: Arc<AtomicBool>,
}

impl PartialEq for WallTimeRegistrations {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.registrations, &other.registrations)
    }
}

impl Eq for WallTimeRegistrations {}

impl Hash for WallTimeRegistrations {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.registrations).hash(state);
    }
}

impl WallTimeRegistrations {
    pub fn register(&self, handle: ShmHandle, owner: u64) -> std::io::Result<libc::pid_t> {
        let mapping = handle.map()?;
        if mapping.get_size() < size_of::<WallTimeShmRegion>() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "wall-time profiling shared region is too small",
            ));
        }
        let ptr = mapping.as_slice().as_ptr();
        if ptr.align_offset(align_of::<WallTimeShmRegion>()) != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "wall-time profiling shared region is misaligned",
            ));
        }
        // The mapping covers the complete region and is properly aligned.
        let region = unsafe { &*ptr.cast::<WallTimeShmRegion>() };
        if region.pid <= 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid wall-time profiling pid",
            ));
        }

        let pid = region.pid;
        let (replaced, count) = {
            let mut registrations = self.registrations.lock_or_panic();
            let replaced = registrations
                .insert(pid, Arc::new(Registration { mapping, owner }))
                .is_some();
            (replaced, registrations.len())
        };
        debug!(
            "wall-time profiler registered pid={pid} owner={owner} replaced={replaced} workers={count}"
        );
        self.start_scheduler();
        Ok(pid)
    }

    pub fn unregister(&self, pid: libc::pid_t) {
        self.registrations.lock_or_panic().remove(&pid);
    }

    pub fn unregister_owned(&self, pid: libc::pid_t, owner: u64) {
        let mut registrations = self.registrations.lock_or_panic();
        let remaining = if registrations
            .get(&pid)
            .is_some_and(|registration| registration.owner == owner)
        {
            registrations.remove(&pid);
            Some(registrations.len())
        } else {
            None
        };
        drop(registrations);
        if let Some(remaining) = remaining {
            debug!("wall-time profiler unregistered pid={pid} owner={owner} workers={remaining}");
        }
    }

    pub fn notify_remote_config(&self, pid: libc::pid_t) {
        let registration = self.registrations.lock_or_panic().get(&pid).cloned();
        if let Some(registration) = registration {
            registration.config_pending().store(1, Ordering::Release);
        }
    }

    fn start_scheduler(&self) {
        if self
            .scheduler_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let registrations = self.registrations.clone();
        let scheduler_started = self.scheduler_started.clone();
        tokio::spawn(async move {
            loop {
                let count = registrations.lock_or_panic().len();
                if count == 0 {
                    scheduler_started.store(false, Ordering::Release);
                    if registrations.lock_or_panic().is_empty() {
                        return;
                    }
                    // A registration raced with the transition to idle. Either retain
                    // ownership of scheduling or let the newly spawned scheduler do it.
                    if scheduler_started
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                let multiplier = u32::try_from(count.max(1)).unwrap_or(u32::MAX);
                tokio::time::sleep(Duration::from_millis(10).saturating_mul(multiplier)).await;
                let snapshot: Vec<(libc::pid_t, Arc<Registration>)> = registrations
                    .lock_or_panic()
                    .iter()
                    .map(|(&pid, registration)| (pid, registration.clone()))
                    .collect();
                for (pid, registration) in snapshot {
                    if registration.mark_wall_pending() {
                        // The release publication of wall_sample_pending precedes notification.
                        let result = unsafe { libc::kill(pid, libc::SIGVTALRM) };
                        if result != 0 {
                            let mut registrations = registrations.lock_or_panic();
                            if registrations
                                .get(&pid)
                                .is_some_and(|current| Arc::ptr_eq(current, &registration))
                            {
                                registrations.remove(&pid);
                            }
                        }
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(pid: libc::pid_t) -> WallTimeShmRegion {
        WallTimeShmRegion {
            pid,
            wall_sample_pending: 0,
            config_reread_pending: 0,
        }
    }

    fn mapped_region(value: WallTimeShmRegion) -> MappedMem<ShmHandle> {
        let handle = ShmHandle::new(size_of::<WallTimeShmRegion>()).expect("create shared memory");
        let mut mapping = handle.map().expect("map shared memory");
        // ShmHandle mappings are page-aligned and large enough for the fixed ABI region.
        unsafe {
            mapping
                .as_slice_mut()
                .as_mut_ptr()
                .cast::<WallTimeShmRegion>()
                .write(value);
        }
        mapping
    }

    #[test]
    fn pending_bit_applies_backpressure_until_consumed() {
        let registration = Registration {
            mapping: mapped_region(region(123)),
            owner: 1,
        };

        assert!(registration.mark_wall_pending());
        assert!(!registration.mark_wall_pending());
        assert_eq!(registration.wall_pending().swap(0, Ordering::Acquire), 1);
        assert!(registration.mark_wall_pending());
    }

    #[tokio::test]
    async fn stale_connection_cleanup_does_not_remove_replayed_registration() {
        let registrations = WallTimeRegistrations::default();
        let first = mapped_region(region(123)).into();
        let second = mapped_region(region(123)).into();

        registrations
            .register(first, 1)
            .expect("first registration");
        registrations
            .register(second, 2)
            .expect("replacement registration");
        registrations.unregister_owned(123, 1);
        assert_eq!(registrations.registrations.lock_or_panic().len(), 1);

        registrations.unregister_owned(123, 2);
        assert!(registrations.registrations.lock_or_panic().is_empty());
    }

    #[tokio::test]
    async fn rejects_invalid_pid() {
        let registrations = WallTimeRegistrations::default();

        let error = registrations
            .register(mapped_region(region(0)).into(), 1)
            .expect_err("invalid pids must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
