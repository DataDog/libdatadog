// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::{
    random_uuid_string, ConfigApplyState, ConfigClientState, ConfigFetcher, ConfigFetcherState,
    ConfigInvariants, ConfigProductCapabilities, FileStorage,
};
use crate::file_change_tracker::{Change, ChangeTracker, FilePath, UpdatedFiles};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Simple implementation
pub struct SingleFetcher<S: FileStorage, C: HttpClientCapability + SleepCapability> {
    fetcher: ConfigFetcher<S, C>,
    target: Arc<Target>,
    product_capabilities: ConfigProductCapabilities,
    runtime_id: String,
    client_id: String,
    client_state: ConfigClientState,
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
            target: Arc::new(target),
            product_capabilities: ConfigProductCapabilities::new(
                options.products,
                options.capabilities,
            ),
            runtime_id,
            client_id: random_uuid_string(),
            client_state: ConfigClientState::default(),
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
            target: Arc::new(target),
            product_capabilities: ConfigProductCapabilities::new(
                options.products,
                options.capabilities,
            ),
            runtime_id,
            client_id: random_uuid_string(),
            client_state: ConfigClientState::default(),
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
        Arc::make_mut(&mut self.target).tags = tags;
    }

    /// Polls the current runtime config files.
    pub async fn fetch_once(&mut self) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        self.fetcher
            .fetch_once(
                self.runtime_id.as_str(),
                &self.target,
                &self.product_capabilities,
                self.client_id.as_str(),
                &mut self.client_state,
            )
            .await
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

    /// Sets the apply state on a stored file.
    pub fn set_config_state(&self, file: &RemoteConfigPath, state: ConfigApplyState) {
        self.fetcher.set_config_state(file, state)
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

pub struct SingleChangesFetcher<S: FileStorage, C: HttpClientCapability + SleepCapability>
where
    S::StoredFile: FilePath,
{
    changes: ChangeTracker<S::StoredFile>,
    pub fetcher: SingleFetcher<S, C>,
}

impl<S: FileStorage, C: HttpClientCapability + SleepCapability> SingleChangesFetcher<S, C>
where
    S::StoredFile: FilePath,
{
    pub async fn new(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        http_client: C,
    ) -> anyhow::Result<Self> {
        Ok(SingleChangesFetcher {
            changes: ChangeTracker::default(),
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
            changes: ChangeTracker::default(),
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

    /// Polls for new changes
    pub async fn fetch_changes<R>(&mut self) -> anyhow::Result<Vec<Change<Arc<S::StoredFile>, R>>>
    where
        S: UpdatedFiles<S::StoredFile, R>,
    {
        Ok(match self.fetcher.fetch_once().await? {
            None => vec![],
            Some(files) => self
                .changes
                .get_changes(files, self.fetcher.fetcher.file_storage.updated()),
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

    /// Sets the apply state on a stored file.
    pub fn set_config_state(&self, file: &S::StoredFile, state: ConfigApplyState) {
        self.fetcher.set_config_state(file.path(), state)
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
            NativeCapabilities::new_without_connection_pooling(),
        )
        .with_client_id("old-client-id".to_string());

        fetcher.fetch_changes::<Vec<u8>>().await.unwrap();
        fetcher.set_identity(
            "new-client-id".to_string(),
            "new-runtime-id".to_string(),
            vec!["runtime-id:new-runtime-id".to_string()],
        );
        fetcher.fetch_changes::<Vec<u8>>().await.unwrap();

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
