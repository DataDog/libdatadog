// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::{
    ConfigApplyState, ConfigClientState, ConfigFetcher, ConfigFetcherState, ConfigInvariants,
    FileStorage,
};
use crate::file_change_tracker::{Change, ChangeTracker, FilePath, UpdatedFiles};
use crate::{RemoteConfigPath, Target};
use std::sync::Arc;

/// Simple implementation
pub struct SingleFetcher<S: FileStorage> {
    fetcher: ConfigFetcher<S>,
    target: Arc<Target>,
    runtime_id: String,
    client_id: String,
    opaque_state: ConfigClientState,
}

impl<S: FileStorage> SingleFetcher<S> {
    pub fn new(sink: S, target: Target, runtime_id: String, invariants: ConfigInvariants) -> Self {
        SingleFetcher {
            fetcher: ConfigFetcher::new(sink, Arc::new(ConfigFetcherState::new(invariants))),
            target: Arc::new(target),
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
                self.target.clone(),
                self.client_id.as_str(),
                &mut self.opaque_state,
            )
            .await
    }

    pub fn get_client_id(&self) -> &String {
        &self.client_id
    }

    /// Sets the apply state on a stored file.
    pub fn set_config_state(&self, file: &RemoteConfigPath, state: ConfigApplyState) {
        self.fetcher.set_config_state(file, state)
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
    pub fn new(sink: S, target: Target, runtime_id: String, invariants: ConfigInvariants) -> Self {
        SingleChangesFetcher {
            changes: ChangeTracker::default(),
            fetcher: SingleFetcher::new(sink, target, runtime_id, invariants),
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
}
