// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::{
    ConfigApplyState, ConfigFetcherState, ConfigInvariants, ConfigProductCapabilities, FileStorage,
    RefcountedFile, RefcountingStorage, RefcountingStorageStats, SharedFetcher, random_uuid_string,
};
use crate::{RemoteConfigCapabilities, RemoteConfigProduct, Target};
use futures_util::FutureExt;
use futures_util::future::Shared;
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::MutexExt;
use manual_future::ManualFuture;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::default::Default;
use std::fmt::Debug;
use std::hash::Hash;
use std::ops::Add;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::select;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tracing::{debug, trace};

/// MultiTargetFetcher built on a set of SharedFetchers, managing multiple environments and services
/// at once.
/// It is able to keep track of all Target tuples as well as runtime_ids currently active.
/// The implementation chooses an arbitrary runtime id from the set of runtimes which have just a
/// single associated Target. If there is no such runtime id, it uses a synthetic runtime id.
/// This fetcher is designed for use cases with more than one Target tuple associated to a
/// specific runtime id and/or handling hundreds to thousands of different runtime ids with a low
/// amount of actual remote config clients.
pub struct MultiTargetFetcher<
    N: NotifyTarget,
    S: FileStorage + Clone + Sync + Send,
    C: HttpClientCapability + SleepCapability + Send + Sync,
> where
    S::StoredFile: RefcountedFile + Sync + Send,
    S: MultiTargetHandlers<N, S, C>,
{
    /// Keyed by runtime_id
    runtimes: Mutex<HashMap<String, RuntimeInfo<N>>>,
    /// Refetch interval in nanoseconds.
    pub remote_config_interval: AtomicU64,
    /// All services by target in use
    /// WARNING: do NOT lock runtimes while holding a lock to services!
    services: Mutex<HashMap<Arc<Target>, KnownTarget>>,
    pending_async_insertions: AtomicU32,
    pub storage: RefcountingStorage<S, C>,
    /// Limit on how many fetchers can be active at once.
    /// This functionality is mostly targeted at CLI programs which generally have their file name
    /// as the service name. E.g. a phpt testsuite will generate one service for every single file.
    /// The remote config backend can only handle a certain amount of services at once.
    fetcher_semaphore: Semaphore,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MultiTargetStats {
    known_runtimes: u32,
    starting_fetchers: u32,
    active_fetchers: u32,
    inactive_fetchers: u32,
    removing_fetchers: u32,
    storage: RefcountingStorageStats,
}

impl Add for MultiTargetStats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        MultiTargetStats {
            known_runtimes: self.known_runtimes + rhs.known_runtimes,
            starting_fetchers: self.starting_fetchers + rhs.starting_fetchers,
            active_fetchers: self.active_fetchers + rhs.active_fetchers,
            inactive_fetchers: self.inactive_fetchers + rhs.inactive_fetchers,
            removing_fetchers: self.removing_fetchers + rhs.removing_fetchers,
            storage: self.storage + rhs.storage,
        }
    }
}

enum KnownTargetStatus {
    Pending,
    Alive,
    RemoveAt(Instant),
    Removing(Shared<ManualFuture<()>>),
}

pub struct ProductCapabilities {
    pub products: Vec<RemoteConfigProduct>,
    pub capabilities: Vec<RemoteConfigCapabilities>,
}

struct KnownTarget {
    refcount: u32,
    status: Arc<Mutex<KnownTargetStatus>>,
    synthetic_id: bool,
    /// All runtime ids belonging to a specific target, with their capabilities
    runtimes: HashMap<String, ProductCapabilities>,
    fetcher: Arc<SharedFetcher>,
    active_products: HashMap<RemoteConfigProduct, u32>,
    active_capabilities: HashMap<RemoteConfigCapabilities, u32>,
}

fn addref_hashmap<K: Hash + Eq>(map: &mut HashMap<K, u32>, key: K) -> bool {
    match map.entry(key) {
        Entry::Occupied(mut e) => {
            *e.get_mut() += 1;
            false
        }
        Entry::Vacant(e) => {
            e.insert(1);
            true
        }
    }
}

fn decref_hashmap<K: Hash + Eq>(map: &mut HashMap<K, u32>, key: K) -> bool {
    if let Some(entry) = map.get_mut(&key) {
        if *entry == 1 {
            map.remove(&key);
            return true;
        } else {
            *entry -= 1;
        }
    }
    false
}

impl KnownTarget {
    fn update_product_capabilities(&mut self) {
        if self.refcount > 0 {
            *self.fetcher.product_capabilities.lock_or_panic() =
                Arc::new(ConfigProductCapabilities::new(
                    self.active_products.keys().cloned().collect(),
                    self.active_capabilities.keys().cloned().collect(),
                ));
        }
    }
}

impl Drop for KnownTarget {
    fn drop(&mut self) {
        self.fetcher.cancel();
    }
}

pub trait NotifyTarget: Sync + Send + Sized + Hash + Eq + Clone + Debug {
    fn notify(&self);
}

pub trait MultiTargetHandlers<
    N: NotifyTarget,
    S: FileStorage + Clone + Sync + Send + MultiTargetHandlers<N, S, C>,
    C: HttpClientCapability + SleepCapability + Send + Sync,
> where
    S::StoredFile: RefcountedFile + Sync + Send,
{
    fn fetched(
        &self,
        fetcher: &Arc<MultiTargetFetcher<N, S, C>>,
        runtime_id: &Arc<String>,
        target: &Arc<Target>,
        files: &[Arc<S::StoredFile>],
    ) -> bool;

    fn expired(&self, target: &Arc<Target>);

    /// Called when a fetcher has no active runtimes.
    /// Beware: This function may be called at any time, e.g. another thread might be attempting to
    /// call add_runtime() right when no other runtime was left. Be careful with the locking here.
    /// Will not be called multiple times, unless this specific case was encountered.
    fn dead(&self);
}

struct RuntimeInfo<N: NotifyTarget> {
    notify_target: N,
    session_id: String,
    targets: HashMap<Arc<Target>, u32>,
}

impl<
    N: NotifyTarget + 'static,
    S: FileStorage + Clone + Sync + Send + 'static,
    C: HttpClientCapability + SleepCapability + Send + Sync + 'static,
> MultiTargetFetcher<N, S, C>
where
    S::StoredFile: RefcountedFile + Sync + Send,
    S: MultiTargetHandlers<N, S, C>,
{
    pub const DEFAULT_CLIENTS_LIMIT: u32 = 100;

    pub fn new(storage: S, invariants: ConfigInvariants, http_client: C) -> Arc<Self> {
        Arc::new(MultiTargetFetcher {
            storage: RefcountingStorage::new(
                storage,
                ConfigFetcherState::with_client(invariants, http_client),
            ),
            runtimes: Mutex::new(Default::default()),
            remote_config_interval: AtomicU64::new(5_000_000_000),
            services: Mutex::new(Default::default()),
            pending_async_insertions: AtomicU32::new(0),
            fetcher_semaphore: Semaphore::new(Self::DEFAULT_CLIENTS_LIMIT as usize),
        })
    }

    pub fn is_dead(&self) -> bool {
        self.services.lock_or_panic().is_empty()
            && self.pending_async_insertions.load(Ordering::Relaxed) == 0
    }

    /// Allow for more than DEFAULT_CLIENTS_LIMIT fetchers running simultaneously
    pub fn increase_clients_limit(&self, increase: u32) {
        self.fetcher_semaphore.add_permits(increase as usize);
    }

    fn generate_synthetic_id() -> String {
        random_uuid_string()
    }

    fn remove_target(
        self: &Arc<Self>,
        instance_runtime_id: &str,
        session_id: &str,
        target: &Arc<Target>,
        runtimes: MutexGuard<HashMap<String, RuntimeInfo<N>>>,
    ) {
        let mut services = self.services.lock_or_panic();
        // "goto" like handling to drop the known_service borrow and be able to change services
        'service_handling: {
            'drop_service: {
                if let Some(known_service) = services.get_mut(target) {
                    known_service.refcount = match known_service.refcount {
                        0 => {
                            // Handle the possible race condition where the service gets added AND
                            // removed while in Removing state.
                            let status = known_service.status.lock_or_panic();
                            match *status {
                                KnownTargetStatus::Removing(ref future) => {
                                    let future = future.clone();
                                    let instance_runtime_id = instance_runtime_id.to_string();
                                    let session_id = session_id.to_string();
                                    let this = self.clone();
                                    let target = target.clone();
                                    tokio::spawn(async move {
                                        future.await;
                                        let runtimes = this.runtimes.lock_or_panic();
                                        this.remove_target(
                                            instance_runtime_id.as_str(),
                                            session_id.as_str(),
                                            &target,
                                            runtimes,
                                        );
                                    });
                                    return;
                                }
                                _ => {
                                    // It's already in process of being removed
                                    return;
                                }
                            }
                        }
                        1 => {
                            // Drop the runtime's share, keeping what is forced: a revival adds
                            // the products of its runtime back.
                            if let Some(product_data) =
                                known_service.runtimes.remove(instance_runtime_id)
                            {
                                for product in product_data.products {
                                    decref_hashmap(&mut known_service.active_products, product);
                                }
                                for capability in product_data.capabilities {
                                    decref_hashmap(
                                        &mut known_service.active_capabilities,
                                        capability,
                                    );
                                }
                            }
                            let mut status = known_service.status.lock_or_panic();
                            *status = match *status {
                                KnownTargetStatus::Pending => KnownTargetStatus::Alive, /* not really */
                                KnownTargetStatus::Alive => KnownTargetStatus::RemoveAt(
                                    Instant::now() + Duration::from_secs(3666),
                                ),
                                // Stopped by shutdown() while it still had runtimes.
                                KnownTargetStatus::RemoveAt(instant) => {
                                    KnownTargetStatus::RemoveAt(instant)
                                }
                                KnownTargetStatus::Removing(ref future) => {
                                    KnownTargetStatus::Removing(future.clone())
                                }
                            };
                            // We've marked it Alive so that the Pending check in start_fetcher()
                            // will fail
                            if matches!(*status, KnownTargetStatus::Alive) {
                                break 'drop_service;
                            }
                            0
                        }
                        _ => {
                            if let Some(product_data) =
                                known_service.runtimes.remove(instance_runtime_id)
                            {
                                let mut update_product_data = false;
                                for product in product_data.products {
                                    update_product_data =
                                        decref_hashmap(&mut known_service.active_products, product)
                                            || update_product_data;
                                }
                                for capability in product_data.capabilities {
                                    update_product_data = decref_hashmap(
                                        &mut known_service.active_capabilities,
                                        capability,
                                    ) || update_product_data;
                                }
                                if update_product_data {
                                    known_service.update_product_capabilities();
                                }
                            } else {
                                return;
                            }
                            if known_service.fetcher.runtime_id.lock_or_panic().as_str()
                                == session_id
                            {
                                'changed_rt_id: {
                                    for runtime in runtimes.values() {
                                        if runtime.targets.len() == 1
                                            && runtime.targets.contains_key(target)
                                        {
                                            *known_service.fetcher.runtime_id.lock_or_panic() =
                                                Arc::new(runtime.session_id.clone());
                                            break 'changed_rt_id;
                                        }
                                    }
                                    known_service.synthetic_id = true;
                                    *known_service.fetcher.runtime_id.lock_or_panic() =
                                        Arc::new(Self::generate_synthetic_id());
                                }
                            }
                            known_service.refcount - 1
                        }
                    };
                    break 'service_handling;
                }
            }
            trace!("Remove {target:?} from services map while in pending state");
            services.remove(target);
        }
    }

    fn add_target(
        self: &Arc<Self>,
        synthetic_id: bool,
        runtime_id: &str,
        session_id: &str,
        target: Arc<Target>,
        product_capabilities: ProductCapabilities,
    ) {
        let mut services = self.services.lock_or_panic();
        match services.entry(target.clone()) {
            Entry::Occupied(mut e) => {
                let known_target = &mut e.get_mut();
                // The fetcher still polls with the products of the runtimes which left.
                let mut update_product_data = false;
                if known_target.refcount == 0 {
                    let mut status = known_target.status.lock_or_panic();
                    match *status {
                        KnownTargetStatus::RemoveAt(_) => {
                            *status = KnownTargetStatus::Alive;
                            update_product_data = true;
                            if synthetic_id && !known_target.synthetic_id {
                                known_target.synthetic_id = true;
                                *known_target.fetcher.runtime_id.lock_or_panic() =
                                    Arc::new(Self::generate_synthetic_id());
                            }
                        }
                        KnownTargetStatus::Removing(ref future) => {
                            let future = future.clone();
                            // Avoid deadlocking between known_target.status and self.services
                            self.pending_async_insertions.fetch_add(1, Ordering::AcqRel);
                            let runtime_id = runtime_id.to_string();
                            let session_id = session_id.to_string();
                            let this = self.clone();
                            tokio::spawn(async move {
                                future.await;
                                this.add_target(
                                    synthetic_id,
                                    runtime_id.as_str(),
                                    session_id.as_str(),
                                    target,
                                    product_capabilities,
                                );
                                this.pending_async_insertions.fetch_sub(1, Ordering::AcqRel);
                            });
                            return;
                        }
                        KnownTargetStatus::Alive | KnownTargetStatus::Pending => unreachable!(),
                    }
                }
                known_target.refcount += 1;
                for product in product_capabilities.products.iter() {
                    known_target
                        .active_products
                        .entry(*product)
                        .and_modify(|count| *count += 1)
                        .or_insert_with(|| {
                            update_product_data = true;
                            1
                        });
                }
                for capability in product_capabilities.capabilities.iter() {
                    known_target
                        .active_capabilities
                        .entry(*capability)
                        .and_modify(|count| *count += 1)
                        .or_insert_with(|| {
                            update_product_data = true;
                            1
                        });
                }
                if update_product_data {
                    known_target.update_product_capabilities();
                }
                known_target
                    .runtimes
                    .insert(runtime_id.to_string(), product_capabilities);
                if !synthetic_id && known_target.synthetic_id {
                    known_target.synthetic_id = false;
                    *known_target.fetcher.runtime_id.lock_or_panic() = Arc::new(session_id.into());
                }
            }
            Entry::Vacant(e) => {
                self.start_fetcher(
                    e.insert(KnownTarget {
                        refcount: 1,
                        status: Arc::new(Mutex::new(KnownTargetStatus::Pending)),
                        active_products: product_capabilities
                            .products
                            .iter()
                            .map(|p| (*p, 1))
                            .collect(),
                        active_capabilities: product_capabilities
                            .capabilities
                            .iter()
                            .map(|c| (*c, 1))
                            .collect(),
                        synthetic_id,
                        runtimes: {
                            let mut set = HashMap::default();
                            set.insert(
                                runtime_id.to_string(),
                                ProductCapabilities {
                                    products: product_capabilities.products.clone(),
                                    capabilities: product_capabilities.capabilities.clone(),
                                },
                            );
                            set
                        },
                        fetcher: Arc::new(SharedFetcher::new(
                            target,
                            if synthetic_id {
                                Self::generate_synthetic_id()
                            } else {
                                session_id.into()
                            },
                            ConfigProductCapabilities::new(
                                product_capabilities.products,
                                product_capabilities.capabilities,
                            ),
                        )),
                    }),
                );
            }
        }
    }

    pub fn add_runtime(
        self: &Arc<Self>,
        session_id: String,
        runtime_id: String,
        notify_target: N,
        target: &Arc<Target>,
        product_capabilities: ProductCapabilities,
    ) {
        trace!("Adding remote config runtime: {target:?} with runtime id {runtime_id}");
        match self.runtimes.lock_or_panic().entry(runtime_id) {
            Entry::Occupied(mut runtime_entry) => {
                let info = runtime_entry.get_mut();
                // The runtime may now listen elsewhere, e.g. after reconnecting.
                info.notify_target = notify_target;
                let primary_target = if info.targets.len() == 1 {
                    info.targets.keys().next().cloned()
                } else {
                    None
                };
                match info.targets.entry(target.clone()) {
                    Entry::Occupied(mut e) => *e.get_mut() += 1,
                    Entry::Vacant(e) => {
                        if let Some(primary_target) = primary_target {
                            let mut services = self.services.lock_or_panic();
                            if let Some(known_target) = services.get_mut(&primary_target) {
                                // it's the second usage here
                                if !known_target.synthetic_id {
                                    known_target.synthetic_id = true;
                                    *known_target.fetcher.runtime_id.lock_or_panic() =
                                        Arc::new(Self::generate_synthetic_id());
                                }
                            }
                        }
                        e.insert(1);
                        self.add_target(
                            true,
                            runtime_entry.key(),
                            session_id.as_str(),
                            target.clone(),
                            product_capabilities,
                        );
                    }
                }
            }
            Entry::Vacant(e) => {
                if self
                    .storage
                    .invariants()
                    .endpoint
                    .url
                    .scheme()
                    .map(|s| s.as_str() != "file")
                    == Some(true)
                {
                    let info = RuntimeInfo {
                        notify_target,
                        session_id: session_id.clone(),
                        targets: HashMap::from([(target.clone(), 1)]),
                    };
                    self.add_target(
                        false,
                        e.key(),
                        session_id.as_str(),
                        target.clone(),
                        product_capabilities,
                    );
                    e.insert(info);
                }
            }
        }
    }

    pub fn delete_runtime(
        self: &Arc<Self>,
        runtime_id: &str,
        session_id: &str,
        target: &Arc<Target>,
    ) {
        trace!("Removing remote config runtime: {target:?} with runtime id {runtime_id}");
        let mut runtimes = self.runtimes.lock_or_panic();
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
        Self::remove_target(self, runtime_id, session_id, target, runtimes);
    }

    /// Sets the apply state of `file` (a version of a path) as held by the client of `target`,
    /// reported with its following requests. Discarded, returning false, unless that client
    /// still holds this version.
    pub fn set_config_state(
        &self,
        target: &Target,
        file: &S::StoredFile,
        state: ConfigApplyState,
    ) -> bool {
        let Some(fetcher) = self
            .services
            .lock_or_panic()
            .get(target)
            .map(|known_target| known_target.fetcher.clone())
        else {
            debug!("Discarding the apply state of a file for unknown target {target:?}");
            return false;
        };
        let refcount = file.refcount();
        fetcher.set_config_state(&refcount.path, refcount.version, state)
    }

    fn start_fetcher(self: &Arc<Self>, known_target: &KnownTarget) {
        let this = self.clone();
        let fetcher = known_target.fetcher.clone();
        let status = known_target.status.clone();
        fetcher.interval.store(
            self.remote_config_interval.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        tokio::spawn(async move {
            // Relatively primitive, no prioritization or anything. It is not expected that this
            // semaphore is ever awaiting under standard usage. Can be improved if needed, e.g.
            // sorted by amount of targets on the outstanding services or similar.
            // A target removed while pending is dropped from services, which cancels its fetcher.
            #[allow(clippy::unwrap_used)]
            let permit = select! {
                permit = this.fetcher_semaphore.acquire() => Some(permit.unwrap()),
                _ = fetcher.cancelled() => None,
            };
            let removed_while_pending = {
                let mut status = status.lock_or_panic();
                let pending = permit.is_some() && matches!(*status, KnownTargetStatus::Pending);
                if pending {
                    *status = KnownTargetStatus::Alive;
                }
                !pending
            };
            if removed_while_pending {
                // It never fetched, so it published no listing for expired() to clear; and
                // services may hold a new entry for the same target by now, whose listing
                // expired() would clear. Its removal may still have left no target behind.
                if this.is_dead() {
                    this.storage.storage.dead();
                }
                return;
            }

            let (remove_future, remove_completer) = ManualFuture::new();
            let shared_future = remove_future.shared();

            let inner_fetcher = fetcher.clone();
            let inner_this = this.clone();
            let finish_target = fetcher.target.clone();
            let finish_this = this.clone();
            let fetcher_fut = fetcher
                .run(
                    this.storage.clone(),
                    Box::new(move |files| {
                        let runtime_id = Arc::new(inner_fetcher.runtime_id.lock_or_panic().clone());
                        let notify = inner_this.storage.storage.fetched(
                            &inner_this,
                            &runtime_id,
                            &inner_fetcher.target,
                            files,
                        );

                        if notify {
                            // notify_targets is Hash + Eq + Clone, allowing us to deduplicate. Also
                            // avoid the lock during notifying
                            let mut notify_targets = HashSet::new();
                            {
                                // Block to make sure the lock is released
                                // Ensure runtimes lock is held before locking services
                                let all_runtimes = inner_this.runtimes.lock_or_panic();
                                if let Some(known_target) = inner_this
                                    .services
                                    .lock_or_panic()
                                    .get(&inner_fetcher.target)
                                {
                                    for runtime_id in known_target.runtimes.keys() {
                                        if let Some(runtime) = all_runtimes.get(runtime_id) {
                                            notify_targets.insert(runtime.notify_target.clone());
                                        }
                                    }
                                }
                            }

                            debug!("Notify {:?} about remote config changes", notify_targets);
                            for notify_target in notify_targets {
                                notify_target.notify();
                            }
                        }
                    }),
                    // The target's files are released only once their listing is cleared.
                    Box::new(move || finish_this.storage.storage.expired(&finish_target)),
                )
                .shared();

            loop {
                {
                    let mut status = status.lock_or_panic();
                    if let KnownTargetStatus::RemoveAt(instant) = *status {
                        // Voluntarily give up the semaphore for services in RemoveAt status if
                        // there are only few available permits
                        if this.fetcher_semaphore.available_permits() < 10
                            || instant < Instant::now()
                        {
                            // We need to signal that we're in progress of removing to avoid race
                            // conditions
                            *status = KnownTargetStatus::Removing(shared_future.clone());
                            // break here to drop mutex guard and avoid having status and services
                            // locked simultaneously
                            fetcher.cancel();
                            break;
                        }
                    }
                } // unlock mutex

                select! {
                    _ = tokio::time::sleep(Duration::from_nanos(fetcher.interval.load(Ordering::Relaxed))) => {},
                    _ = fetcher_fut.clone() => {
                        break;
                    }
                }
            }

            // Let the cancelled run end: dropping it instead would never release its files.
            fetcher_fut.await;

            let is_dead = {
                // scope lock before await
                trace!(
                    "Remove {:?} from services map at fetcher end",
                    fetcher.target
                );
                let mut services = this.services.lock_or_panic();
                services.remove(&fetcher.target);
                services.is_empty() && this.pending_async_insertions.load(Ordering::Relaxed) == 0
            };
            if is_dead {
                this.storage.storage.dead();
            }
            remove_completer.complete(()).await;
        });
    }

    /// Stops all targets, whether they still have runtimes or not. Each running target ends
    /// like a removed one: its listing is cleared, then its files are released.
    pub fn shutdown(&self) {
        self.services.lock_or_panic().retain(|target, service| {
            let mut status = service.status.lock_or_panic();
            match *status {
                KnownTargetStatus::Pending => {
                    trace!("Remove pending {target:?} from services map at shutdown");
                    // Not Pending, lest its task starts it once it gets a fetcher slot.
                    *status = KnownTargetStatus::RemoveAt(Instant::now());
                    false
                }
                KnownTargetStatus::Alive | KnownTargetStatus::RemoveAt(_) => {
                    *status = KnownTargetStatus::RemoveAt(Instant::now());
                    service.fetcher.cancel();
                    true
                }
                KnownTargetStatus::Removing(_) => true,
            }
        });
    }

    pub fn active_runtimes(&self) -> usize {
        self.runtimes.lock_or_panic().len()
    }

    pub fn invariants(&self) -> &ConfigInvariants {
        self.storage.invariants()
    }

    pub fn force_product(&self, target: &Target, product: RemoteConfigProduct) {
        if let Some(known_target) = self.services.lock_or_panic().get_mut(target)
            && addref_hashmap(&mut known_target.active_products, product)
        {
            known_target.update_product_capabilities();
        }
    }

    pub fn force_capabilities(&self, target: &Target, capabilities: RemoteConfigCapabilities) {
        if let Some(known_target) = self.services.lock_or_panic().get_mut(target)
            && addref_hashmap(&mut known_target.active_capabilities, capabilities)
        {
            known_target.update_product_capabilities();
        }
    }

    pub fn unforce_product(&self, target: &Target, product: RemoteConfigProduct) {
        if let Some(known_target) = self.services.lock_or_panic().get_mut(target)
            && decref_hashmap(&mut known_target.active_products, product)
        {
            known_target.update_product_capabilities();
        }
    }

    pub fn unforce_capabilities(&self, target: &Target, capabilities: RemoteConfigCapabilities) {
        if let Some(known_target) = self.services.lock_or_panic().get_mut(target)
            && decref_hashmap(&mut known_target.active_capabilities, capabilities)
        {
            known_target.update_product_capabilities();
        }
    }

    pub fn stats(&self) -> MultiTargetStats {
        let (starting_fetchers, active_fetchers, inactive_fetchers, removing_fetchers) = {
            let services = self.services.lock_or_panic();
            let mut starting = 0;
            let mut active = 0;
            let mut inactive = 0;
            let mut removing = 0;
            for known_target in services.values() {
                match *known_target.status.lock_or_panic() {
                    KnownTargetStatus::Pending => starting += 1,
                    KnownTargetStatus::Alive => active += 1,
                    KnownTargetStatus::RemoveAt(_) => inactive += 1,
                    KnownTargetStatus::Removing(_) => removing += 1,
                }
            }
            (starting, active, inactive, removing)
        };
        MultiTargetStats {
            known_runtimes: self.active_runtimes() as u32,
            starting_fetchers,
            active_fetchers,
            inactive_fetchers,
            removing_fetchers,
            storage: self.storage.stats(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::fetcher::tests::*;
    use crate::fetch::shared::tests::*;
    use crate::fetch::test_server::RemoteConfigServer;
    use crate::{RemoteConfigPath, Target};
    use libdd_capabilities_impl::NativeCapabilities;
    use manual_future::ManualFutureCompleter;
    use std::hash::Hasher;
    use std::sync::Weak;
    use std::sync::atomic::{AtomicBool, AtomicU8};

    #[derive(Clone)]
    struct MultiFileStorage {
        rc: RcFileStorage,
        on_dead_completer: Arc<Mutex<Option<ManualFutureCompleter<()>>>>,
        #[allow(clippy::type_complexity)]
        recent_fetches: Arc<Mutex<HashMap<Arc<Target>, Vec<Arc<RcPathStore>>>>>,
        awaiting_fetches: Arc<AtomicU8>,
        awaited_fetched_done: Arc<Mutex<Option<ManualFutureCompleter<()>>>>,
        expected_expirations: Arc<Mutex<HashMap<Arc<Target>, ManualFutureCompleter<()>>>>,
        /// The files each target's current listing names.
        #[allow(clippy::type_complexity)]
        listed: Arc<Mutex<HashMap<Arc<Target>, Vec<Weak<RcPathStore>>>>>,
        /// Set if a target's files were released before its listing was cleared.
        released_while_listed: Arc<AtomicBool>,
    }

    impl MultiFileStorage {
        pub fn await_fetches(&self, num: u8) -> ManualFuture<()> {
            let (future, completer) = ManualFuture::new();

            self.recent_fetches.lock().unwrap().clear();
            self.awaiting_fetches.store(num, Ordering::SeqCst);
            *self.awaited_fetched_done.lock().unwrap() = Some(completer);

            future
        }

        pub fn expect_expiration(&self, target: &Arc<Target>) -> ManualFuture<()> {
            let (future, completer) = ManualFuture::new();
            self.expected_expirations
                .lock()
                .unwrap()
                .insert(target.clone(), completer);
            future
        }
    }

    impl MultiTargetHandlers<Notifier, MultiFileStorage, NativeCapabilities> for MultiFileStorage {
        fn fetched(
            &self,
            _fetcher: &Arc<MultiTargetFetcher<Notifier, MultiFileStorage, NativeCapabilities>>,
            _runtime_id: &Arc<String>,
            target: &Arc<Target>,
            files: &[Arc<RcPathStore>],
        ) -> bool {
            self.listed
                .lock()
                .unwrap()
                .insert(target.clone(), files.iter().map(Arc::downgrade).collect());
            match self.recent_fetches.lock().unwrap().entry(target.clone()) {
                Entry::Occupied(_) => panic!("Double fetch without recent_fetches clear"),
                Entry::Vacant(e) => {
                    e.insert(files.to_vec());
                }
            }

            match self.awaiting_fetches.fetch_sub(1, Ordering::SeqCst) {
                2.. => {}
                1 => {
                    tokio::spawn(
                        self.awaited_fetched_done
                            .lock()
                            .unwrap()
                            .take()
                            .unwrap()
                            .complete(()),
                    );
                }
                ..=0 => panic!("Got unexpected fetch"),
            }

            true
        }

        fn expired(&self, target: &Arc<Target>) {
            if let Some(listed) = self.listed.lock().unwrap().remove(target)
                && listed.iter().any(|f| f.strong_count() == 0)
            {
                self.released_while_listed.store(true, Ordering::SeqCst);
            }
            tokio::spawn(
                self.expected_expirations
                    .lock()
                    .unwrap()
                    .remove(target)
                    .unwrap()
                    .complete(()),
            );
        }

        fn dead(&self) {
            tokio::spawn(
                self.on_dead_completer
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .complete(()),
            );
        }
    }

    impl FileStorage for MultiFileStorage {
        type StoredFile = <RcFileStorage as FileStorage>::StoredFile;

        fn store(
            &self,
            version: u64,
            path: Arc<RemoteConfigPath>,
            contents: Vec<u8>,
        ) -> anyhow::Result<Arc<Self::StoredFile>> {
            self.rc.store(version, path, contents)
        }
    }

    #[derive(Default, Debug)]
    struct NotifyState {
        notifications: Mutex<HashSet<u8>>,
    }

    impl NotifyState {
        fn assert_notified(&self, ids: &[u8]) {
            let mut notified = std::mem::take(&mut *self.notifications.lock().unwrap())
                .into_iter()
                .collect::<Vec<u8>>();
            notified.sort();
            assert_eq!(notified, ids);
        }
    }

    #[derive(Clone, Debug)]
    struct Notifier {
        id: u8,
        state: Arc<NotifyState>,
    }

    impl Hash for Notifier {
        fn hash<H: Hasher>(&self, state: &mut H) {
            state.write_u8(self.id)
        }
    }

    impl PartialEq<Self> for Notifier {
        fn eq(&self, other: &Self) -> bool {
            self.id == other.id
        }
    }

    impl Eq for Notifier {}

    impl NotifyTarget for Notifier {
        fn notify(&self) {
            self.state.notifications.lock().unwrap().insert(self.id);
        }
    }

    static RT_ID_1: &str = "3b43524b-a70c-45dc-921d-34504e50c5eb";
    static RT_ID_2: &str = "ae588386-8464-43ba-bd3a-3e2d36b2c22c";
    static RT_ID_3: &str = "0125dff8-d9a7-4fd3-a0c2-0ca3b12816a1";

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_multi_fetcher() {
        let server = RemoteConfigServer::spawn();
        let (on_dead, on_dead_completer) = ManualFuture::new();
        let storage = MultiFileStorage {
            rc: RcFileStorage::default(),
            on_dead_completer: Arc::new(Mutex::new(Some(on_dead_completer))),
            recent_fetches: Default::default(),
            awaiting_fetches: Default::default(),
            awaited_fetched_done: Default::default(),
            expected_expirations: Default::default(),
            listed: Default::default(),
            released_while_listed: Default::default(),
        };
        let state = Arc::new(NotifyState::default());

        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let fut = storage.await_fetches(1);

        let fetcher = MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::new(
            storage.clone(),
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        fetcher.remote_config_interval.store(1000, Ordering::SeqCst);

        fetcher.add_runtime(
            RT_ID_1.to_string(),
            RT_ID_1.to_string(),
            Notifier {
                id: 1,
                state: state.clone(),
            },
            &OTHER_TARGET,
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
        );
        assert_eq!(
            fetcher
                .services
                .lock()
                .unwrap()
                .get(&*OTHER_TARGET)
                .unwrap()
                .fetcher
                .runtime_id
                .lock()
                .unwrap()
                .as_str(),
            RT_ID_1
        );

        fetcher.add_runtime(
            RT_ID_1.to_string(),
            RT_ID_1.to_string(),
            Notifier {
                id: 1,
                state: state.clone(),
            },
            &DUMMY_TARGET,
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
        );
        fetcher.add_runtime(
            RT_ID_2.to_string(),
            RT_ID_2.to_string(),
            Notifier {
                id: 2,
                state: state.clone(),
            },
            &DUMMY_TARGET,
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
        );

        assert_eq!(
            fetcher
                .services
                .lock()
                .unwrap()
                .get(&*DUMMY_TARGET)
                .unwrap()
                .fetcher
                .runtime_id
                .lock()
                .unwrap()
                .as_str(),
            RT_ID_2
        );
        assert_ne!(
            fetcher
                .services
                .lock()
                .unwrap()
                .get(&*OTHER_TARGET)
                .unwrap()
                .fetcher
                .runtime_id
                .lock()
                .unwrap()
                .as_str(),
            RT_ID_1
        );

        assert_eq!(fetcher.runtimes.lock().unwrap().len(), 2); // two runtimes
        assert_eq!(fetcher.services.lock().unwrap().len(), 2); // two fetchers

        fetcher.add_runtime(
            RT_ID_3.to_string(),
            RT_ID_3.to_string(),
            Notifier {
                id: 3,
                state: state.clone(),
            },
            &OTHER_TARGET,
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
        );

        fut.await;
        state.assert_notified(&[1, 2]);

        let last_fetched: Vec<_> = storage
            .recent_fetches
            .lock()
            .unwrap()
            .get(&*DUMMY_TARGET)
            .unwrap()
            .iter()
            .map(|p| p.store.data.clone())
            .collect();
        assert_eq!(last_fetched.len(), 1);

        let fut = storage.await_fetches(2);
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![OTHER_TARGET.clone()], 1, "v1".to_string()),
        );

        fut.await;
        state.assert_notified(&[1, 2, 3]);

        let new_fetched: Vec<_> = storage
            .recent_fetches
            .lock()
            .unwrap()
            .get(&*OTHER_TARGET)
            .unwrap()
            .iter()
            .map(|p| p.store.data.clone())
            .collect();
        assert_eq!(
            storage
                .recent_fetches
                .lock()
                .unwrap()
                .get(&*OTHER_TARGET)
                .unwrap()
                .len(),
            1
        );
        if !Arc::ptr_eq(&new_fetched[0], &last_fetched[0]) {
            assert_eq!(
                *new_fetched[0].lock().unwrap(),
                *last_fetched[0].lock().unwrap()
            );
        }

        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &OTHER_TARGET);
        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &DUMMY_TARGET);
        fetcher.delete_runtime(RT_ID_2, RT_ID_2, &DUMMY_TARGET);
        fetcher.delete_runtime(RT_ID_3, RT_ID_3, &OTHER_TARGET);

        fetcher.shutdown();
        storage.expect_expiration(&DUMMY_TARGET);
        storage.expect_expiration(&OTHER_TARGET);

        on_dead.await
    }

    /// A removed target's listing names its files until expired() clears it; the files must
    /// outlive that, and be released afterwards.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn removed_target_releases_files_after_clearing_its_listing() {
        let server = RemoteConfigServer::spawn();
        let (on_dead, on_dead_completer) = ManualFuture::new();
        let storage = MultiFileStorage {
            rc: RcFileStorage::default(),
            on_dead_completer: Arc::new(Mutex::new(Some(on_dead_completer))),
            recent_fetches: Default::default(),
            awaiting_fetches: Default::default(),
            awaited_fetched_done: Default::default(),
            expected_expirations: Default::default(),
            listed: Default::default(),
            released_while_listed: Default::default(),
        };
        let state = Arc::new(NotifyState::default());
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let fut = storage.await_fetches(1);
        let fetcher = MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::new(
            storage.clone(),
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        fetcher.remote_config_interval.store(1000, Ordering::SeqCst);
        fetcher.add_runtime(
            RT_ID_1.to_string(),
            RT_ID_1.to_string(),
            Notifier { id: 1, state },
            &DUMMY_TARGET,
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
        );
        fut.await;
        // Only the fetcher may keep the delivered files alive.
        storage.recent_fetches.lock().unwrap().clear();
        assert_eq!(storage.rc.0.files.lock().unwrap().len(), 1);

        let expired = storage.expect_expiration(&DUMMY_TARGET);
        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &DUMMY_TARGET);
        fetcher.shutdown();
        expired.await;
        on_dead.await;

        assert!(
            !storage.released_while_listed.load(Ordering::SeqCst),
            "the files were released while the target's listing still named them"
        );
        assert!(
            storage.rc.0.files.lock().unwrap().is_empty(),
            "the removed target's files were never released"
        );
    }

    fn multi_file_storage() -> (MultiFileStorage, ManualFuture<()>) {
        let (on_dead, on_dead_completer) = ManualFuture::new();
        let storage = MultiFileStorage {
            rc: RcFileStorage::default(),
            on_dead_completer: Arc::new(Mutex::new(Some(on_dead_completer))),
            recent_fetches: Default::default(),
            awaiting_fetches: Default::default(),
            awaited_fetched_done: Default::default(),
            expected_expirations: Default::default(),
            listed: Default::default(),
            released_while_listed: Default::default(),
        };
        (storage, on_dead)
    }

    /// Returns the products of a request the fetcher started after this call.
    async fn products_of_a_later_request(server: &RemoteConfigServer) -> Vec<String> {
        // The first request seen may have been in flight already.
        for _ in 0..2 {
            *server.last_request.lock().unwrap() = None;
            while server.last_request.lock().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        let mut products = server
            .last_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .products
            .clone();
        products.sort();
        products
    }

    /// A target revived by a new runtime while waiting for removal polls with the products of
    /// its current runtimes, plus the forced ones.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn revived_target_polls_with_the_products_of_its_new_runtimes() {
        let server = RemoteConfigServer::spawn();
        let (storage, on_dead) = multi_file_storage();
        let state = Arc::new(NotifyState::default());
        let fetcher = MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::new(
            storage.clone(),
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        fetcher.remote_config_interval.store(1000, Ordering::SeqCst);
        let runtime = |id: u8, product| {
            (
                Notifier {
                    id,
                    state: state.clone(),
                },
                ProductCapabilities {
                    products: vec![product],
                    capabilities: vec![],
                },
            )
        };

        let (notifier, products) = runtime(1, RemoteConfigProduct::ApmTracing);
        fetcher.add_runtime(
            RT_ID_1.to_string(),
            RT_ID_1.to_string(),
            notifier,
            &DUMMY_TARGET,
            products,
        );
        fetcher.force_product(&DUMMY_TARGET, RemoteConfigProduct::AsmFeatures);
        assert_eq!(
            products_of_a_later_request(&server).await,
            ["APM_TRACING", "ASM_FEATURES"]
        );

        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &DUMMY_TARGET);
        let (notifier, products) = runtime(2, RemoteConfigProduct::LiveDebugging);
        fetcher.add_runtime(
            RT_ID_2.to_string(),
            RT_ID_2.to_string(),
            notifier,
            &DUMMY_TARGET,
            products,
        );
        assert_eq!(
            products_of_a_later_request(&server).await,
            ["ASM_FEATURES", "LIVE_DEBUGGING"]
        );

        let expired = storage.expect_expiration(&DUMMY_TARGET);
        fetcher.delete_runtime(RT_ID_2, RT_ID_2, &DUMMY_TARGET);
        fetcher.shutdown();
        expired.await;
        on_dead.await;
    }

    /// A target removed while waiting for a fetcher slot ends like any other: the fetcher is
    /// reported dead once it has no target left. Having fetched nothing, it has no listing.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn target_removed_while_pending_is_reported_dead() {
        let server = RemoteConfigServer::spawn();
        let (storage, on_dead) = multi_file_storage();
        let fetcher = MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::new(
            storage.clone(),
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        fetcher.remote_config_interval.store(1000, Ordering::SeqCst);
        // Take every fetcher slot, so that the target stays pending.
        let _all_slots = fetcher
            .fetcher_semaphore
            .acquire_many(MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::DEFAULT_CLIENTS_LIMIT)
            .await
            .unwrap();

        fetcher.add_runtime(
            RT_ID_1.to_string(),
            RT_ID_1.to_string(),
            Notifier {
                id: 1,
                state: Default::default(),
            },
            &DUMMY_TARGET,
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
        );
        assert_eq!(fetcher.stats().starting_fetchers, 1);

        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &DUMMY_TARGET);
        tokio::time::timeout(Duration::from_secs(5), on_dead)
            .await
            .expect("the fetcher was never reported dead");
        assert!(fetcher.is_dead());
    }

    /// shutdown() stops every target, whatever its state: a running target releases its files
    /// once its listing is cleared, and a pending one ends without fetching.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn shutdown_stops_running_and_pending_targets() {
        let server = RemoteConfigServer::spawn();
        let (storage, on_dead) = multi_file_storage();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );
        let fetcher = MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::new(
            storage.clone(),
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        fetcher.remote_config_interval.store(1000, Ordering::SeqCst);
        // Leave a single fetcher slot, so that only the first target runs.
        let _other_slots = fetcher
            .fetcher_semaphore
            .acquire_many(
                MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::DEFAULT_CLIENTS_LIMIT - 1,
            )
            .await
            .unwrap();
        let add = |runtime_id: &str, id, target| {
            fetcher.add_runtime(
                runtime_id.to_string(),
                runtime_id.to_string(),
                Notifier {
                    id,
                    state: Default::default(),
                },
                target,
                ProductCapabilities {
                    products: server.dummy_options().products,
                    capabilities: server.dummy_options().capabilities,
                },
            )
        };

        let fut = storage.await_fetches(1);
        add(RT_ID_1, 1, &DUMMY_TARGET);
        fut.await;
        add(RT_ID_2, 2, &OTHER_TARGET);
        // Only the fetcher may keep the delivered files alive.
        storage.recent_fetches.lock().unwrap().clear();
        assert_eq!(storage.rc.0.files.lock().unwrap().len(), 1);
        let stats = fetcher.stats();
        assert_eq!((stats.active_fetchers, stats.starting_fetchers), (1, 1));

        let expired = storage.expect_expiration(&DUMMY_TARGET);
        fetcher.shutdown();
        tokio::time::timeout(Duration::from_secs(5), expired)
            .await
            .expect("the running target was never stopped");
        tokio::time::timeout(Duration::from_secs(5), on_dead)
            .await
            .expect("the fetcher was never reported dead");

        assert!(fetcher.is_dead());
        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &DUMMY_TARGET);
        fetcher.delete_runtime(RT_ID_2, RT_ID_2, &OTHER_TARGET);
        assert!(
            !storage.released_while_listed.load(Ordering::SeqCst),
            "the files were released while the target's listing still named them"
        );
        assert!(
            storage.rc.0.files.lock().unwrap().is_empty(),
            "the stopped target's files were never released"
        );
    }

    /// The runtimes of a target being stopped by shutdown() may leave before it stopped.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn runtime_leaving_a_target_stopped_by_shutdown() {
        let server = RemoteConfigServer::spawn();
        let (storage, on_dead) = multi_file_storage();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );
        let fetcher = MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::new(
            storage.clone(),
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        fetcher.remote_config_interval.store(1000, Ordering::SeqCst);
        let fut = storage.await_fetches(1);
        fetcher.add_runtime(
            RT_ID_1.to_string(),
            RT_ID_1.to_string(),
            Notifier {
                id: 1,
                state: Default::default(),
            },
            &DUMMY_TARGET,
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
        );
        fut.await;
        storage.recent_fetches.lock().unwrap().clear();

        let expired = storage.expect_expiration(&DUMMY_TARGET);
        fetcher.shutdown();
        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &DUMMY_TARGET);
        tokio::time::timeout(Duration::from_secs(5), expired)
            .await
            .expect("the target was never stopped");
        tokio::time::timeout(Duration::from_secs(5), on_dead)
            .await
            .expect("the fetcher was never reported dead");
        assert!(storage.rc.0.files.lock().unwrap().is_empty());
    }

    /// A runtime registering again with another notify target is notified at the new one.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn runtime_is_notified_at_its_latest_notify_target() {
        let server = RemoteConfigServer::spawn();
        let (storage, on_dead) = multi_file_storage();
        let state = Arc::new(NotifyState::default());
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );
        let fetcher = MultiTargetFetcher::<Notifier, MultiFileStorage, NativeCapabilities>::new(
            storage.clone(),
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        fetcher.remote_config_interval.store(1000, Ordering::SeqCst);
        let add = |id| {
            fetcher.add_runtime(
                RT_ID_1.to_string(),
                RT_ID_1.to_string(),
                Notifier {
                    id,
                    state: state.clone(),
                },
                &DUMMY_TARGET,
                ProductCapabilities {
                    products: server.dummy_options().products,
                    capabilities: server.dummy_options().capabilities,
                },
            )
        };

        let fut = storage.await_fetches(1);
        add(1);
        fut.await;
        state.assert_notified(&[1]);

        add(2);
        let fut = storage.await_fetches(1);
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
        );
        fut.await;
        state.assert_notified(&[2]);

        storage.recent_fetches.lock().unwrap().clear();
        let expired = storage.expect_expiration(&DUMMY_TARGET);
        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &DUMMY_TARGET);
        fetcher.delete_runtime(RT_ID_1, RT_ID_1, &DUMMY_TARGET);
        fetcher.shutdown();
        expired.await;
        on_dead.await;
    }
}
