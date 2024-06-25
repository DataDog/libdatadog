// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::{ConfigFetcher, ConfigFetcherState, ConfigInvariants, FileStorage, OpaqueState};
use crate::file_change_tracker::{Change, ChangeTracker, FilePath, UpdatedFiles};
use crate::Target;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Simple implementation
pub struct SingleFetcher<S: FileStorage> {
    fetcher: ConfigFetcher<S>,
    target: Arc<Target>,
    runtime_id: String,
    config_id: String,
    last_error: Option<String>,
    opaque_state: OpaqueState,
}

impl<S: FileStorage> SingleFetcher<S> {
    pub fn new(sink: S, target: Target, runtime_id: String, invariants: ConfigInvariants) -> Self {
        SingleFetcher {
            fetcher: ConfigFetcher::new(sink, Arc::new(ConfigFetcherState::new(invariants))),
            target: Arc::new(target),
            runtime_id,
            config_id: uuid::Uuid::new_v4().to_string(),
            last_error: None,
            opaque_state: OpaqueState::default(),
        }
    }

    pub fn with_config_id(mut self, config_id: String) -> Self {
        self.config_id = config_id;
        self
    }

    /// Polls the current runtime config files.
    pub async fn fetch_once(&mut self) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        self.fetcher
            .fetch_once(
                self.runtime_id.as_str(),
                self.target.clone(),
                self.config_id.as_str(),
                self.last_error.take(),
                &mut self.opaque_state,
            )
            .await
    }

    /// Timeout after which to report failure, in milliseconds.
    pub fn set_timeout(&self, milliseconds: u32) {
        self.fetcher.timeout.store(milliseconds, Ordering::Relaxed);
    }

    /// Collected interval. May be zero if not provided by the remote config server or fetched yet.
    /// Given in nanoseconds.
    pub fn get_interval(&self) -> u64 {
        self.fetcher.interval.load(Ordering::Relaxed)
    }

    /// Sets the error to be reported to the backend.
    pub fn set_last_error(&mut self, error: String) {
        self.last_error = Some(error);
    }

    pub fn get_config_id(&self) -> &String {
        &self.config_id
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

    pub fn with_config_id(mut self, config_id: String) -> Self {
        self.fetcher = self.fetcher.with_config_id(config_id);
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

    /// Timeout after which to report failure, in milliseconds.
    pub fn set_timeout(&self, milliseconds: u32) {
        self.fetcher.set_timeout(milliseconds)
    }

    /// Collected interval. May be zero if not provided by the remote config server or fetched yet.
    /// Given in nanoseconds.
    pub fn get_interval(&self) -> u64 {
        self.fetcher.get_interval()
    }

    /// Sets the error to be reported to the backend.
    pub fn set_last_error(&mut self, error: String) {
        self.fetcher.set_last_error(error);
    }

    pub fn get_config_id(&self) -> &String {
        self.fetcher.get_config_id()
    }
}
