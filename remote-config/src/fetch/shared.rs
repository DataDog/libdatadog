use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::error;
use crate::fetch::{ConfigFetcher, ConfigFetcherState, ConfigInvariants, FileStorage, OpaqueState};
use crate::{RemoteConfigPath, Target};
use tokio::time::sleep;
use tokio::select;

/// Fetcher which does a run-loop and carefully manages state around files, with the following
/// guarantees:
///  - A file at a given RemoteConfigPath will not be recreated as long as it exists
///    I.e. it will always be drop()'ed before recreation.
///  - It does not leak files which are no longer in use, i.e. it refcounts across all remote
///    config clients sharing the same RefcountingStorage.
///  - The state is always valid, i.e. there will be no intermittently expired files.
///
pub struct SharedFetcher {
    /// (env, service, version) tuple representing the basic remote config target
    pub target: Arc<Target>, // could be theoretically also Mutex<>ed if needed
    /// A unique runtime id. It must not be used by any other remote config client at the same time.
    /// Is allowed to be changed at any time.
    pub runtime_id: Arc<Mutex<String>>,
    /// Each fetcher must have an unique id. Defaults to a random UUID.
    pub client_id: String,
    cancellation: CancellationToken,
    /// Interval used if the remote server does not specify a refetch interval
    pub default_interval: AtomicU64,
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
    pub path: RemoteConfigPath,
    pub version: u64,
}

impl FileRefcountData {
    pub fn new(version: u64, path: RemoteConfigPath) -> Self {
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

    fn set_dropped_run_id(&self, val: u64) {
        self.refcount().dropped_run_id.store(val, Ordering::SeqCst)
    }

    fn get_dropped_run_id(&self) -> u64 {
        self.refcount().dropped_run_id.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct RunnersGeneration {
    val: AtomicU64,
}

/// Atomic structure to represent the exact amount of remote config fetching runners at a specific
/// point in time represented by the generation, an integer which is only ever incremented.
/// This data structure helps contain which inactive files are pending deletion.
impl RunnersGeneration {
    const RUN_ID_SHIFT: i32 = 20;

    /// Increments run_id and increments active runners. Returns first run_id to watch for.
    fn inc_runners(&self) -> u64 {
        (self.val.fetch_add((1 << Self::RUN_ID_SHIFT) + 1, Ordering::SeqCst) >> Self::RUN_ID_SHIFT) + 1
    }

    /// Increments run_id and decrements active runners. Returns last run_id to watch for.
    fn dec_runners(&self) -> u64 {
        self.val.fetch_add((1 << Self::RUN_ID_SHIFT) - 1, Ordering::SeqCst) >> Self::RUN_ID_SHIFT
    }

    /// Returns amount of active runners and current run_id.
    fn runners_and_run_id(&self) -> (u32, u64) {
        let val = self.val.load(Ordering::Acquire);
        ((val & ((1 << Self::RUN_ID_SHIFT) - 1)) as u32, val >> Self::RUN_ID_SHIFT)
    }
}

pub struct RefcountingStorage<S: FileStorage + Clone> where S::StoredFile: RefcountedFile {
    pub storage: S,
    state: Arc<ConfigFetcherState<S::StoredFile>>,
    /// Stores recently expired files. When a file refcount drops to zero, they're no longer sent
    /// via the remote config client. However, there may still be in-flight requests, with telling
    /// the remote config server that we know about these files. Thus, as long as these requests are
    /// being processed, we must retain the files, as these would not be resent, leaving us with a
    /// potentially incomplete configuration.
    inactive: Arc<Mutex<HashMap<RemoteConfigPath, Arc<S::StoredFile>>>>,
    /// times ConfigFetcher::<S>::fetch_once() is currently being run
    run_id: Arc<RunnersGeneration>,
}

impl<S: FileStorage + Clone> Clone for RefcountingStorage<S> where S::StoredFile: RefcountedFile {
    fn clone(&self) -> Self {
        RefcountingStorage {
            storage: self.storage.clone(),
            state: self.state.clone(),
            inactive: self.inactive.clone(),
            run_id: self.run_id.clone(),
        }
    }
}

impl<S: FileStorage + Clone> RefcountingStorage<S> where S::StoredFile: RefcountedFile {
    pub fn new(storage: S, mut state: ConfigFetcherState<S::StoredFile>) -> Self {
        state.expire_unused_files = true;
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
        expire_lock.expire_file(&file.refcount().path);
        drop(expire_lock); // early release
        let (runners, run_id) = self.run_id.runners_and_run_id();
        if runners > 0 {
            file.setref(runners);
            file.set_dropped_run_id(run_id);
            inactive.insert(file.refcount().path.clone(), file);
        }
    }

    pub fn invariants(&self) -> &ConfigInvariants {
        &self.state.invariants
    }
}

impl<S: FileStorage + Clone> FileStorage for RefcountingStorage<S> where S::StoredFile: RefcountedFile {
    type StoredFile = S::StoredFile;

    fn store(&self, version: u64, path: RemoteConfigPath, contents: Vec<u8>) -> anyhow::Result<Arc<Self::StoredFile>> {
        let mut inactive = self.inactive.lock().unwrap();
        if let Some(existing) = inactive.remove(&path) {
            if version <= existing.refcount().version {
                existing.set_dropped_run_id(0);
                existing.setref(0);
            } else {
                self.storage.update(&existing, version, contents)?;
            }
            Ok(existing)
        } else {
            self.storage.store(version, path, contents)
        }
    }

    fn update(&self, file: &Arc<Self::StoredFile>, version: u64, contents: Vec<u8>) -> anyhow::Result<()> {
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
            default_interval: AtomicU64::new(5_000_000_000),
        }
    }

    /// Runs.
    /// On successful fetches on_fetch() is called with the new configuration.
    /// Should not be called more than once.
    #[allow(clippy::type_complexity)]
    pub async fn run<S: FileStorage + Clone>(&self, storage: RefcountingStorage<S>, on_fetch: Box<dyn Send + Fn(&Vec<Arc<S::StoredFile>>) -> Option<String>>) where S::StoredFile: RefcountedFile {
        let state = storage.state.clone();
        let mut fetcher = ConfigFetcher::new(storage, state);

        let mut opaque_state = OpaqueState::default();

        let mut last_files: Vec<Arc<S::StoredFile>> = vec![];
        let mut last_error = None;

        loop {
            let first_run_id = fetcher.file_storage.run_id.inc_runners();

            let runtime_id = self.runtime_id.lock().unwrap().clone();
            let fetched = fetcher.fetch_once(runtime_id.as_str(), self.target.clone(), self.client_id.as_str(), last_error.take(), &mut opaque_state).await;

            let last_run_id = fetcher.file_storage.run_id.dec_runners();
            fetcher.file_storage.inactive.lock().unwrap().retain(|_, v| {
                (first_run_id..last_run_id).contains(&v.get_dropped_run_id()) && v.delref() == 0
            });

            match fetched {
                Ok(None) => { /* unchanged */ },
                Ok(Some(files)) => {
                    for file in files.iter() {
                        file.incref();
                    }

                    for file in last_files {
                        if file.delref() == 0 {
                            fetcher.file_storage.expire_file(file);
                        }
                    }

                    last_files = files;

                    last_error = on_fetch(&last_files);
                }
                Err(e) => error!("{:?}", e),
            }

            select! {
                _ = self.cancellation.cancelled() => { break; }
                _ = {
                    let mut ns = fetcher.interval.load(Ordering::Relaxed);
                    if ns == 0 {
                        ns = self.default_interval.load(Ordering::Relaxed);
                    }
                    sleep(Duration::from_nanos(ns))
                } => {}
            }
        }

        for file in last_files {
            if file.delref() == 0 {
                fetcher.file_storage.expire_file(file);
            }
        }
    }

    /// Note that due to the async logic, a cancellation does not guarantee a strict ordering:
    /// A final on_fetch call from within the run() method may happen after the cancellation.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}
