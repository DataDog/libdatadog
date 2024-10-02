// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::{
    ConfigApplyState, ConfigClientState, ConfigFetcher, ConfigFetcherState,
    ConfigFetcherStateStats, ConfigInvariants, FileStorage,
};
use crate::{RemoteConfigPath, Target};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Add;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::select;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::error;

/// Fetcher which does a run-loop and carefully manages state around files, with the following
/// guarantees:
///  - A file at a given RemoteConfigPath will not be recreated as long as it exists I.e. it will
///    always be drop()'ed before recreation.
///  - It does not leak files which are no longer in use, i.e. it refcounts across all remote config
///    clients sharing the same RefcountingStorage.
///  - The state is always valid, i.e. there will be no intermittently expired files.
pub struct SharedFetcher {
    /// (env, service, version) tuple representing the basic remote config target
    pub target: Arc<Target>, // could be theoretically also Mutex<>ed if needed
    /// A unique runtime id. It must not be used by any other remote config client at the same
    /// time. Is allowed to be changed at any time.
    pub runtime_id: Arc<Mutex<String>>,
    /// Each fetcher must have an unique id. Defaults to a random UUID.
    pub client_id: String,
    cancellation: CancellationToken,
    /// Refetch interval in nanoseconds.
    pub interval: AtomicU64,
}

pub struct FileRefcountData {
    /// Primary refcounter:
    ///  - When active (dropped_run_id == 0), the amount of runners holding it since the last
    ///    remote config fetch.
    ///  - When inactive (dropped_run_id > 0), the remaining amount of runners actively fetching
    ///    remote config at the point in time dropped_run_id represents.
    rc: AtomicU32,
    /// 0, or point in time (see RunnersGeneration) where the file was moved to inactive.
    dropped_run_id: AtomicU64,
    pub path: Arc<RemoteConfigPath>,
    pub version: u64,
}

impl FileRefcountData {
    pub fn new(version: u64, path: Arc<RemoteConfigPath>) -> Self {
        FileRefcountData {
            rc: AtomicU32::new(0),
            dropped_run_id: AtomicU64::new(0),
            path,
            version,
        }
    }
}

pub trait RefcountedFile {
    fn refcount(&self) -> &FileRefcountData;

    fn incref(&self) -> u32 {
        self.refcount().rc.fetch_add(1, Ordering::AcqRel)
    }

    fn delref(&self) -> u32 {
        self.refcount().rc.fetch_sub(1, Ordering::AcqRel)
    }

    fn setref(&self, val: u32) {
        self.refcount().rc.store(val, Ordering::SeqCst)
    }

    fn set_expiring_run_id(&self, val: u64) {
        self.refcount().dropped_run_id.store(val, Ordering::SeqCst)
    }

    fn get_expiring_run_id(&self) -> u64 {
        self.refcount().dropped_run_id.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct RunnersGeneration {
    /// This atomic contains both run_id and runners count; saving us from needing a Mutex.
    val: AtomicU64,
}

/// Atomic structure to represent the exact amount of remote config fetching runners at a specific
/// point in time represented by the generation (run_id), an integer which is only ever incremented.
/// This data structure helps contain which inactive files are pending deletion.
impl RunnersGeneration {
    const RUN_ID_SHIFT: i32 = 20;

    /// Increments run_id and increments active runners. Returns first run_id to watch for.
    fn inc_runners(&self) -> u64 {
        (self
            .val
            .fetch_add((1 << Self::RUN_ID_SHIFT) + 1, Ordering::SeqCst)
            >> Self::RUN_ID_SHIFT)
            + 1
    }

    /// Increments run_id and decrements active runners. Returns last run_id to watch for.
    fn dec_runners(&self) -> u64 {
        self.val
            .fetch_add((1 << Self::RUN_ID_SHIFT) - 1, Ordering::SeqCst)
            >> Self::RUN_ID_SHIFT
    }

    /// Returns amount of active runners and current run_id.
    fn runners_and_run_id(&self) -> (u32, u64) {
        let val = self.val.load(Ordering::Acquire);
        (
            (val & ((1 << Self::RUN_ID_SHIFT) - 1)) as u32,
            val >> Self::RUN_ID_SHIFT,
        )
    }
}

pub struct RefcountingStorage<S: FileStorage + Clone>
where
    S::StoredFile: RefcountedFile,
{
    pub storage: S,
    state: Arc<ConfigFetcherState<S::StoredFile>>,
    /// Stores recently expired files. When a file refcount drops to zero, they're no longer sent
    /// via the remote config client. However, there may still be in-flight requests, with telling
    /// the remote config server that we know about these files. Thus, as long as these requests
    /// are being processed, we must retain the files, as these would not be resent, leaving us
    /// with a potentially incomplete configuration.
    #[allow(clippy::type_complexity)]
    inactive: Arc<Mutex<HashMap<Arc<RemoteConfigPath>, Arc<S::StoredFile>>>>,
    /// times ConfigFetcher::<S>::fetch_once() is currently being run
    run_id: Arc<RunnersGeneration>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct RefcountingStorageStats {
    pub inactive_files: u32,
    pub fetcher: ConfigFetcherStateStats,
}

impl Add for RefcountingStorageStats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        RefcountingStorageStats {
            inactive_files: self.inactive_files + rhs.inactive_files,
            fetcher: self.fetcher + rhs.fetcher,
        }
    }
}

impl<S: FileStorage + Clone> Clone for RefcountingStorage<S>
where
    S::StoredFile: RefcountedFile,
{
    fn clone(&self) -> Self {
        RefcountingStorage {
            storage: self.storage.clone(),
            state: self.state.clone(),
            inactive: self.inactive.clone(),
            run_id: self.run_id.clone(),
        }
    }
}

impl<S: FileStorage + Clone> RefcountingStorage<S>
where
    S::StoredFile: RefcountedFile,
{
    pub fn new(storage: S, mut state: ConfigFetcherState<S::StoredFile>) -> Self {
        state.expire_unused_files = false;
        RefcountingStorage {
            storage,
            state: Arc::new(state),
            inactive: Default::default(),
            run_id: Default::default(),
        }
    }

    fn expire_file(&mut self, file: Arc<S::StoredFile>) {
        let mut expire_lock = self.state.files_lock();
        let mut inactive = self.inactive.lock().unwrap();
        if file.refcount().rc.load(Ordering::Relaxed) != 0 {
            return; // Don't do anything if refcount was increased while acquiring the lock
        }
        expire_lock.mark_expiring(&file.refcount().path);
        let (runners, run_id) = self.run_id.runners_and_run_id();
        if runners > 0 {
            file.setref(runners);
            file.set_expiring_run_id(run_id);
            inactive.insert(file.refcount().path.clone(), file);
        } else {
            expire_lock.expire_file(&file.refcount().path);
        }
    }

    /// Sets the apply state on a stored file.
    pub fn set_config_state(&self, file: &S::StoredFile, state: ConfigApplyState) {
        self.state.set_config_state(&file.refcount().path, state)
    }

    pub fn invariants(&self) -> &ConfigInvariants {
        &self.state.invariants
    }

    pub fn stats(&self) -> RefcountingStorageStats {
        RefcountingStorageStats {
            inactive_files: self.inactive.lock().unwrap().len() as u32,
            fetcher: self.state.stats(),
        }
    }
}

impl<S: FileStorage + Clone> FileStorage for RefcountingStorage<S>
where
    S::StoredFile: RefcountedFile,
{
    type StoredFile = S::StoredFile;

    fn store(
        &self,
        version: u64,
        path: Arc<RemoteConfigPath>,
        contents: Vec<u8>,
    ) -> anyhow::Result<Arc<Self::StoredFile>> {
        self.storage.store(version, path, contents)
    }

    fn update(
        &self,
        file: &Arc<Self::StoredFile>,
        version: u64,
        contents: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.storage.update(file, version, contents)
    }
}

impl SharedFetcher {
    pub fn new(target: Arc<Target>, runtime_id: String) -> Self {
        SharedFetcher {
            target,
            runtime_id: Arc::new(Mutex::new(runtime_id)),
            client_id: uuid::Uuid::new_v4().to_string(),
            cancellation: CancellationToken::new(),
            interval: AtomicU64::new(5_000_000_000),
        }
    }

    /// Runs.
    /// On successful fetches on_fetch() is called with the new configuration.
    /// Should not be called more than once.
    #[allow(clippy::type_complexity)]
    pub async fn run<S: FileStorage + Clone>(
        &self,
        storage: RefcountingStorage<S>,
        on_fetch: Box<dyn Send + Fn(&Vec<Arc<S::StoredFile>>)>,
    ) where
        S::StoredFile: RefcountedFile,
    {
        let state = storage.state.clone();
        let mut fetcher = ConfigFetcher::new(storage, state);

        let mut opaque_state = ConfigClientState::default();

        let mut last_files: Vec<Arc<S::StoredFile>> = vec![];

        loop {
            let first_run_id = fetcher.file_storage.run_id.inc_runners();

            let runtime_id = self.runtime_id.lock().unwrap().clone();
            let fetched = fetcher
                .fetch_once(
                    runtime_id.as_str(),
                    self.target.clone(),
                    self.client_id.as_str(),
                    &mut opaque_state,
                )
                .await;

            let clean_inactive = || {
                let run_range = first_run_id..=fetcher.file_storage.run_id.dec_runners();
                let mut inactive = fetcher.file_storage.inactive.lock().unwrap();
                inactive.retain(|_, v| {
                    if run_range.contains(&v.get_expiring_run_id()) && v.delref() == 1 {
                        fetcher
                            .file_storage
                            .state
                            .files_lock()
                            .expire_file(&v.refcount().path);
                        false
                    } else {
                        true
                    }
                });
            };

            match fetched {
                Ok(None) => clean_inactive(), // nothing changed
                Ok(Some(files)) => {
                    if !files.is_empty() || !last_files.is_empty() {
                        for file in files.iter() {
                            if file.get_expiring_run_id() != 0 {
                                let mut inactive = fetcher.file_storage.inactive.lock().unwrap();
                                if inactive.remove(&file.refcount().path).is_some() {
                                    file.setref(0);
                                    file.set_expiring_run_id(0);
                                }
                            }
                            file.incref();
                        }

                        clean_inactive();

                        for file in last_files {
                            if file.delref() == 1 {
                                fetcher.file_storage.expire_file(file);
                            }
                        }

                        last_files = files;

                        on_fetch(&last_files);
                    } else {
                        clean_inactive();
                    }
                }
                Err(e) => {
                    clean_inactive();
                    error!("{:?}", e);
                }
            }

            select! {
                _ = self.cancellation.cancelled() => { break; }
                _ = sleep(Duration::from_nanos(self.interval.load(Ordering::Relaxed))) => {}
            }
        }

        for file in last_files {
            if file.delref() == 1 {
                fetcher.file_storage.expire_file(file);
            }
        }
    }

    /// Note that due to the async logic, a cancellation does not guarantee a strict ordering:
    /// A final on_fetch call from within the run() method may happen after the cancellation.
    /// Cancelling from within on_fetch callback is always final.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::fetch::fetcher::tests::*;
    use crate::fetch::test_server::RemoteConfigServer;
    use crate::Target;
    use futures::future::join_all;
    use lazy_static::lazy_static;
    use std::sync::Arc;

    lazy_static! {
        pub static ref OTHER_TARGET: Arc<Target> = Arc::new(Target {
            service: "other".to_string(),
            env: "env".to_string(),
            app_version: "7.8.9".to_string(),
        });
    }

    pub struct RcPathStore {
        pub store: Arc<PathStore>,
        refcounted: FileRefcountData,
    }

    impl RefcountedFile for RcPathStore {
        fn refcount(&self) -> &FileRefcountData {
            &self.refcounted
        }
    }

    #[derive(Default, Clone)]
    pub struct RcFileStorage(Arc<Storage>);

    impl FileStorage for RcFileStorage {
        type StoredFile = RcPathStore;

        fn store(
            &self,
            version: u64,
            path: Arc<RemoteConfigPath>,
            contents: Vec<u8>,
        ) -> anyhow::Result<Arc<Self::StoredFile>> {
            Ok(Arc::new(RcPathStore {
                store: self.0.store(version, path.clone(), contents)?,
                refcounted: FileRefcountData::new(version, path),
            }))
        }

        fn update(
            &self,
            file: &Arc<Self::StoredFile>,
            version: u64,
            contents: Vec<u8>,
        ) -> anyhow::Result<()> {
            self.0.update(&file.store, version, contents)
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_single_fetcher() {
        let server = RemoteConfigServer::spawn();
        let storage = RcFileStorage::default();
        let rc_storage = RefcountingStorage::new(
            storage.clone(),
            ConfigFetcherState::new(server.dummy_invariants()),
        );

        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let fetcher = SharedFetcher::new(
            DUMMY_TARGET.clone(),
            "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
        );
        let iteration = AtomicU32::new(0);
        let inner_fetcher = unsafe { &*(&fetcher as *const SharedFetcher) };
        let inner_storage = storage.clone();
        fetcher
            .run(
                rc_storage,
                Box::new(
                    move |fetched| match iteration.fetch_add(1, Ordering::SeqCst) {
                        0 => {
                            assert_eq!(fetched.len(), 1);
                            assert_eq!(fetched[0].store.data.lock().unwrap().contents, "v1");

                            server.files.lock().unwrap().insert(
                                PATH_SECOND.clone(),
                                (vec![DUMMY_TARGET.clone()], 1, "X".to_string()),
                            );
                        }
                        1 => {
                            assert_eq!(fetched.len(), 2);

                            server.files.lock().unwrap().remove(&*PATH_SECOND);
                        }
                        2 => {
                            assert_eq!(fetched.len(), 1);
                            assert_eq!(inner_storage.0.files.lock().unwrap().len(), 1);
                            let req = server.last_request.lock().unwrap();
                            let req = req.as_ref().unwrap();
                            let client = req.client.as_ref().unwrap();
                            let state = client.state.as_ref().unwrap();
                            assert!(!state.has_error);

                            inner_fetcher.cancel();
                        }
                        _ => panic!("Unexpected"),
                    },
                ),
            )
            .await;

        assert!(storage.0.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_parallel_fetchers() {
        let server = RemoteConfigServer::spawn();
        let storage = RcFileStorage::default();
        let rc_storage = RefcountingStorage::new(
            storage.clone(),
            ConfigFetcherState::new(server.dummy_invariants()),
        );

        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (
                vec![DUMMY_TARGET.clone(), OTHER_TARGET.clone()],
                1,
                "v1".to_string(),
            ),
        );
        server.files.lock().unwrap().insert(
            PATH_SECOND.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "X".to_string()),
        );

        let server_1 = server.clone();
        let server_1_storage = storage.clone();
        let server_first_1 = move || {
            assert_eq!(server_1_storage.0.files.lock().unwrap().len(), 2);
            server_1.files.lock().unwrap().insert(
                PATH_FIRST.clone(),
                (vec![OTHER_TARGET.clone()], 1, "v1".to_string()),
            );
            server_1.files.lock().unwrap().insert(
                PATH_SECOND.clone(),
                (
                    vec![DUMMY_TARGET.clone(), OTHER_TARGET.clone()],
                    1,
                    "X".to_string(),
                ),
            );
        };
        let server_first_2 = server_first_1.clone();

        let server_2 = server.clone();
        let server_2_storage = storage.clone();
        let server_second_1 = move || {
            assert_eq!(server_2_storage.0.files.lock().unwrap().len(), 2);
            server_2.files.lock().unwrap().insert(
                PATH_FIRST.clone(),
                (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
            );
            server_2.files.lock().unwrap().remove(&*PATH_SECOND);
        };
        let server_second_2 = server_second_1.clone();

        let server_3 = server.clone();
        let server_3_storage = storage.clone();
        let server_3_rc_storage = rc_storage.clone();
        let server_third_1 = move || {
            // It may happen that the other fetcher is _right now_ doing a fetch.
            // This leads to a race condition:
            // - If the other fetcher is currently fetching, then the file will be inactive and
            //   dropped once its fetching ended.
            // - If there's no other fetching active, it'll immediately drop the file.
            let (runners, _) = server_3_rc_storage.run_id.runners_and_run_id();
            let (expected_files, expected_inactive) = if runners == 0 { (1, 0) } else { (2, 1) };
            assert_eq!(
                server_3_rc_storage.inactive.lock().unwrap().len(),
                expected_inactive
            );
            // one file should be expired by now
            assert_eq!(
                server_3_storage.0.files.lock().unwrap().len(),
                expected_files
            );
            server_3.files.lock().unwrap().clear();
        };
        let server_third_2 = server_third_1.clone();

        let fetcher_1 = SharedFetcher::new(
            DUMMY_TARGET.clone(),
            "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
        );
        let fetcher_2 = SharedFetcher::new(
            OTHER_TARGET.clone(),
            "ae588386-8464-43ba-bd3a-3e2d36b2c22c".to_string(),
        );
        let iteration = Arc::new(AtomicU32::new(0));
        let iteration_1 = iteration.clone();
        let iteration_2 = iteration.clone();
        let inner_fetcher_1 = unsafe { &*(&fetcher_1 as *const SharedFetcher) };
        let inner_fetcher_2 = unsafe { &*(&fetcher_2 as *const SharedFetcher) };
        join_all(vec![
            fetcher_1.run(
                rc_storage.clone(),
                Box::new(
                    move |fetched| match iteration_1.fetch_add(1, Ordering::SeqCst) {
                        i @ 0 | i @ 1 => {
                            assert_eq!(fetched.len(), 2);

                            if i == 1 {
                                server_first_1();
                            }
                        }
                        i @ 2 | i @ 3 => {
                            assert_eq!(fetched.len(), 1);
                            assert_eq!(fetched[0].store.data.lock().unwrap().contents, "X");

                            if i == 3 {
                                server_second_1();
                            }
                        }
                        i @ 4 | i @ 5 => {
                            assert_eq!(fetched.len(), 1);
                            assert_eq!(fetched[0].store.data.lock().unwrap().contents, "v2");

                            if i == 5 {
                                server_third_1();
                            }
                        }
                        6 => {
                            assert_eq!(fetched.len(), 0);

                            inner_fetcher_1.cancel();
                        }
                        _ => panic!("Unexpected"),
                    },
                ),
            ),
            fetcher_2.run(
                rc_storage,
                Box::new(
                    move |fetched| match iteration_2.fetch_add(1, Ordering::SeqCst) {
                        i @ 0 | i @ 1 => {
                            assert_eq!(fetched.len(), 1);
                            assert_eq!(fetched[0].store.data.lock().unwrap().contents, "v1");

                            if i == 1 {
                                server_first_2();
                            }
                        }
                        i @ 2 | i @ 3 => {
                            assert_eq!(fetched.len(), 2);

                            if i == 3 {
                                server_second_2();
                            }
                        }
                        i @ 4 | i @ 5 => {
                            assert_eq!(fetched.len(), 0);

                            if i == 5 {
                                server_third_2();
                            }

                            inner_fetcher_2.cancel();
                        }
                        _ => panic!("Unexpected"),
                    },
                ),
            ),
        ])
        .await;

        assert!(storage.0.files.lock().unwrap().is_empty());
    }
}
