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
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zwohash::HashMap;

#[derive(Clone)]
#[cfg_attr(unix, derive(Hash, Eq, PartialEq))]
#[cfg_attr(windows, derive(Debug, serde::Serialize, serde::Deserialize))]
pub struct RemoteConfigNotifyTarget {
    #[cfg(unix)]
    pub pid: libc::pid_t,
    #[cfg(windows)]
    pub(crate) event: libdd_ipc::platform::PlatformHandle<std::os::windows::io::OwnedHandle>,
    #[cfg(windows)]
    // Stable across handle duplication and reconnects; raw HANDLE values are process-local.
    pub(crate) id: u128,
}

#[cfg(windows)]
impl PartialEq for RemoteConfigNotifyTarget {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[cfg(windows)]
impl Eq for RemoteConfigNotifyTarget {}

#[cfg(windows)]
impl std::hash::Hash for RemoteConfigNotifyTarget {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.id, state);
    }
}

#[cfg(windows)]
impl libdd_ipc::handles::TransferHandles for RemoteConfigNotifyTarget {
    fn copy_handles<T: libdd_ipc::handles::HandlesTransport>(
        &self,
        transport: T,
    ) -> Result<(), T::Error> {
        self.event.copy_handles(transport)
    }

    fn receive_handles<T: libdd_ipc::handles::HandlesTransport>(
        &mut self,
        transport: T,
    ) -> Result<(), T::Error> {
        self.event.receive_handles(transport)
    }
}

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
    fn notify(&self) {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            if winapi::um::synchapi::SetEvent(self.event.as_raw_handle().cast()) == 0 {
                tracing::warn!(
                    "Failed to signal remote config event: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
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
