use crate::fetch::{ConfigFetcher, ConfigFetcherState, ConfigInvariants, FileStorage, OpaqueState};
use crate::Target;
use std::sync::Arc;

pub struct SingleFetcher<S: FileStorage> {
    fetcher: ConfigFetcher<S>,
    target: Arc<Target>,
    runtime_id: String,
    pub config_id: String,
    pub last_error: Option<String>,
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
}
