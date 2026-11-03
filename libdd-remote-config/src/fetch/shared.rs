// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::{
    ClientHoldings, ConfigApplyState, ConfigClientState, ConfigFetcher, ConfigFetcherState,
    ConfigProductCapabilities, Fetched, FileStorage, random_uuid_string,
};
use crate::{RemoteConfigPath, Target};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::MutexExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::select;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::error;

/// Fetcher which does a run-loop, one of possibly many clients sharing a [`ConfigFetcherState`],
/// with the following guarantees:
///  - A file is not recreated as long as it exists. A new version of a path is stored as a new
///    file, while targets which haven't fetched it yet keep the previous version alive.
///  - It does not leak files which are no longer in use: a file is alive exactly as long as a
///    client holds it, a request which advertised it is in flight, or a consumer keeps it.
///  - The state is always valid, i.e. there will be no intermittently expired files.
pub struct SharedFetcher<F> {
    /// (env, service, version) tuple representing the basic remote config target
    pub target: Arc<Target>, // could be theoretically also Mutex<>ed if needed
    /// A unique runtime id. It must not be used by any other remote config client at the same
    /// time. Is allowed to be changed at any time.
    pub runtime_id: Mutex<Arc<String>>,
    /// Each fetcher must have an unique id. Defaults to a random UUID.
    pub client_id: String,
    /// Products&Capabilities used to query the remote config server.
    pub product_capabilities: Mutex<Arc<ConfigProductCapabilities>>,
    cancellation: CancellationToken,
    /// Refetch interval in nanoseconds.
    pub interval: AtomicU64,
    /// The file versions this client holds, with their apply states. They own the files the
    /// client was last delivered.
    holdings: ClientHoldings<F>,
}

impl<F> SharedFetcher<F> {
    pub fn new(
        target: Arc<Target>,
        runtime_id: String,
        product_capabilities: ConfigProductCapabilities,
    ) -> Self {
        SharedFetcher {
            target,
            runtime_id: Mutex::new(Arc::new(runtime_id)),
            client_id: random_uuid_string(),
            product_capabilities: Mutex::new(Arc::new(product_capabilities)),
            cancellation: CancellationToken::new(),
            interval: AtomicU64::new(5_000_000_000),
            holdings: ClientHoldings::default(),
        }
    }

    /// Sets the apply state of `version` of the file at `path`, reported with the following
    /// requests. Discarded, returning false, unless this client still holds that version.
    pub fn set_config_state(
        &self,
        path: &RemoteConfigPath,
        version: u64,
        state: ConfigApplyState,
    ) -> bool {
        self.holdings.set(path, Some(version), state)
    }

    /// Sets the apply state of `file`, the very stored file, reported with the following
    /// requests. Discarded, returning false, unless this client still holds it.
    pub(crate) fn set_file_config_state(&self, file: &F, state: ConfigApplyState) -> bool {
        self.holdings.set_for_file(file, None, state)
    }

    /// Runs.
    /// On successful fetches on_fetch() is called with the new configuration. The files it was
    /// called with previously are released only after it returns, so whatever it published may
    /// keep naming them until then.
    /// Once the run-loop ends, on_finish() is called, before the files on_fetch() was last
    /// called with are released: it must stop publishing them.
    /// Should not be called more than once.
    /// `state` is shared by every client of its invariants.
    #[allow(clippy::type_complexity)]
    pub async fn run<S: FileStorage<StoredFile = F>, C: HttpClientCapability + SleepCapability>(
        &self,
        storage: S,
        state: Arc<ConfigFetcherState<F, C>>,
        on_fetch: Box<dyn Send + Fn(&Vec<Arc<F>>)>,
        on_finish: Box<dyn Send + FnOnce()>,
    ) {
        let mut fetcher = match ConfigFetcher::new(storage, state).await {
            Ok(f) => f,
            Err(e) => {
                error!("failed to create the fetcher: {:?}", e);
                on_finish();
                return;
            }
        };

        // The holdings own the files on_fetch() was last called with.
        let mut opaque_state = ConfigClientState::default();
        opaque_state.holdings = self.holdings.clone();

        loop {
            let runtime_id = self.runtime_id.lock_or_panic().clone();
            let product_capabilities = self.product_capabilities.lock_or_panic().clone();
            let fetched = fetcher
                .fetch(
                    runtime_id.as_str(),
                    &self.target,
                    &product_capabilities,
                    self.client_id.as_str(),
                    &mut opaque_state,
                )
                .await;

            match fetched {
                Ok(None) => {}
                Ok(Some(Fetched { files, previous })) => {
                    if !files.is_empty() || !previous.is_empty() {
                        // Publish the new files before releasing the previous ones: until
                        // on_fetch() returns, the published listing still names them.
                        on_fetch(&files.into_iter().map(|held| held.file).collect());
                    }
                    drop(previous);
                }
                Err(e) => {
                    error!("{:?}", e);
                }
            }

            if let Some(interval) = opaque_state.server_recommended_refresh_interval() {
                // Keep the run-loop interval in sync with the server-provided value.
                // If the interval in nanoseconds is greater than u64::MAX, pick the max
                // representable value. This is tolerable as u64::MAX nanoseconds
                // still represents 35584 years.
                self.interval.store(
                    interval.as_nanos().min(u64::MAX as u128) as u64,
                    Ordering::Relaxed,
                );
            }

            select! {
                _ = self.cancellation.cancelled() => { break; }
                _ = sleep(Duration::from_nanos(self.interval.load(Ordering::Relaxed))) => {}
            }
        }

        let held = self.holdings.take();
        on_finish();
        drop(held);
    }

    /// Note that due to the async logic, a cancellation does not guarantee a strict ordering:
    /// A final on_fetch call from within the run() method may happen after the cancellation.
    /// Cancelling from within on_fetch callback is always final.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Completes once cancel() was called.
    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::Target;
    use crate::fetch::ConfigInvariants;
    use crate::fetch::fetcher::tests::*;
    use crate::fetch::test_server::RemoteConfigServer;
    use futures::future::join_all;
    use libdd_capabilities_impl::NativeCapabilities;
    use std::sync::atomic::AtomicU32;
    use std::sync::{Arc, LazyLock};

    pub(crate) static OTHER_TARGET: LazyLock<Arc<Target>> = LazyLock::new(|| {
        Arc::new(Target::new(
            "other".to_string(),
            "env".to_string(),
            "7.8.9".to_string(),
            vec![],
            vec![],
        ))
    });

    /// A state shared by several clients, as SharedFetcher requires.
    pub(crate) fn shared_state(
        server: &RemoteConfigServer,
    ) -> Arc<ConfigFetcherState<PathStore, NativeCapabilities>> {
        Arc::new(ConfigFetcherState::with_client(
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        ))
    }

    /// Targets sharing a path see a new version at different times: each receives it with its own
    /// next fetch. Until then a target's listing keeps naming the version it has, so that version
    /// must stay intact when another target receives the new one.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn new_version_leaves_other_targets_their_file() {
        let server = RemoteConfigServer::spawn();
        let both = vec![DUMMY_TARGET.clone(), OTHER_TARGET.clone()];
        server
            .files
            .lock()
            .unwrap()
            .insert(PATH_FIRST.clone(), (both.clone(), 1, "v1".to_string()));

        let storage = Arc::new(Storage::default());
        let state = shared_state(&server);
        let mut fetcher = ConfigFetcher::new(storage.clone(), state.clone())
            .await
            .unwrap();
        let capabilities = server.dummy_product_capabilities();
        let mut state_a = ConfigClientState::default();
        let mut state_b = ConfigClientState::default();
        let runtime_id = "1b8c1a0c-4c7f-4d33-9e44-7c1f3d1bd2a1";

        let held_by_a = fetcher
            .fetch_once(runtime_id, &DUMMY_TARGET, &capabilities, "a", &mut state_a)
            .await
            .unwrap()
            .unwrap();
        fetcher
            .fetch_once(runtime_id, &OTHER_TARGET, &capabilities, "b", &mut state_b)
            .await
            .unwrap()
            .unwrap();

        // Applying v1 failed; the target holding v1 reports that until it gets v2.
        assert!(state_a.set_config_state(
            &PATH_FIRST,
            1,
            ConfigApplyState::Error("invalid data".to_string()),
        ));
        server
            .files
            .lock()
            .unwrap()
            .insert(PATH_FIRST.clone(), (both, 2, "v2".to_string()));
        let fetched_by_b = fetcher
            .fetch_once(runtime_id, &OTHER_TARGET, &capabilities, "b", &mut state_b)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched_by_b[0].data.lock().unwrap().contents, "v2");
        assert_eq!(
            held_by_a[0].data.lock().unwrap().contents,
            "v1",
            "the file the other target still lists was changed under it"
        );

        // The lagging target reports the version it holds, so it is sent the new one.
        let fetched = fetcher
            .fetch_once(runtime_id, &DUMMY_TARGET, &capabilities, "a", &mut state_a)
            .await
            .unwrap();
        let reported: Vec<(u64, u64, String)> = server
            .last_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .state
            .as_ref()
            .unwrap()
            .config_states
            .iter()
            .map(|state| (state.version, state.apply_state, state.apply_error.clone()))
            .collect();
        assert_eq!(
            reported,
            vec![(1, 3, "invalid data".to_string())],
            "the lagging target must report the state of the version it holds"
        );
        let fetched_by_a = fetched.expect("the lagging target must be sent the new version");
        assert!(Arc::ptr_eq(&fetched_by_a[0], &fetched_by_b[0]));
    }

    /// Two clients holding the same version each have their own apply state: the outcome one of
    /// them reports must not change what the other reports.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn apply_state_is_per_client() {
        let server = RemoteConfigServer::spawn();
        let both = vec![DUMMY_TARGET.clone(), OTHER_TARGET.clone()];
        server
            .files
            .lock()
            .unwrap()
            .insert(PATH_FIRST.clone(), (both, 1, "v1".to_string()));

        let storage = Arc::new(Storage::default());
        let state = shared_state(&server);
        let mut fetcher = ConfigFetcher::new(storage.clone(), state.clone())
            .await
            .unwrap();
        let capabilities = server.dummy_product_capabilities();
        let mut state_a = ConfigClientState::default();
        let mut state_b = ConfigClientState::default();
        let runtime_id = "1b8c1a0c-4c7f-4d33-9e44-7c1f3d1bd2a1";
        for (target, id, state) in [
            (&*DUMMY_TARGET, "a", &mut state_a),
            (&*OTHER_TARGET, "b", &mut state_b),
        ] {
            fetcher
                .fetch_once(runtime_id, target, &capabilities, id, state)
                .await
                .unwrap()
                .unwrap();
        }

        // Applying v1 failed in client a only.
        assert!(state_a.set_config_state(
            &PATH_FIRST,
            1,
            ConfigApplyState::Error("invalid data".to_string()),
        ));

        fetcher
            .fetch_once(runtime_id, &OTHER_TARGET, &capabilities, "b", &mut state_b)
            .await
            .unwrap();
        assert_eq!(
            server.reported_config_states(),
            [(1, 2, String::new())],
            "client b must not report the outcome of client a"
        );
        fetcher
            .fetch_once(runtime_id, &DUMMY_TARGET, &capabilities, "a", &mut state_a)
            .await
            .unwrap();
        assert_eq!(
            server.reported_config_states(),
            [(1, 3, "invalid data".to_string())]
        );
    }

    /// An outcome reported for a version the client no longer holds is discarded: it must not
    /// be applied to the version that replaced it.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn late_outcome_for_a_replaced_version_is_discarded() {
        let server = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let storage = Arc::new(Storage::default());
        let state = shared_state(&server);
        let mut fetcher = ConfigFetcher::new(storage.clone(), state.clone())
            .await
            .unwrap();
        let capabilities = server.dummy_product_capabilities();
        let mut state = ConfigClientState::default();
        let runtime_id = "1b8c1a0c-4c7f-4d33-9e44-7c1f3d1bd2a1";

        fetcher
            .fetch_once(runtime_id, &DUMMY_TARGET, &capabilities, "a", &mut state)
            .await
            .unwrap()
            .unwrap();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
        );
        fetcher
            .fetch_once(runtime_id, &DUMMY_TARGET, &capabilities, "a", &mut state)
            .await
            .unwrap()
            .unwrap();

        // The outcome of applying v1 arrives once the client already holds v2.
        assert!(!state.set_config_state(
            &PATH_FIRST,
            1,
            ConfigApplyState::Error("late".to_string()),
        ));

        fetcher
            .fetch_once(runtime_id, &DUMMY_TARGET, &capabilities, "a", &mut state)
            .await
            .unwrap();
        assert_eq!(
            server.reported_config_states(),
            [(2, 2, String::new())],
            "the outcome for v1 must not land on v2"
        );
    }

    /// The apply state set through a running SharedFetcher is that of its own holding, reported
    /// with its next request.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn shared_fetcher_reports_the_apply_state_set() {
        let server = RemoteConfigServer::spawn();
        let storage = Arc::new(Storage::default());
        let state = shared_state(&server);
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let fetcher = SharedFetcher::new(
            DUMMY_TARGET.clone(),
            "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
            server.dummy_product_capabilities(),
        );
        fetcher.interval.store(1000, Ordering::SeqCst);
        let iteration = AtomicU32::new(0);
        let inner_fetcher = unsafe { &*(&fetcher as *const SharedFetcher<_>) };
        let inner_server = server.clone();
        fetcher
            .run(
                storage.clone(),
                state.clone(),
                Box::new(move |_| match iteration.fetch_add(1, Ordering::SeqCst) {
                    0 => {
                        let error = || ConfigApplyState::Error("bad".to_string());
                        assert!(!inner_fetcher.set_config_state(&PATH_FIRST, 2, error()));
                        assert!(!inner_fetcher.set_config_state(&PATH_SECOND, 1, error()));
                        assert!(inner_fetcher.set_config_state(&PATH_FIRST, 1, error()));
                        inner_server.files.lock().unwrap().insert(
                            PATH_SECOND.clone(),
                            (vec![DUMMY_TARGET.clone()], 1, "X".to_string()),
                        );
                    }
                    1 => {
                        assert_eq!(
                            inner_server.reported_config_states(),
                            [(1, 3, "bad".to_string())]
                        );
                        inner_fetcher.cancel();
                    }
                    _ => panic!("Unexpected"),
                }),
                Box::new(|| {}),
            )
            .await;
        assert!(!fetcher.set_config_state(&PATH_FIRST, 1, ConfigApplyState::Acknowledged));
    }

    /// A target's listing names the files it was last delivered. Each of them must stay alive
    /// until the listing replacing it has been delivered, whether the path got a new version or
    /// was removed, and at shutdown until on_finish() withdrew the listing.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn previous_files_outlive_the_delivery_of_new_ones() {
        let server = RemoteConfigServer::spawn();
        let storage = Arc::new(Storage::default());
        let state = shared_state(&server);
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let fetcher = SharedFetcher::new(
            DUMMY_TARGET.clone(),
            "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
            server.dummy_product_capabilities(),
        );
        let iteration = AtomicU32::new(0);
        let previous: Arc<Mutex<Vec<std::sync::Weak<PathStore>>>> = Default::default();
        let finish_previous = previous.clone();
        let inner_fetcher = unsafe { &*(&fetcher as *const SharedFetcher<_>) };
        let inner_server = server.clone();
        server.files.lock().unwrap().insert(
            PATH_SECOND.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "X".to_string()),
        );
        fetcher
            .run(
                storage.clone(),
                state.clone(),
                Box::new(move |fetched| {
                    let i = iteration.fetch_add(1, Ordering::SeqCst);
                    let mut previous = previous.lock().unwrap();
                    assert!(
                        previous.iter().all(|f| f.strong_count() > 0),
                        "delivery {i}: a file the previous listing named was released before \
                         the new listing was delivered"
                    );
                    *previous = fetched.iter().map(Arc::downgrade).collect();
                    let mut files = inner_server.files.lock().unwrap();
                    let mut contents: Vec<String> = fetched
                        .iter()
                        .map(|f| f.data.lock().unwrap().contents.clone())
                        .collect();
                    contents.sort();
                    match i {
                        0 => {
                            assert_eq!(contents, ["X", "v1"]);
                            files.insert(
                                PATH_FIRST.clone(),
                                (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
                            );
                        }
                        1 => {
                            assert_eq!(contents, ["X", "v2"]);
                            files.remove(&*PATH_FIRST);
                        }
                        2 => {
                            // PATH_SECOND stays listed, to be released at shutdown.
                            assert_eq!(contents, ["X"]);
                            inner_fetcher.cancel();
                        }
                        _ => panic!("Unexpected"),
                    }
                }),
                Box::new(move || {
                    let previous = finish_previous.lock().unwrap();
                    assert!(!previous.is_empty());
                    assert!(
                        previous.iter().all(|f| f.strong_count() > 0),
                        "a file the last listing named was released before on_finish"
                    );
                }),
            )
            .await;

        assert!(storage.files.lock().unwrap().is_empty());
    }

    /// Stats, advertising (and dropping the snapshot, which may release a version) and storing
    /// (and releasing a version, whose Drop takes the storage's lock, as store() does) never wait
    /// on each other.
    #[tokio::test]
    async fn stats_and_expiration_do_not_deadlock() {
        let state = Arc::new(ConfigFetcherState::<PathStore, _>::with_client(
            ConfigInvariants {
                language: "php".into(),
                tracer_version: "test".into(),
                endpoint: libdd_common::Endpoint::from_slice("http://localhost:8126"),
                agentless: None,
            },
            NativeCapabilities::new_periodic(),
        ));
        let storage = Arc::new(Storage::default());
        let get_or_store = |state: &ConfigFetcherState<PathStore, _>,
                            storage: &Arc<Storage>,
                            path: &RemoteConfigPath| {
            let contents = b"config".to_vec();
            let meta = libdd_trace_protobuf::remoteconfig::TargetFileMeta {
                path: path.to_string(),
                length: contents.len() as i64,
                hashes: vec![],
            };
            let path = Arc::new(path.clone());
            state
                .registry
                .get_or_store(path.clone(), "hash".to_string(), meta, 1, || {
                    storage.store(1, path, contents)
                })
                .unwrap()
                .1
        };
        // Kept alive, so that advertising lists it.
        let _file = get_or_store(&state, &storage, &PATH_FIRST);
        let start = Arc::new(std::sync::Barrier::new(3));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let mut threads = Vec::new();
        for task in 0..3 {
            let state = state.clone();
            let storage = storage.clone();
            let start = start.clone();
            let done = done_tx.clone();
            threads.push(std::thread::spawn(move || {
                start.wait();
                for _ in 0..if cfg!(miri) { 100 } else { 100_000 } {
                    match task {
                        0 => drop(state.stats()),
                        1 => drop(state.registry.advertise()),
                        // Like a response storing a new version which nothing holds afterwards.
                        _ => drop(get_or_store(&state, &storage, &PATH_SECOND)),
                    }
                }
                done.send(()).unwrap();
            }));
        }
        for _ in 0..3 {
            done_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("stats and expiration deadlocked");
        }
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_single_fetcher() {
        let server = RemoteConfigServer::spawn();
        let storage = Arc::new(Storage::default());
        let state = shared_state(&server);

        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let fetcher = SharedFetcher::new(
            DUMMY_TARGET.clone(),
            "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
            server.dummy_product_capabilities(),
        );
        let iteration = AtomicU32::new(0);
        let inner_fetcher = unsafe { &*(&fetcher as *const SharedFetcher<_>) };
        let inner_storage = storage.clone();
        fetcher
            .run(
                storage.clone(),
                state.clone(),
                Box::new(
                    move |fetched| match iteration.fetch_add(1, Ordering::SeqCst) {
                        0 => {
                            assert_eq!(fetched.len(), 1);
                            assert_eq!(fetched[0].data.lock().unwrap().contents, "v1");

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
                            // The removed file is released once this listing is published.
                            assert_eq!(inner_storage.files.lock().unwrap().len(), 2);
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
                Box::new(|| {}),
            )
            .await;

        assert!(storage.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_parallel_fetchers() {
        let server = RemoteConfigServer::spawn();
        let storage = Arc::new(Storage::default());
        let state = shared_state(&server);

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
            assert_eq!(server_1_storage.files.lock().unwrap().len(), 2);
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
            assert_eq!(server_2_storage.files.lock().unwrap().len(), 2);
            server_2.files.lock().unwrap().insert(
                PATH_FIRST.clone(),
                (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
            );
            server_2.files.lock().unwrap().remove(&*PATH_SECOND);
        };
        let server_second_2 = server_second_1.clone();

        let server_3 = server.clone();
        let server_3_storage = storage.clone();
        let server_3_state = state.clone();
        let server_third_1 = move || {
            // Called while the target delivering second publishes its new files. Its previous
            // files, PATH_SECOND among them as both targets held it, are released only after
            // that, so they are still alive. The target which delivered first released its
            // previous files once it published, and stopped: it has no request in flight, and
            // the request of the target delivering now was processed. So nothing is alive only
            // for an in-flight request.
            assert_eq!(server_3_state.stats().inactive_files, 0);
            assert_eq!(server_3_storage.files.lock().unwrap().len(), 2);
            server_3.files.lock().unwrap().clear();
        };
        let server_third_2 = server_third_1.clone();

        let fetcher_1 = SharedFetcher::new(
            DUMMY_TARGET.clone(),
            "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
            server.dummy_product_capabilities(),
        );
        let fetcher_2 = SharedFetcher::new(
            OTHER_TARGET.clone(),
            "ae588386-8464-43ba-bd3a-3e2d36b2c22c".to_string(),
            server.dummy_product_capabilities(),
        );
        let iteration = Arc::new(AtomicU32::new(0));
        let iteration_1 = iteration.clone();
        let iteration_2 = iteration.clone();
        let inner_fetcher_1 = unsafe { &*(&fetcher_1 as *const SharedFetcher<_>) };
        let inner_fetcher_2 = unsafe { &*(&fetcher_2 as *const SharedFetcher<_>) };
        join_all(vec![
            fetcher_1.run(
                storage.clone(),
                state.clone(),
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
                            assert_eq!(fetched[0].data.lock().unwrap().contents, "X");

                            if i == 3 {
                                server_second_1();
                            }
                        }
                        i @ 4 | i @ 5 => {
                            assert_eq!(fetched.len(), 1);
                            assert_eq!(fetched[0].data.lock().unwrap().contents, "v2");

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
                Box::new(|| {}),
            ),
            fetcher_2.run(
                storage.clone(),
                state.clone(),
                Box::new(
                    move |fetched| match iteration_2.fetch_add(1, Ordering::SeqCst) {
                        i @ 0 | i @ 1 => {
                            assert_eq!(fetched.len(), 1);
                            assert_eq!(fetched[0].data.lock().unwrap().contents, "v1");

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
                Box::new(|| {}),
            ),
        ])
        .await;

        assert!(storage.files.lock().unwrap().is_empty());
    }

    /// The stats count every live version of a file, not just the paths with a stored file: a
    /// replaced version stays alive while a listing still names it.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn stats_count_live_file_versions() {
        let server = RemoteConfigServer::spawn();
        let storage = Arc::new(Storage::default());
        let state = shared_state(&server);
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let fetcher = SharedFetcher::new(
            DUMMY_TARGET.clone(),
            "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
            server.dummy_product_capabilities(),
        );
        fetcher.interval.store(1000, Ordering::SeqCst);
        let iteration = AtomicU32::new(0);
        let inner_fetcher = unsafe { &*(&fetcher as *const SharedFetcher<_>) };
        let inner_server = server.clone();
        let inner_state = state.clone();
        fetcher
            .run(
                storage.clone(),
                state.clone(),
                Box::new(move |_| {
                    let stats = inner_state.stats();
                    match iteration.fetch_add(1, Ordering::SeqCst) {
                        0 => {
                            assert_eq!((stats.fetcher.active_files, stats.live_files), (1, 1));
                            inner_server.files.lock().unwrap().insert(
                                PATH_FIRST.clone(),
                                (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
                            );
                        }
                        1 => {
                            // v1 is released once this listing is published.
                            assert_eq!((stats.fetcher.active_files, stats.live_files), (1, 2));
                            assert_eq!(stats.inactive_files, 0);
                            inner_fetcher.cancel();
                        }
                        _ => panic!("Unexpected"),
                    }
                }),
                Box::new(|| {}),
            )
            .await;

        let stats = state.stats();
        assert_eq!((stats.live_files, stats.inactive_files), (0, 0));
    }
}
