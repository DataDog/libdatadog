// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::{
    ConfigApplyState, ConfigClientState, ConfigFetcher, ConfigFetcherState, ConfigInvariants,
    ConfigProductCapabilities, Fetched, FileStorage, HeldFile, random_uuid_string,
};
use crate::file_change_tracker::{Change, changes};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::{Endpoint, MutexExt};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Simple implementation
pub struct SingleFetcher<S: FileStorage, C: HttpClientCapability + SleepCapability> {
    fetcher: ConfigFetcher<S, C>,
    target: Target,
    product_capabilities: ConfigProductCapabilities,
    runtime_id: String,
    client_id: String,
    client_state: ConfigClientState<S::StoredFile>,
    /// The files held before a rebind, which reset the client state. They are handed out as the
    /// previous files of the next fetch which changes anything, as if still held until then.
    carried: Vec<HeldFile<S::StoredFile>>,
}

#[derive(Clone, Debug)]
pub struct ConfigOptions {
    pub invariants: ConfigInvariants,
    pub products: Vec<RemoteConfigProduct>,
    pub capabilities: Vec<RemoteConfigCapabilities>,
}

impl<S: FileStorage, C: HttpClientCapability + SleepCapability> SingleFetcher<S, C> {
    pub async fn new(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        http_client: C,
    ) -> anyhow::Result<Self> {
        Ok(SingleFetcher {
            fetcher: ConfigFetcher::new(
                sink,
                Arc::new(ConfigFetcherState::with_client(
                    options.invariants,
                    http_client,
                )),
            )
            .await?,
            target,
            product_capabilities: ConfigProductCapabilities::new(
                options.products,
                options.capabilities,
            ),
            runtime_id,
            client_id: random_uuid_string(),
            client_state: ConfigClientState::default(),
            carried: vec![],
        })
    }

    pub fn new_no_agentless(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        http_client: C,
    ) -> Self {
        SingleFetcher {
            fetcher: ConfigFetcher::new_no_agentless(
                sink,
                Arc::new(ConfigFetcherState::with_client(
                    options.invariants,
                    http_client,
                )),
            ),
            target,
            product_capabilities: ConfigProductCapabilities::new(
                options.products,
                options.capabilities,
            ),
            runtime_id,
            client_id: random_uuid_string(),
            client_state: ConfigClientState::default(),
            carried: vec![],
        }
    }

    pub fn with_client_id(mut self, client_id: String) -> Self {
        self.client_id = client_id;
        self
    }

    /// Replaces the identity sent on subsequent polls without resetting client or file state.
    pub fn set_identity(&mut self, client_id: String, runtime_id: String, tags: Vec<String>) {
        self.client_id = client_id;
        self.runtime_id = runtime_id;
        self.target.tags = tags;
    }

    /// Switches to another target or Endpoint, keeping the known-files state
    pub async fn rebind(self, target: Target, endpoint: Option<Endpoint>) -> anyhow::Result<Self> {
        let SingleFetcher {
            fetcher,
            target: _,
            product_capabilities,
            runtime_id,
            client_id,
            client_state: previous_client_state,
            mut carried,
        } = self;
        let (file_storage, previous) = fetcher.into_parts();

        let mut invariants = previous.invariants.clone();
        if let Some(endpoint) = endpoint {
            invariants.endpoint = endpoint;
        }
        let mut state = ConfigFetcherState::with_client(invariants, previous.http_client.clone());
        state.expire_unused_files = previous.expire_unused_files;
        state.target_files_by_path = Mutex::new(std::mem::take(
            &mut *previous.target_files_by_path.lock_or_panic(),
        ));

        let mut client_state = ConfigClientState::default();
        carried.extend(previous_client_state.holdings.take());
        client_state.set_extra_services(previous_client_state.extra_services);

        Ok(SingleFetcher {
            fetcher: ConfigFetcher::new(file_storage, Arc::new(state)).await?,
            target,
            product_capabilities,
            runtime_id,
            client_id,
            client_state,
            carried,
        })
    }

    /// Polls the current runtime config files.
    pub async fn fetch_once(&mut self) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        Ok(self
            .fetch()
            .await?
            .map(|fetched| fetched.files.into_iter().map(|held| held.file).collect()))
    }

    /// Polls the current runtime config files, returning both the files held now and those held
    /// before. See [`ConfigFetcher::fetch`].
    pub async fn fetch(&mut self) -> anyhow::Result<Option<Fetched<S::StoredFile>>> {
        let fetched = self
            .fetcher
            .fetch(
                self.runtime_id.as_str(),
                &self.target,
                &self.product_capabilities,
                self.client_id.as_str(),
                &mut self.client_state,
            )
            .await?;
        Ok(fetched.map(|mut fetched| {
            if !self.carried.is_empty() {
                let mut previous = std::mem::take(&mut self.carried);
                previous.append(&mut fetched.previous);
                fetched.previous = previous;
            }
            fetched
        }))
    }

    pub fn get_client_id(&self) -> &str {
        &self.client_id
    }

    /// Returns the server-recommended interval before the next poll.
    /// In agentless mode this is updated after every successful fetch.
    /// In agent mode it returns the default of 5 seconds.
    pub fn get_refresh_interval(&self) -> Duration {
        self.client_state
            .server_recommended_refresh_interval()
            .unwrap_or(DEFAULT_REFRESH_INTERVAL)
    }

    /// Accesses the underlying file storage (the [`ConfigFetcher`]'s `file_storage`).
    pub fn file_storage(&self) -> &S {
        &self.fetcher.file_storage
    }

    /// Sets the apply state of whichever version of the file this client currently holds at
    /// `file`, reported with the following requests. An outcome set after a later fetch_once()
    /// applies to the version held then; use [`Self::set_config_state_for_version`] to have it
    /// discarded if that is not the version the outcome is for.
    pub fn set_config_state(&self, file: &RemoteConfigPath, state: ConfigApplyState) {
        self.client_state.set_current_config_state(file, state);
    }

    /// Sets the apply state of `version` of the file at `path`, reported with the following
    /// requests. Discarded, returning false, unless this client still holds that version.
    pub fn set_config_state_for_version(
        &self,
        path: &RemoteConfigPath,
        version: u64,
        state: ConfigApplyState,
    ) -> bool {
        self.client_state.set_config_state(path, version, state)
    }

    /// Update the set of services discovered at runtime
    /// Sent to the agent on each subsequent poll so it can route configs targeting those
    /// services to this client. Replace-semantics: the new vec fully overrides the previous one.
    pub fn set_extra_services(&mut self, services: Vec<String>) {
        self.client_state.set_extra_services(services);
    }

    /// Replace the set of subscribed products and capabilities.
    ///
    /// Hosts whose product/capability set changes at runtime (e.g. enabling ASM
    /// products on remote activation) call this before a subsequent `fetch_once`.
    pub fn set_product_capabilities(
        &mut self,
        products: Vec<RemoteConfigProduct>,
        capabilities: Vec<RemoteConfigCapabilities>,
    ) {
        self.product_capabilities = ConfigProductCapabilities::new(products, capabilities);
    }
}

pub struct SingleChangesFetcher<S: FileStorage, C: HttpClientCapability + SleepCapability> {
    pub fetcher: SingleFetcher<S, C>,
}

impl<S: FileStorage, C: HttpClientCapability + SleepCapability> SingleChangesFetcher<S, C> {
    pub async fn new(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        http_client: C,
    ) -> anyhow::Result<Self> {
        Ok(SingleChangesFetcher {
            fetcher: SingleFetcher::new(sink, target, runtime_id, options, http_client).await?,
        })
    }

    pub fn new_no_agentless(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        http_client: C,
    ) -> Self {
        SingleChangesFetcher {
            fetcher: SingleFetcher::new_no_agentless(
                sink,
                target,
                runtime_id,
                options,
                http_client,
            ),
        }
    }

    pub fn with_client_id(mut self, client_id: String) -> Self {
        self.fetcher = self.fetcher.with_client_id(client_id);
        self
    }

    /// See [`SingleFetcher::set_identity`].
    pub fn set_identity(&mut self, client_id: String, runtime_id: String, tags: Vec<String>) {
        self.fetcher.set_identity(client_id, runtime_id, tags);
    }

    /// Polls for new changes: the difference between the files held before and after the fetch.
    /// A new version of a file is a new stored file: it is reported as [`Change::Update`] along
    /// with the previous one.
    pub async fn fetch_changes(
        &mut self,
    ) -> anyhow::Result<Vec<Change<Arc<S::StoredFile>, Arc<S::StoredFile>>>> {
        Ok(match self.fetcher.fetch().await? {
            None => vec![],
            Some(Fetched { files, previous }) => changes(&previous, &files),
        })
    }

    pub fn get_client_id(&self) -> &str {
        self.fetcher.get_client_id()
    }

    /// Returns the interval before the next poll.
    /// See [`SingleFetcher::get_refresh_interval`].
    pub fn get_refresh_interval(&self) -> Duration {
        self.fetcher.get_refresh_interval()
    }

    /// Sets the apply state of `file`, the very stored file, reported with the following requests.
    /// Discarded if this client no longer holds it: an outcome for a file replaced by a new
    /// version never applies to the new one.
    pub fn set_config_state(&self, file: &S::StoredFile, state: ConfigApplyState) {
        self.fetcher
            .client_state
            .set_file_config_state(file, None, state);
    }

    /// Sets the apply state of `version` of `file`, the very stored file; discarded, returning
    /// false, unless this client still holds it at that version. Pass the version the outcome
    /// was computed for (e.g. [`crate::file_storage::RawFile::version`]).
    pub fn set_config_state_for_version(
        &self,
        file: &S::StoredFile,
        version: u64,
        state: ConfigApplyState,
    ) -> bool {
        self.fetcher
            .client_state
            .set_file_config_state(file, Some(version), state)
    }

    /// See [`SingleFetcher::set_extra_services`].
    pub fn set_extra_services(&mut self, services: Vec<String>) {
        self.fetcher.set_extra_services(services);
    }

    /// See [`SingleFetcher::set_product_capabilities`].
    pub fn set_product_capabilities(
        &mut self,
        products: Vec<RemoteConfigProduct>,
        capabilities: Vec<RemoteConfigCapabilities>,
    ) {
        self.fetcher
            .set_product_capabilities(products, capabilities);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::test_server::RemoteConfigServer;
    use crate::file_storage::SimpleFileStorage;
    use libdd_capabilities_impl::NativeCapabilities;

    /// The apply state set for the version held is reported with the next request; once the file
    /// was replaced by a new version, a late outcome for the previous one is discarded.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn apply_state_is_kept_for_the_version_held() {
        let server = RemoteConfigServer::spawn();
        let target = Target::new(
            "service".to_string(),
            "env".to_string(),
            "1.2.3".to_string(),
            vec![],
            vec![],
        );
        let path = RemoteConfigPath::parse("employee/APM_TRACING/state/config").unwrap();
        server.files.lock().unwrap().insert(
            path.clone(),
            (vec![Arc::new(target.clone())], 1, "v1".to_string()),
        );
        let mut fetcher = SingleFetcher::new_no_agentless(
            SimpleFileStorage::default(),
            target.clone(),
            "runtime-id".to_string(),
            server.dummy_options(),
            NativeCapabilities::new_periodic(),
        );

        fetcher.fetch_once().await.unwrap().unwrap();
        fetcher.set_config_state(&path, ConfigApplyState::Error("bad".to_string()));
        fetcher.fetch_once().await.unwrap();
        assert_eq!(server.reported_config_states(), [(1, 3, "bad".to_string())]);
        assert!(fetcher.set_config_state_for_version(
            &path,
            1,
            ConfigApplyState::Error("worse".to_string())
        ));
        fetcher.fetch_once().await.unwrap();
        assert_eq!(
            server.reported_config_states(),
            [(1, 3, "worse".to_string())]
        );

        server
            .files
            .lock()
            .unwrap()
            .insert(path.clone(), (vec![Arc::new(target)], 2, "v2".to_string()));
        fetcher.fetch_once().await.unwrap().unwrap();
        // The outcome of applying v1 arrives once the client already holds v2.
        assert!(!fetcher.set_config_state_for_version(
            &path,
            1,
            ConfigApplyState::Error("late".to_string())
        ));
        fetcher.fetch_once().await.unwrap();
        assert_eq!(
            server.reported_config_states(),
            [(2, 2, String::new())],
            "the outcome for v1 must not land on v2"
        );
    }

    /// The apply state set for a stored file lands on the holding of that very file: once it was
    /// replaced by a new version, an outcome for the previous file is discarded, while one for
    /// the current file is reported.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn apply_state_set_for_a_file_targets_its_holding() {
        let server = RemoteConfigServer::spawn();
        let target = Target::new(
            "service".to_string(),
            "env".to_string(),
            "1.2.3".to_string(),
            vec![],
            vec![],
        );
        let path = RemoteConfigPath::parse("employee/APM_TRACING/state/config").unwrap();
        server.files.lock().unwrap().insert(
            path.clone(),
            (vec![Arc::new(target.clone())], 1, "v1".to_string()),
        );
        let mut fetcher = SingleChangesFetcher::new_no_agentless(
            SimpleFileStorage::default(),
            target.clone(),
            "runtime-id".to_string(),
            server.dummy_options(),
            NativeCapabilities::new_periodic(),
        );

        let changes = fetcher.fetch_changes().await.unwrap();
        let [Change::Add(v1)] = changes.as_slice() else {
            panic!("expected v1 to be added");
        };
        let v1 = v1.clone();

        server
            .files
            .lock()
            .unwrap()
            .insert(path.clone(), (vec![Arc::new(target)], 2, "v2".to_string()));
        let changes = fetcher.fetch_changes().await.unwrap();
        let [Change::Update(v2, old)] = changes.as_slice() else {
            panic!("expected v1 to be updated to v2");
        };
        assert!(Arc::ptr_eq(old, &v1));
        let v2 = v2.clone();

        // The outcome of applying v1 arrives once the client already holds v2.
        fetcher.set_config_state(&v1, ConfigApplyState::Error("late".to_string()));
        assert!(!fetcher.set_config_state_for_version(
            &v1,
            1,
            ConfigApplyState::Error("late".to_string())
        ));
        assert!(fetcher.fetch_changes().await.unwrap().is_empty());
        assert_eq!(
            server.reported_config_states(),
            [(2, 2, String::new())],
            "the outcome for v1 must not land on v2"
        );

        fetcher.set_config_state(&v2, ConfigApplyState::Error("bad".to_string()));
        fetcher.fetch_changes().await.unwrap();
        assert_eq!(
            server.reported_config_states(),
            [(2, 3, "bad".to_string())],
            "the outcome for the file held is reported"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn updates_identity_without_resetting_client_state() {
        let server = RemoteConfigServer::spawn();
        let target = Target::new(
            "service".to_string(),
            "env".to_string(),
            "1.2.3".to_string(),
            vec!["runtime-id:old-runtime-id".to_string()],
            vec!["entrypoint.type:script".to_string()],
        );
        server.files.lock().unwrap().insert(
            RemoteConfigPath::parse("employee/APM_TRACING/identity/config").unwrap(),
            (vec![Arc::new(target.clone())], 1, "v1".to_string()),
        );
        let mut fetcher = SingleChangesFetcher::new_no_agentless(
            SimpleFileStorage::default(),
            target,
            "old-runtime-id".to_string(),
            server.dummy_options(),
            NativeCapabilities::new_periodic(),
        )
        .with_client_id("old-client-id".to_string());

        fetcher.fetch_changes().await.unwrap();
        fetcher.set_identity(
            "new-client-id".to_string(),
            "new-runtime-id".to_string(),
            vec!["runtime-id:new-runtime-id".to_string()],
        );
        fetcher.fetch_changes().await.unwrap();

        let request = server.last_request.lock().unwrap();
        let client = request.as_ref().unwrap().client.as_ref().unwrap();
        let tracer = client.client_tracer.as_ref().unwrap();
        assert_eq!(client.id, "new-client-id");
        assert_eq!(tracer.runtime_id, "new-runtime-id");
        assert_eq!(tracer.service, "service");
        assert_eq!(tracer.env, "env");
        assert_eq!(tracer.app_version, "1.2.3");
        assert_eq!(tracer.tags, ["runtime-id:new-runtime-id"]);
        assert_eq!(tracer.process_tags, ["entrypoint.type:script"]);
        assert_eq!(
            client.state.as_ref().unwrap().backend_client_state,
            b"some state"
        );
    }
}
