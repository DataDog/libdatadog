// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "agentless")]
use crate::fetch::AgentlessConfig;
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
        let ConfigOptions {
            invariants,
            products,
            capabilities,
        } = options;
        let state = Arc::new(ConfigFetcherState::with_client(invariants, http_client));
        let fetcher = ConfigFetcher::new(sink, state).await?;

        Ok(Self::from_fetcher(
            fetcher,
            target,
            runtime_id,
            products,
            capabilities,
        ))
    }

    #[cfg(feature = "agentless")]
    async fn new_agentless(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        agentless_config: AgentlessConfig,
        http_client: C,
    ) -> anyhow::Result<Self> {
        let ConfigOptions {
            mut invariants,
            products,
            capabilities,
        } = options;
        invariants.agentless = Some(agentless_config.clone());
        let state = Arc::new(ConfigFetcherState::with_client(invariants, http_client));
        let fetcher = ConfigFetcher::new_agentless(sink, state, agentless_config).await?;

        Ok(Self::from_fetcher(
            fetcher,
            target,
            runtime_id,
            products,
            capabilities,
        ))
    }

    pub fn new_no_agentless(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        http_client: C,
    ) -> Self {
        let ConfigOptions {
            invariants,
            products,
            capabilities,
        } = options;
        let state = Arc::new(ConfigFetcherState::with_client(invariants, http_client));
        let fetcher = ConfigFetcher::new_no_agentless(sink, state);

        Self::from_fetcher(fetcher, target, runtime_id, products, capabilities)
    }

    fn from_fetcher(
        fetcher: ConfigFetcher<S, C>,
        target: Target,
        runtime_id: String,
        products: Vec<RemoteConfigProduct>,
        capabilities: Vec<RemoteConfigCapabilities>,
    ) -> Self {
        SingleFetcher {
            fetcher,
            target: Arc::new(target),
            product_capabilities: ConfigProductCapabilities::new(products, capabilities),
            runtime_id,
            client_id: random_uuid_string(),
            client_state: ConfigClientState::default(),
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
        let fetcher = SingleFetcher::new(sink, target, runtime_id, options, http_client).await?;
        Ok(Self::from_fetcher(fetcher))
    }

    /// Creates a fetcher that only uses the agentless transport.
    ///
    /// # Errors
    /// Returns an error when TUF initialization fails.
    #[cfg(feature = "agentless")]
    pub async fn new_agentless(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        agentless_config: AgentlessConfig,
        http_client: C,
    ) -> anyhow::Result<Self> {
        let fetcher = SingleFetcher::new_agentless(
            sink,
            target,
            runtime_id,
            options,
            agentless_config,
            http_client,
        )
        .await?;
        Ok(Self::from_fetcher(fetcher))
    }

    pub fn new_no_agentless(
        sink: S,
        target: Target,
        runtime_id: String,
        options: ConfigOptions,
        http_client: C,
    ) -> Self {
        let fetcher =
            SingleFetcher::new_no_agentless(sink, target, runtime_id, options, http_client);
        Self::from_fetcher(fetcher)
    }

    fn from_fetcher(fetcher: SingleFetcher<S, C>) -> Self {
        SingleChangesFetcher {
            changes: ChangeTracker::default(),
            fetcher,
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

#[cfg(all(test, feature = "agentless", not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::file_storage::SimpleFileStorage;
    use bytes::Bytes;
    use libdd_capabilities::{HttpError, MaybeSend};
    use libdd_common::Endpoint;
    use std::future::Future;
    use std::sync::Mutex;

    #[derive(Clone, Debug, Default)]
    struct RecordingHttp {
        requests: Arc<Mutex<Vec<http::Uri>>>,
    }

    impl SleepCapability for RecordingHttp {
        fn new() -> Self {
            Self::default()
        }

        fn sleep(&self, _duration: Duration) -> impl Future<Output = ()> + MaybeSend {
            std::future::pending()
        }
    }

    impl HttpClientCapability for RecordingHttp {
        fn new_client() -> Self {
            Self::default()
        }

        fn new_without_connection_pooling() -> Self {
            Self::default()
        }

        #[allow(clippy::manual_async_fn)]
        fn request(
            &self,
            request: http::Request<Bytes>,
        ) -> impl Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend {
            let requests = self.requests.clone();
            async move {
                requests.lock().unwrap().push(request.uri().clone());
                Err(HttpError::Other(anyhow::anyhow!("request recorded")))
            }
        }
    }

    fn options(endpoint: Endpoint, agentless: Option<AgentlessConfig>) -> ConfigOptions {
        ConfigOptions {
            invariants: ConfigInvariants {
                language: "test".to_string(),
                tracer_version: "1.0.0".to_string(),
                endpoint,
                agentless,
            },
            products: vec![],
            capabilities: vec![],
        }
    }

    fn target() -> Target {
        Target::new(
            "service".to_string(),
            "env".to_string(),
            "1.0.0".to_string(),
            vec![],
            vec![],
        )
    }

    async fn create(
        options: ConfigOptions,
        agentless_config: AgentlessConfig,
        http_client: RecordingHttp,
    ) -> anyhow::Result<SingleChangesFetcher<SimpleFileStorage, RecordingHttp>> {
        SingleChangesFetcher::new_agentless(
            SimpleFileStorage::default(),
            target(),
            "runtime-id".to_string(),
            options,
            agentless_config,
            http_client,
        )
        .await
    }

    async fn assert_uses_endpoint(
        fetcher: &mut SingleChangesFetcher<SimpleFileStorage, RecordingHttp>,
        http_client: &RecordingHttp,
        endpoint: &Endpoint,
    ) {
        assert!(fetcher.fetch_changes::<Vec<u8>>().await.is_err());

        let requests = http_client.requests.lock().unwrap();
        assert!(!requests.is_empty());
        for request in requests.iter() {
            assert_eq!(request.scheme(), endpoint.url.scheme());
            assert_eq!(request.authority(), endpoint.url.authority());
        }
    }

    #[tokio::test]
    async fn new_agentless_uses_explicit_agentless_endpoint() {
        let endpoint = Endpoint::agentless("datadoghq.eu", "api-key".to_string())
            .expect("test endpoint is valid");
        let agentless = AgentlessConfig::new("host".to_string(), &endpoint)
            .expect("test agentless configuration is valid");
        let expected_endpoint = agentless.agentless_endpoint().clone();
        let http_client = RecordingHttp::default();
        let mut fetcher = create(
            options(Endpoint::default(), None),
            agentless,
            http_client.clone(),
        )
        .await
        .expect("agentless fetcher should initialize");

        assert_uses_endpoint(&mut fetcher, &http_client, &expected_endpoint).await;
    }

    #[tokio::test]
    async fn new_uses_configured_agentless_endpoint() {
        let endpoint = Endpoint::agentless("us5.datadoghq.com", "api-key".to_string())
            .expect("test endpoint is valid");
        let agentless = AgentlessConfig::new("host".to_string(), &endpoint)
            .expect("test agentless configuration is valid");
        let expected_endpoint = agentless.agentless_endpoint().clone();
        let http_client = RecordingHttp::default();
        let mut fetcher = SingleChangesFetcher::new(
            SimpleFileStorage::default(),
            target(),
            "runtime-id".to_string(),
            options(Endpoint::default(), Some(agentless)),
            http_client.clone(),
        )
        .await
        .expect("agentless fetcher should initialize");

        assert_uses_endpoint(&mut fetcher, &http_client, &expected_endpoint).await;
    }
}
