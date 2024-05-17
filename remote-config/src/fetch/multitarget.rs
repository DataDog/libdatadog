// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::default::Default;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use futures_util::future::Shared;
use futures_util::FutureExt;
use manual_future::ManualFuture;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tracing::{debug, error, trace};
use crate::fetch::{ConfigFetcherState, ConfigInvariants, FileStorage, RefcountedFile, RefcountingStorage, SharedFetcher};
use crate::Target;

/// MultiTargetFetcher built on a set of SharedFetchers, managing multiple environments and services
/// at once.
/// It is able to keep track of all Target tuples as well as runtime_ids currently active.
/// The implementation chooses an arbitrary runtime id from the set of runtimes which have just a
/// single associated Target. If there is no such runtime id, it uses a synthetic runtime id.
/// This fetcher is designed for use cases with more than one Target tuple associated to a
/// specific runtime id and/or handling hundreds to thousands of different runtime ids with a low
/// amount of actual remote config clients.
pub struct MultiTargetFetcher<N: NotifyTarget, S: FileStorage + Clone + Sync + Send> where S::StoredFile: RefcountedFile + Sync + Send, S: MultiTargetHandlers<S::StoredFile> {
    /// All runtime ids belonging to a specific target
    target_runtimes: Mutex<HashMap<Arc<Target>, HashSet<String>>>,
    /// Keyed by runtime_id
    runtimes: Mutex<HashMap<String, RuntimeInfo<N>>>,
    pub remote_config_interval: AtomicU32,
    /// All services by target in use
    services: Mutex<HashMap<Arc<Target>, KnownTarget>>,
    pending_async_insertions: AtomicU32,
    storage: RefcountingStorage<S>,
    /// Limit on how many fetchers can be active at once.
    /// This functionality is mostly targeted at CLI programs which generally have their file name
    /// as the service name. E.g. a phpt testsuite will generate one service for every single file.
    /// The remote config backend can only handle a certain amount of services at once.
    fetcher_semaphore: Semaphore,
}

enum KnownTargetStatus {
    Pending,
    Alive,
    RemoveAt(Instant),
    Removing(Shared<ManualFuture<()>>),
}

struct KnownTarget {
    refcount: u32,
    status: Arc<Mutex<KnownTargetStatus>>,
    synthetic_id: bool,
    runtimes: HashSet<String>,
    fetcher: Arc<SharedFetcher>,
}

impl Drop for KnownTarget {
    fn drop(&mut self) {
        self.fetcher.cancel();
    }
}

pub trait NotifyTarget: Sync + Send + Sized + Hash + Eq + Clone + Debug {
    fn notify(&self);
}

pub trait MultiTargetHandlers<S> {
    fn fetched(&self, target: &Arc<Target>, files: &[Arc<S>]) -> (Option<String>, bool);

    fn expired(&self, target: &Arc<Target>);

    fn dead(&self);
}

struct RuntimeInfo<N: NotifyTarget> {
    notify_target: N,
    targets: HashMap<Arc<Target>, u32>,
}

impl<N: NotifyTarget + 'static, S: FileStorage + Clone + Sync + Send + 'static> MultiTargetFetcher<N, S> where S::StoredFile: RefcountedFile + Sync + Send, S: MultiTargetHandlers<S::StoredFile> {
    pub const DEFAULT_CLIENTS_LIMIT: u32 = 100;
    
    pub fn new(storage: S, invariants: ConfigInvariants) -> Arc<Self> {
        Arc::new(MultiTargetFetcher {
            storage: RefcountingStorage::new(storage, ConfigFetcherState::new(invariants)),
            target_runtimes: Mutex::new(Default::default()),
            runtimes: Mutex::new(Default::default()),
            remote_config_interval: AtomicU32::new(5000),
            services: Mutex::new(Default::default()),
            pending_async_insertions: AtomicU32::new(0),
            fetcher_semaphore: Semaphore::new(Self::DEFAULT_CLIENTS_LIMIT as usize),
        })
    }

    pub fn is_dead(&self) -> bool {
        self.services.lock().unwrap().is_empty() && self.pending_async_insertions.load(Ordering::Relaxed) == 0
    }

    /// Allow for more than DEFAULT_CLIENTS_LIMIT fetchers running simultaneously
    pub fn increase_clients_limit(&self, increase: u32) {
        self.fetcher_semaphore.add_permits(increase as usize);
    }

    fn generate_synthetic_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn remove_target(self: &Arc<Self>, runtime_id: &str, target: &Arc<Target>) {
        let mut services = self.services.lock().unwrap();
        // "goto" like handling to drop the known_service borrow and be able to change services
        'service_handling: {
            'drop_service: {
                let known_service = services.get_mut(target).unwrap();
                known_service.refcount = if known_service.refcount == 1 {
                    known_service.runtimes.remove(runtime_id);
                    let mut status = known_service.status.lock().unwrap();
                    *status = match *status {
                        KnownTargetStatus::Pending => break 'drop_service,
                        KnownTargetStatus::Alive => {
                            KnownTargetStatus::RemoveAt(Instant::now() + Duration::from_secs(3666))
                        },
                        KnownTargetStatus::RemoveAt(_) | KnownTargetStatus::Removing(_) => unreachable!(),
                    };
                    0
                } else {
                    if *known_service.fetcher.runtime_id.lock().unwrap() == runtime_id {
                        'changed_rt_id: {
                            for (id, runtime) in self.runtimes.lock().unwrap().iter() {
                                if runtime.targets.len() == 1 && runtime.targets.contains_key(target) {
                                    *known_service.fetcher.runtime_id.lock().unwrap() = id.to_string();
                                    break 'changed_rt_id;
                                }
                            }
                            known_service.synthetic_id = true;
                            *known_service.fetcher.runtime_id.lock().unwrap() = Self::generate_synthetic_id();
                        }
                    }
                    known_service.refcount - 1
                };
                break 'service_handling;
            }
            services.remove(target);
        }

        let mut target_runtimes = self.target_runtimes.lock().unwrap();
        if if let Some(target_runtime) = target_runtimes.get_mut(target) {
            target_runtime.remove(runtime_id);
            target_runtime.is_empty()
        } else {
            false
        } {
            target_runtimes.remove(target);
        }
    }

    fn add_target(self: &Arc<Self>, synthetic_id: bool, runtime_id: &str, target: Arc<Target>) {
        let mut target_runtimes = self.target_runtimes.lock().unwrap();
        match target_runtimes.entry(target.clone()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(HashSet::new()),
        }.insert(runtime_id.to_string());
        drop(target_runtimes); // unlock

        let mut services = self.services.lock().unwrap();
        match services.entry(target.clone()) {
            Entry::Occupied(mut e) => {
                let known_target = &mut e.get_mut();
                if known_target.refcount == 0 {
                    let mut status = known_target.status.lock().unwrap();
                    match *status {
                        KnownTargetStatus::RemoveAt(_) => {
                            *status = KnownTargetStatus::Alive;
                            known_target.refcount = 1;
                            if synthetic_id && !known_target.synthetic_id {
                                known_target.synthetic_id = true;
                                *known_target.fetcher.runtime_id.lock().unwrap() = Self::generate_synthetic_id();
                            }
                            known_target.runtimes.insert(runtime_id.to_string());
                        },
                        KnownTargetStatus::Removing(ref future) => {
                            let future = future.clone();
                            // Avoid deadlocking between known_target.status and self.services
                            self.pending_async_insertions.fetch_add(1, Ordering::AcqRel);
                            let runtime_id = runtime_id.to_string();
                            let this = self.clone();
                            tokio::spawn(async move {
                                future.await;
                                this.add_target(synthetic_id, runtime_id.as_str(), target);
                                this.pending_async_insertions.fetch_sub(1, Ordering::AcqRel);
                            });
                            return;
                        },
                        KnownTargetStatus::Alive | KnownTargetStatus::Pending => unreachable!(),
                    }
                } else {
                    known_target.refcount += 1;
                }
                if !synthetic_id && known_target.synthetic_id {
                    known_target.synthetic_id = false;
                    *known_target.fetcher.runtime_id.lock().unwrap() = runtime_id.into();
                }
            },
            Entry::Vacant(e) => {
                let runtime_id = if synthetic_id { Self::generate_synthetic_id() } else { runtime_id.into() };
                self.start_fetcher(e.insert(KnownTarget {
                    refcount: 1,
                    status: Arc::new(Mutex::new(KnownTargetStatus::Pending)),
                    synthetic_id,
                    runtimes: {
                        let mut set = HashSet::default();
                        set.insert(runtime_id.to_string());
                        set
                    },
                    fetcher: Arc::new(SharedFetcher::new(target, runtime_id)),
                }));
            }
        }
    }

    fn requires_synthetic_id(info: &RuntimeInfo<N>) -> bool {
        info.targets.len() > 1
    }

    pub fn add_runtime(
        self: &Arc<Self>,
        runtime_id: String,
        notify_target: N,
        target: &Arc<Target>,
    ) {
        trace!("Adding remote config runtime: {target:?} with runtime id {runtime_id}");
        match self.runtimes.lock().unwrap().entry(runtime_id) {
            Entry::Occupied(mut runtime_entry) => {
                let info = runtime_entry.get_mut();
                match info.targets.entry(target.clone()) {
                    Entry::Occupied(mut e) => *e.get_mut() += 1,
                    Entry::Vacant(e) => {
                        e.insert(1);
                        self.add_target(Self::requires_synthetic_id(info), runtime_entry.key(), target.clone());
                    },
                }
            }
            Entry::Vacant(e) => {
                if self.storage.invariants().endpoint.url.scheme().map(|s| s.as_str() != "file") == Some(true) {
                    let info = RuntimeInfo {
                        notify_target,
                        targets: HashMap::from([(target.clone(), 1)]),
                    };
                    self.add_target(Self::requires_synthetic_id(&info), e.key(), target.clone());
                    e.insert(info);
                }
            }
        }
    }

    pub fn delete_runtime(
        self: &Arc<Self>,
        runtime_id: &str,
        target: &Arc<Target>,
    ) {
        trace!("Removing remote config runtime: {target:?} with runtime id {runtime_id}");
        {
            let mut runtimes = self.runtimes.lock().unwrap();
            let last_removed = {
                let info = match runtimes.get_mut(runtime_id) {
                    None => return,
                    Some(i) => i,
                };
                match info.targets.entry(target.clone()) {
                    Entry::Occupied(mut e) => {
                        if *e.get() == 1 {
                            e.remove();
                        } else {
                            *e.get_mut() -= 1;
                            return;
                        }
                    }
                    Entry::Vacant(_) => unreachable!("Missing target runtime"),
                }
                info.targets.is_empty()
            };
            if last_removed {
                runtimes.remove(runtime_id);
            }
        }
        Self::remove_target(self, runtime_id, target);
    }

    fn start_fetcher(self: &Arc<Self>, known_target: &mut KnownTarget) {
        let this = self.clone();
        let fetcher = known_target.fetcher.clone();
        let status = known_target.status.clone();
        tokio::spawn(async move {
            // Relatively primitive, no prioritization or anything. It is not expected that this
            // semaphore is ever awaiting under standard usage. Can be improved if needed, e.g.
            // sorted by amount of targets on the outstanding services or similar.
            let _semaphore = this.fetcher_semaphore.acquire().await.unwrap();
            {
                let mut status = status.lock().unwrap();
                if !matches!(*status, KnownTargetStatus::Pending) {
                    return;
                }
                *status = KnownTargetStatus::Alive;
            }

            let (remove_future, remove_completer) = ManualFuture::new();
            let shared_future = remove_future.shared();

            let inner_fetcher = fetcher.clone();
            let inner_this = this.clone();
            fetcher.run(this.storage.clone(), Box::new(move |files| {
                {
                    let mut status = status.lock().unwrap();
                    if let KnownTargetStatus::RemoveAt(instant) = *status {
                        // Voluntarily give up the semaphore for services in RemoveAt status if
                        // there are only few available permits
                        if inner_this.fetcher_semaphore.available_permits() < 10 || instant < Instant::now() {
                            // We need to signal that we're in progress of removing to avoid race conditions
                            *status = KnownTargetStatus::Removing(shared_future.clone());
                            // break here to drop mutex guard and avoid having status and services locked simultaneously
                            inner_fetcher.cancel();
                            return None;
                        }
                    }
                } // unlock status

                let (error, notify) = inner_this.storage.storage.fetched(&inner_fetcher.target, files);

                if notify {
                    // notify_targets is Hash + Eq + Clone, allowing us to deduplicate. Also avoid the lock during notifying
                    let mut notify_targets = HashSet::new();
                    if let Some(runtimes) = inner_this.target_runtimes.lock().unwrap().get(&inner_fetcher.target) {
                        for runtime_id in runtimes {
                            if let Some(runtime) = inner_this.runtimes.lock().unwrap().get(runtime_id) {
                                notify_targets.insert(runtime.notify_target.clone());
                            }
                        }
                    }

                    debug!("Notify {:?} about remote config changes", notify_targets);
                    for notify_target in notify_targets {
                        notify_target.notify();
                    }
                }

                error
            })).await;

            this.storage.storage.expired(&fetcher.target);

            { // scope lock before await
                let mut services = this.services.lock().unwrap();
                services.remove(&fetcher.target);
                if services.is_empty() && this.pending_async_insertions.load(Ordering::Relaxed) == 0 {
                    this.storage.storage.dead();
                }
            }
            remove_completer.complete(()).await;
        });
    }

    pub fn shutdown(&self) {
        let services = self.services.lock().unwrap();
        for (target, service) in services.iter() {
            let mut status = service.status.lock().unwrap();
            match *status {
                KnownTargetStatus::Pending | KnownTargetStatus::Alive => {
                    error!("Trying to shutdown {:?} while still alive", target);
                },
                KnownTargetStatus::RemoveAt(_) => {
                    *status = KnownTargetStatus::RemoveAt(Instant::now());
                    service.fetcher.cancel();
                },
                KnownTargetStatus::Removing(_) => {},
            }
        }
    }
}
