// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::shm_remote_config::{ShmRemoteConfigs, ShmRemoteConfigsGuard};
use datadog_remote_config::fetch::{ConfigInvariants, MultiTargetStats, NotifyTarget};
use std::collections::hash_map::Entry;
use std::fmt::Debug;
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

#[derive(Clone, Hash, Eq, PartialEq)]
#[cfg_attr(windows, derive(Debug))]
pub struct RemoteConfigNotifyTarget {
    #[cfg(unix)]
    pub pid: libc::pid_t,
    #[cfg(windows)]
    pub process_handle: crate::service::sidecar_server::ProcessHandle,
    #[cfg(windows)]
    // contains address in that process address space of the notification function
    pub notify_function: RemoteConfigNotifyFunction,
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
    #[allow(clippy::missing_transmute_annotations)]
    fn notify(&self) {
        unsafe {
            let dummy = 0;
            winapi::um::processthreadsapi::CreateRemoteThread(
                self.process_handle.0,
                std::ptr::null_mut(),
                0,
                Some(std::mem::transmute(self.notify_function.0)),
                &dummy as *const i32 as winapi::shared::minwindef::LPVOID,
                0,
                std::ptr::null_mut(),
            );
        }
    }
}

#[derive(Default, Clone)]
pub struct RemoteConfigs(
    Arc<Mutex<HashMap<ConfigInvariants, ShmRemoteConfigs<RemoteConfigNotifyTarget>>>>,
);
pub type RemoteConfigsGuard = ShmRemoteConfigsGuard<RemoteConfigNotifyTarget>;

impl RemoteConfigs {
    pub fn add_runtime(
        &self,
        invariants: ConfigInvariants,
        poll_interval: Duration,
        runtime_id: String,
        notify_target: RemoteConfigNotifyTarget,
        env: String,
        service: String,
        app_version: String,
    ) -> RemoteConfigsGuard {
        match self.0.lock().unwrap().entry(invariants) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let this = self.0.clone();
                let invariants = e.key().clone();
                e.insert(ShmRemoteConfigs::new(
                    invariants.clone(),
                    Box::new(move || {
                        this.lock().unwrap().remove(&invariants);
                    }),
                    poll_interval,
                ))
            }
        }
        .add_runtime(runtime_id, notify_target, env, service, app_version)
    }

    pub fn shutdown(&self) {
        for (_, rc) in self.0.lock().unwrap().drain() {
            rc.shutdown();
        }
    }

    pub fn stats(&self) -> MultiTargetStats {
        self.0
            .lock()
            .unwrap()
            .values()
            .map(|rc| rc.stats())
            .fold(MultiTargetStats::default(), |a, b| a + b)
    }
}
