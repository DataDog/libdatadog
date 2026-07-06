// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::{
    ConfigApplyState, ConfigClientState, ConfigFetcher, ConfigFetcherState, ConfigInvariants,
    ConfigProductCapabilities, FileStorage,
};
use crate::file_change_tracker::{Change, ChangeTracker, FilePath, UpdatedFiles};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use std::sync::Arc;

/// Simple implementation
pub struct SingleFetcher<S: FileStorage> {
    fetcher: ConfigFetcher<S>,
    target: Arc<Target>,
    product_capabilities: ConfigProductCapabilities,
    runtime_id: String,
    client_id: String,
    opaque_state: ConfigClientState,
}

#[derive(Clone, Debug)]
pub struct ConfigOptions {
    pub invariants: ConfigInvariants,
    pub products: Vec<RemoteConfigProduct>,
    pub capabilities: Vec<RemoteConfigCapabilities>,
}

impl<S: FileStorage> SingleFetcher<S> {
    pub fn new(sink: S, target: Target, runtime_id: String, options: ConfigOptions) -> Self {
        SingleFetcher {
            fetcher: ConfigFetcher::new(
                sink,
                Arc::new(ConfigFetcherState::new(options.invariants)),
            ),
            target: Arc::new(target),
            product_capabilities: ConfigProductCapabilities::new(
                options.products,
                options.capabilities,
            ),
            runtime_id,
            client_id: uuid::Uuid::new_v4().to_string(),
            opaque_state: ConfigClientState::default(),
        }
    }

    pub fn with_client_id(mut self, client_id: String) -> Self {
        self.client_id = client_id;
        self
    }

    /// Polls the current runtime config files.
    pub async fn fetch_once(&mut self) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        self.fetcher
            .fetch_once(
                self.runtime_id.as_str(),
                &self.target,
                &self.product_capabilities,
                self.client_id.as_str(),
                &mut self.opaque_state,
            )
            .await
    }

    pub fn get_client_id(&self) -> &String {
        &self.client_id
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
        self.opaque_state.set_extra_services(services);
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

pub struct SingleChangesFetcher<S: FileStorage>
where
    S::StoredFile: FilePath,
{
    changes: ChangeTracker<S::StoredFile>,
    pub fetcher: SingleFetcher<S>,
}

impl<S: FileStorage> SingleChangesFetcher<S>
where
    S::StoredFile: FilePath,
{
    pub fn new(sink: S, target: Target, runtime_id: String, options: ConfigOptions) -> Self {
        SingleChangesFetcher {
            changes: ChangeTracker::default(),
            fetcher: SingleFetcher::new(sink, target, runtime_id, options),
        }
    }

    pub fn with_client_id(mut self, client_id: String) -> Self {
        self.fetcher = self.fetcher.with_client_id(client_id);
        self
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

    pub fn get_client_id(&self) -> &String {
        self.fetcher.get_client_id()
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
