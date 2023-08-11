use std::collections::hash_map::Entry;
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, Mutex};
use zwohash::HashMap;
use datadog_remote_config::fetch::{ConfigInvariants, NotifyTarget};
use crate::shm_remote_config::{ShmRemoteConfigs, ShmRemoteConfigsGuard};

#[derive(Default, Clone, Hash, Eq, PartialEq)]
pub struct RemoteConfigNotifyTarget {
    pub pid: libc::pid_t,
    #[cfg(windows)]
    // contains address in that process address space of the notification function
    pub notify_function: libc::c_void,
}

impl Debug for RemoteConfigNotifyTarget {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
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
        // TODO: CreateRemoteThread -> ddtrace_set_all_thread_vm_interrupt
    }
}

#[derive(Default, Clone)]
pub struct RemoteConfigs(Arc<Mutex<HashMap<ConfigInvariants, ShmRemoteConfigs<RemoteConfigNotifyTarget>>>>);
pub type RemoteConfigsGuard = ShmRemoteConfigsGuard<RemoteConfigNotifyTarget>;

impl RemoteConfigs {
    pub fn add_runtime(
        &self,
        invariants: ConfigInvariants,
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
                e.insert(ShmRemoteConfigs::new(invariants.clone(), Box::new(move || {
                    this.lock().unwrap().remove(&invariants);
                })))
            }
        }.add_runtime(runtime_id, notify_target, env, service, app_version)
    }

    pub fn shutdown(&self) {
        for (_, rc) in self.0.lock().unwrap().drain() {
            rc.shutdown();
        }
    }
}
