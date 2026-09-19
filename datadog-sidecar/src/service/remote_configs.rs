// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::{DynamicInstrumentationConfigState, InstanceId};
use crate::shm_remote_config::{ShmRemoteConfigs, ShmRemoteConfigsGuard};
use libdd_common::{tag::Tag, MutexExt};
use libdd_remote_config::fetch::{
    ConfigInvariants, ConfigOptions, MultiTargetStats, NotifyTarget, ProductCapabilities,
};
use std::collections::hash_map::Entry;
use std::fmt::Debug;
#[cfg(windows)]
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zwohash::HashMap;

#[cfg(windows)]
#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct RemoteConfigNotifyFunction(pub *mut libc::c_void);
#[cfg(windows)]
unsafe impl Send for RemoteConfigNotifyFunction {}
#[cfg(windows)]
unsafe impl Sync for RemoteConfigNotifyFunction {}
#[cfg(windows)]
impl Default for RemoteConfigNotifyFunction {
    fn default() -> Self {
        RemoteConfigNotifyFunction(std::ptr::null_mut())
    }
}

#[cfg(windows)]
impl serde::Serialize for RemoteConfigNotifyFunction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u64(self.0 as u64)
    }
}

#[cfg(windows)]
impl<'de> serde::Deserialize<'de> for RemoteConfigNotifyFunction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        <u64 as serde::Deserialize<'de>>::deserialize(deserializer)
            .map(|p| RemoteConfigNotifyFunction(p as *mut libc::c_void))
    }
}

#[cfg_attr(not(windows), derive(Clone, Hash, Eq, PartialEq))]
pub struct RemoteConfigNotifyTarget {
    #[cfg(unix)]
    pub pid: libc::pid_t,
    #[cfg(windows)]
    process_handle: crate::service::sidecar_server::ProcessHandle,
    #[cfg(windows)]
    // contains address in that process address space of the notification function
    notify_function: RemoteConfigNotifyFunction,
    #[cfg(windows)]
    active: Arc<Mutex<bool>>,
}

#[cfg(windows)]
impl RemoteConfigNotifyTarget {
    pub fn new(
        process_handle: crate::service::sidecar_server::ProcessHandle,
        notify_function: RemoteConfigNotifyFunction,
    ) -> Self {
        Self {
            process_handle,
            notify_function,
            active: Arc::new(Mutex::new(true)),
        }
    }
}

#[cfg(windows)]
impl Clone for RemoteConfigNotifyTarget {
    fn clone(&self) -> Self {
        Self {
            process_handle: self.process_handle,
            notify_function: self.notify_function,
            active: self.active.clone(),
        }
    }
}

#[cfg(windows)]
impl Debug for RemoteConfigNotifyTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteConfigNotifyTarget")
            .field("process_handle", &self.process_handle)
            .field("notify_function", &self.notify_function)
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
impl Hash for RemoteConfigNotifyTarget {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.process_handle.hash(state);
        self.notify_function.hash(state);
    }
}

#[cfg(windows)]
impl PartialEq for RemoteConfigNotifyTarget {
    fn eq(&self, other: &Self) -> bool {
        self.process_handle == other.process_handle && self.notify_function == other.notify_function
    }
}

#[cfg(windows)]
impl Eq for RemoteConfigNotifyTarget {}

#[cfg(unix)]
impl Debug for RemoteConfigNotifyTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.pid.fmt(f)
    }
}

impl NotifyTarget for RemoteConfigNotifyTarget {
    #[cfg(not(windows))]
    fn notify(&self) {
        unsafe { libc::kill(self.pid, libc::SIGVTALRM) };
    }

    #[cfg(windows)]
    #[allow(clippy::missing_transmute_annotations)]
    fn notify(&self) {
        let active = self.active.lock_or_panic();
        if !*active {
            return;
        }

        unsafe {
            let thread = winapi::um::processthreadsapi::CreateRemoteThread(
                self.process_handle.0,
                std::ptr::null_mut(),
                0,
                Some(std::mem::transmute(self.notify_function.0)),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
            );
            if !thread.is_null() {
                winapi::um::synchapi::WaitForSingleObject(thread, winapi::um::winbase::INFINITE);
                winapi::um::handleapi::CloseHandle(thread);
            }
        }
    }

    #[cfg(windows)]
    fn deactivate(&self) {
        *self.active.lock_or_panic() = false;
    }
}

#[derive(Default, Clone)]
pub struct RemoteConfigs(
    Arc<Mutex<HashMap<ConfigInvariants, ShmRemoteConfigs<RemoteConfigNotifyTarget>>>>,
);
pub type RemoteConfigsGuard = ShmRemoteConfigsGuard<RemoteConfigNotifyTarget>;

impl RemoteConfigs {
    #[allow(clippy::too_many_arguments)]
    pub fn add_runtime(
        &self,
        options: ConfigOptions,
        poll_interval: Duration,
        instance_id: InstanceId,
        remote_config_generation: u64,
        notify_target: RemoteConfigNotifyTarget,
        env: String,
        service: String,
        app_version: String,
        tags: Vec<Tag>,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
        process_tags: Vec<Tag>,
    ) -> RemoteConfigsGuard {
        match self.0.lock_or_panic().entry(options.invariants) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let this = self.0.clone();
                let invariants = e.key().clone();
                e.insert(ShmRemoteConfigs::new(
                    invariants.clone(),
                    Box::new(move || {
                        // try_lock: if the lock is held _right now_, it means that an insertion is
                        // in progress. In that case we can just ignore the Err() and leave it.
                        // Otherwise we have to check whether it's actually really dead and possibly
                        // re-insert.

                        if let Ok(mut unlocked) = this.try_lock() {
                            if let Some(active) = unlocked.remove(&invariants) {
                                if !active.is_dead() {
                                    unlocked.insert(invariants.clone(), active);
                                }
                            }
                        }
                    }),
                    poll_interval,
                ))
            }
        }
        .add_runtime(
            instance_id,
            remote_config_generation,
            notify_target,
            env,
            service,
            app_version,
            tags,
            ProductCapabilities {
                products: options.products,
                capabilities: options.capabilities,
            },
            dynamic_instrumentation_state,
            process_tags,
        )
    }

    pub fn shutdown(&self) {
        for (_, rc) in self.0.lock_or_panic().drain() {
            rc.shutdown();
        }
    }

    pub fn stats(&self) -> MultiTargetStats {
        self.0
            .lock_or_panic()
            .values()
            .map(|rc| rc.stats())
            .fold(MultiTargetStats::default(), |a, b| a + b)
    }
}
