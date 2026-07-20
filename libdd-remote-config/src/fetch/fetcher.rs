// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "agentless")]
use super::agentless;

use crate::targets::{Root, TargetsList};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use base64::Engine;
use hashbrown::HashMap;
use http::uri::PathAndQuery;
use http::StatusCode;
use http_body_util::BodyExt;
use libdd_common::{http_common, Endpoint, MutexExt};
use libdd_trace_protobuf::remoteconfig::{
    ClientGetConfigsRequest, ClientGetConfigsResponse, ClientState, ClientTracer, ConfigState,
    TargetFileHash, TargetFileMeta,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashSet;
use std::ops::Add;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tracing::{debug, trace, warn};

/// Manages config files.
/// Presents store() and update() operations.
/// It is recommended to minimize the overhead of these operations as they will be invoked while
/// a lock across all ConfigFetchers referencing the same ConfigFetcherState is held.
pub trait FileStorage {
    type StoredFile;

    /// A new, currently unknown file was received.
    fn store(
        &self,
        version: u64,
        path: Arc<RemoteConfigPath>,
        contents: Vec<u8>,
    ) -> anyhow::Result<Arc<Self::StoredFile>>;

    /// A file at a given path was updated (new contents).
    fn update(
        &self,
        file: &Arc<Self::StoredFile>,
        version: u64,
        contents: Vec<u8>,
    ) -> anyhow::Result<()>;
}

/// Fundamental configuration of the RC client, which always must be set.
#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub struct ConfigInvariants {
    pub language: String,
    pub tracer_version: String,
    pub endpoint: Endpoint,
    #[cfg(feature = "agentless")]
    /// Enables and configures agentless mode. If some the fetcher will
    /// talk directly to the RC backend
    pub agentless: Option<agentless::AgentlessConfig>,
    #[cfg(not(feature = "agentless"))]
    pub agentless: Option<std::convert::Infallible>,
}

impl ConfigInvariants {
    pub fn agentless_enabled(&self) -> bool {
        self.agentless.is_some()
    }
}

pub(crate) struct StoredTargetFile<S> {
    pub(crate) hash: String,
    pub(crate) handle: Arc<S>,
    pub(crate) state: ConfigState,
    pub(crate) meta: TargetFileMeta,
    pub(crate) expiring: bool,
}

pub enum ConfigApplyState {
    Unacknowledged,
    Acknowledged,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ConfigProductCapabilities {
    products: Vec<RemoteConfigProduct>,
    capabilities: Vec<RemoteConfigCapabilities>,
    encoded_capabilities: Vec<u8>,
}

impl ConfigProductCapabilities {
    pub fn new(
        products: Vec<RemoteConfigProduct>,
        capabilities: Vec<RemoteConfigCapabilities>,
    ) -> Self {
        ConfigProductCapabilities {
            encoded_capabilities: Self::encode_capatibilites(&capabilities),
            products,
            capabilities,
        }
    }

    fn encode_capatibilites(capabilities: &[RemoteConfigCapabilities]) -> Vec<u8> {
        let capability_len = capabilities
            .iter()
            .map(|c| *c as usize / 8 + 1)
            .max()
            .unwrap_or(0);
        let mut encoded_capabilities = vec![0; capability_len];
        for capability in capabilities.iter().map(|c| *c as usize) {
            encoded_capabilities[capability_len - (capability >> 3) - 1] |= 1 << (capability & 7);
        }
        encoded_capabilities
    }

    pub fn into_parts(self) -> (Vec<RemoteConfigProduct>, Vec<RemoteConfigCapabilities>) {
        (self.products, self.capabilities)
    }
}

pub struct ConfigFetcherState<S> {
    pub(crate) target_files_by_path: Mutex<HashMap<Arc<RemoteConfigPath>, StoredTargetFile<S>>>,
    pub invariants: ConfigInvariants,
    endpoint: Endpoint,
    pub expire_unused_files: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ConfigFetcherStateStats {
    pub active_files: u32,
}

impl Add for ConfigFetcherStateStats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        ConfigFetcherStateStats {
            active_files: self.active_files + rhs.active_files,
        }
    }
}

pub struct ConfigFetcherFilesLock<'a, S> {
    inner: MutexGuard<'a, HashMap<Arc<RemoteConfigPath>, StoredTargetFile<S>>>,
}

impl<S> ConfigFetcherFilesLock<'_, S> {
    /// Actually remove the file from the known files.
    /// It may only be expired if already marked as expiring.
    pub fn expire_file(&mut self, path: &RemoteConfigPath) {
        if let Some(target_file) = self.inner.get(path) {
            if !target_file.expiring {
                return;
            }
        } else {
            return;
        }
        self.inner.remove(path);
    }

    /// Stop advertising the file as known. It's the predecessor to expire_file().
    pub fn mark_expiring(&mut self, path: &RemoteConfigPath) {
        if let Some(target_file) = self.inner.get_mut(path) {
            target_file.expiring = true;
        }
    }
}

impl<S> ConfigFetcherState<S> {
    pub fn new(invariants: ConfigInvariants) -> Self {
        let (endpoint, agentless) = match &invariants.agentless {
            Some(agentless_cfg) => {
                #[cfg(feature = "agentless")]
                match (
                    agentless::make_agentless_configs_endpoint(&invariants.endpoint),
                    agentless_cfg.hostname.is_empty(),
                ) {
                    (Some(e), false) => (e, Some(agentless_cfg.clone())),
                    (Some(_), true) => {
                        warn!("rc_config_fetcher: agentless enabled but the hostname is empty. Downgrading to agent endpoint");
                        (make_agent_configs_endpoint(&invariants.endpoint), None)
                    }
                    (None, _) => {
                        warn!("rc_config_fetcher: agentless enabled but the endpoint is invalid. Downgrading to agent endpoint");
                        (make_agent_configs_endpoint(&invariants.endpoint), None)
                    }
                }

                #[cfg(not(feature = "agentless"))]
                match *agentless_cfg {}
            }
            None => (make_agent_configs_endpoint(&invariants.endpoint), None),
        };
        ConfigFetcherState {
            target_files_by_path: Default::default(),
            endpoint,
            invariants: ConfigInvariants {
                agentless,
                ..invariants
            },
            expire_unused_files: true,
        }
    }

    /// To remove unused remote files manually. Must not be called when auto expiration is active.
    /// Note: careful attention must be paid when using this API in order to not deadlock:
    /// - This files_lock() must always be called prior to locking any data structure locked within
    ///   FileStorage::store().
    /// - Also, files_lock() must not be called from within FileStorage::store().
    pub fn files_lock(&self) -> ConfigFetcherFilesLock<'_, S> {
        assert!(!self.expire_unused_files);
        ConfigFetcherFilesLock {
            inner: self.target_files_by_path.lock_or_panic(),
        }
    }

    /// Sets the apply state on a stored file.
    pub fn set_config_state(&self, file: &RemoteConfigPath, state: ConfigApplyState) {
        if let Some(target_file) = self.target_files_by_path.lock_or_panic().get_mut(file) {
            match state {
                ConfigApplyState::Unacknowledged => {
                    target_file.state.apply_state = 1;
                    target_file.state.apply_error = "".to_string();
                }
                ConfigApplyState::Acknowledged => {
                    target_file.state.apply_state = 2;
                    target_file.state.apply_error = "".to_string();
                }
                ConfigApplyState::Error(error) => {
                    target_file.state.apply_state = 3;
                    target_file.state.apply_error = error;
                }
            }
        }
    }

    pub fn stats(&self) -> ConfigFetcherStateStats {
        ConfigFetcherStateStats {
            active_files: self.target_files_by_path.lock_or_panic().len() as u32,
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum FetcherMode {
    Agent,
    #[cfg(feature = "agentless")]
    Agentless(agentless::NativeAgentlessFetcher),
}

pub struct ConfigFetcher<S: FileStorage> {
    pub file_storage: S,
    state: Arc<ConfigFetcherState<S::StoredFile>>,
    mode: FetcherMode,
}

pub struct ConfigClientState {
    opaque_backend_state: Vec<u8>,
    last_config_paths: HashSet<RemoteConfigPath>,
    targets_version: u64,
    root_version: u64,
    last_error: Option<String>,
    /// Services discovered at runtime. Sent to the agent on each poll so it can route configs
    /// targeting those services to this client. Updated out-of-band by the consumer
    extra_services: Vec<String>,
    /// Server-recommended interval between consecutive polls.
    refresh_interval: Option<Duration>,
}

impl Default for ConfigClientState {
    fn default() -> Self {
        ConfigClientState {
            opaque_backend_state: vec![],
            last_config_paths: Default::default(),
            targets_version: 0,
            root_version: 1,
            last_error: None,
            extra_services: vec![],
            refresh_interval: None,
        }
    }
}

impl ConfigClientState {
    pub fn set_extra_services(&mut self, services: Vec<String>) {
        self.extra_services = services;
    }

    pub fn server_recommended_refresh_interval(&self) -> Option<Duration> {
        self.refresh_interval
    }
}

impl<S: FileStorage> ConfigFetcher<S> {
    /// Create a new config fetcher
    /// This is guaranteed to be immediate (no await point) if `state.invariants.agentless_enabled`
    /// is false
    pub async fn new(
        file_storage: S,
        state: Arc<ConfigFetcherState<S::StoredFile>>,
    ) -> anyhow::Result<Self> {
        #[cfg(feature = "agentless")]
        let mode: FetcherMode = match &state.invariants.agentless {
            Some(agentless_cfg) => FetcherMode::Agentless(
                agentless::NativeAgentlessFetcher::new(
                    agentless_cfg.clone(),
                    state.endpoint.clone(),
                )
                .await?,
            ),
            None => FetcherMode::Agent,
        };
        #[cfg(not(feature = "agentless"))]
        let mode: FetcherMode = FetcherMode::Agent;

        Ok(ConfigFetcher {
            file_storage,
            state,
            mode,
        })
    }

    /// Sets the apply state on a stored file.
    pub fn set_config_state(&self, file: &RemoteConfigPath, state: ConfigApplyState) {
        self.state.set_config_state(file, state)
    }

    fn build_config_request(
        &self,
        runtime_id: &str,
        target: &Target,
        product_capabilities: &ConfigProductCapabilities,
        client_id: &str,
        client_state: &ConfigClientState,
    ) -> ClientGetConfigsRequest {
        let Target {
            service,
            env,
            app_version,
            tags,
            process_tags,
        } = (*target).clone();

        let mut cached_target_files = vec![];
        let mut config_states = vec![];

        {
            let target_files = self.state.target_files_by_path.lock_or_panic();
            for StoredTargetFile { meta, expiring, .. } in target_files.values() {
                if !expiring {
                    cached_target_files.push(meta.clone());
                }
            }

            for config in client_state.last_config_paths.iter() {
                if let Some(StoredTargetFile { state, .. }) = target_files.get(config) {
                    config_states.push(state.clone());
                }
            }
        }
        let extra_services = client_state.extra_services.clone();

        ClientGetConfigsRequest {
            client: Some(libdd_trace_protobuf::remoteconfig::Client {
                state: Some(ClientState {
                    root_version: client_state.root_version,
                    targets_version: client_state.targets_version,
                    config_states,
                    has_error: client_state.last_error.is_some(),
                    error: client_state.last_error.clone().unwrap_or_default(),
                    backend_client_state: client_state.opaque_backend_state.clone(),
                }),
                id: client_id.into(),
                products: product_capabilities
                    .products
                    .iter()
                    .map(|p| p.to_string())
                    .collect(),
                is_tracer: true,
                client_tracer: Some(ClientTracer {
                    runtime_id: runtime_id.to_string(),
                    language: self.state.invariants.language.to_string(),
                    tracer_version: self.state.invariants.tracer_version.clone(),
                    service,
                    extra_services,
                    env,
                    app_version,
                    tags,
                    process_tags,
                    container_tags: vec![],
                }),
                is_agent: false,
                client_agent: None,
                last_seen: 0,
                capabilities: product_capabilities.encoded_capabilities.clone(),
                is_updater: false,
                client_updater: None,
            }),
            cached_target_files,
        }
    }

    async fn fetch_agent(
        &mut self,
        config_req: ClientGetConfigsRequest,
        target: &Target,
        client_state: &mut ConfigClientState,
    ) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        trace!("Submitting remote config request: {config_req:?}");
        let req = self
            .state
            .endpoint
            .to_request_builder(concat!("Libdatadog/", env!("CARGO_PKG_VERSION")))?
            .method(http::Method::POST)
            .header(
                http::header::CONTENT_TYPE,
                libdd_common::header::APPLICATION_JSON,
            )
            .body(http_common::Body::from(serde_json::to_string(&config_req)?))?;
        let response = tokio::time::timeout(
            Duration::from_millis(self.state.endpoint.timeout_ms),
            http_common::new_default_client().request(req),
        )
        .await
        .map_err(|e| anyhow::Error::msg(e).context(format!("Url: {:?}", self.state.endpoint)))?
        .map_err(|e| anyhow::Error::msg(e).context(format!("Url: {:?}", self.state.endpoint)))?;
        let status = response.status();
        let body_bytes = response.into_body().collect().await?.to_bytes();
        if status != StatusCode::OK {
            // Not active
            if status == StatusCode::NOT_FOUND {
                trace!("Requested remote config and but remote config not active");
                return Ok(Some(vec![]));
            }

            let response_body = String::from_utf8_lossy(&body_bytes);
            anyhow::bail!("Server did not accept remote config request: {response_body}");
        }
        client_state.last_error = None;

        // Nothing changed
        if body_bytes.len() <= 3 {
            trace!("Requested remote config and got an empty reply");
            return Ok(None);
        }

        let response: ClientGetConfigsResponse = serde_json::from_slice(body_bytes.as_ref())?;

        let decoded_targets =
            base64::engine::general_purpose::STANDARD.decode(response.targets.as_slice())?;
        let targets_list = TargetsList::try_parse(decoded_targets.as_slice()).map_err(|e| {
            anyhow::Error::msg(e).context(format!(
                "Decoded targets reply: {}",
                String::from_utf8_lossy(decoded_targets.as_slice())
            ))
        })?;

        client_state.root_version = response.roots.iter().try_fold(
            client_state.root_version,
            |max, cur| -> anyhow::Result<_> {
                let decoded_root =
                    base64::engine::general_purpose::STANDARD.decode(cur.as_slice())?;
                let root = Root::try_parse(decoded_root.as_slice()).map_err(|e| {
                    anyhow::Error::msg(e).context(format!(
                        "Decoded roots reply: {}",
                        String::from_utf8_lossy(decoded_root.as_slice())
                    ))
                })?;
                Ok(std::cmp::max(max, root.signed.version))
            },
        )?;

        client_state.opaque_backend_state = targets_list
            .signed
            .custom
            .opaque_backend_state
            .as_bytes()
            .to_vec();

        debug!(
            "Received remote config of length {}, containing {:?} paths for target {:?}",
            body_bytes.len(),
            targets_list.signed.targets.keys().collect::<Vec<_>>(),
            target
        );

        let incoming_files: HashMap<_, _> = response
            .target_files
            .iter()
            .map(|f| (f.path.as_str(), f.raw.as_slice()))
            .collect();

        // This lock must be held continuously at least between the existence check
        // (target_files.get()) and the insertion later on. Makes more sense to just hold it
        // continuously
        let mut target_files = self.state.target_files_by_path.lock_or_panic();

        let mut config_paths = HashSet::new();
        for path in response.client_configs.iter() {
            match RemoteConfigPath::try_parse(path) {
                Ok(parsed) => {
                    config_paths.insert(parsed.into());
                }
                Err(e) => warn!("Failed parsing remote config path: {path} - {e:?}"),
            }
        }

        if self.state.expire_unused_files {
            target_files.retain(|k, _| config_paths.contains(k.as_ref()));
        }

        for (path, target_file) in targets_list.signed.targets {
            fn hash_sha256(v: &[u8]) -> String {
                format!("{:x}", Sha256::digest(v))
            }
            fn hash_sha512(v: &[u8]) -> String {
                format!("{:x}", Sha512::digest(v))
            }
            let (hasher, hash) = if let Some(sha256) = target_file.hashes.get("sha256") {
                (hash_sha256 as fn(&[u8]) -> String, *sha256)
            } else if let Some(sha512) = target_file.hashes.get("sha512") {
                (hash_sha512 as fn(&[u8]) -> String, *sha512)
            } else {
                warn!("Found a target file without hashes at path {path}");
                continue;
            };
            let parsed_path = match RemoteConfigPath::try_parse(path) {
                Ok(parsed_path) => parsed_path,
                Err(e) => {
                    warn!("Failed parsing remote config path: {path} - {e:?}");
                    continue;
                }
            };
            let handle = if let Some(StoredTargetFile {
                hash: old_hash,
                handle,
                ..
            }) = target_files.get(&parsed_path)
            {
                if old_hash == hash {
                    continue;
                }
                Some(handle.clone())
            } else {
                None
            };
            let Some(raw_file) = incoming_files.get(path) else {
                // If the file isn't there, it's not meant for us.
                continue;
            };
            let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(raw_file) else {
                anyhow::bail!(
                    "Failed base64 decoding config for path {path}: {}",
                    String::from_utf8_lossy(raw_file)
                )
            };
            let computed_hash = hasher(decoded.as_slice());
            if hash != computed_hash {
                anyhow::bail!("Computed hash of file {computed_hash} did not match remote config targets file hash {hash} for path {path}: file: {}", String::from_utf8_lossy(decoded.as_slice()));
            }
            let Some(version) = target_file.try_parse_version() else {
                anyhow::bail!("Failed parsing version from remote config path {path}");
            };
            debug!("Fetched new remote config file at path {path} targeting {target:?}");

            let parsed_path: Arc<RemoteConfigPath> = Arc::new(parsed_path.into());
            target_files.insert(
                parsed_path.clone(),
                StoredTargetFile {
                    hash: computed_hash,
                    state: ConfigState {
                        id: parsed_path.config_id.to_string(),
                        version,
                        product: parsed_path.product.to_string(),
                        apply_state: 2, // Acknowledged
                        apply_error: "".to_string(),
                    },
                    meta: TargetFileMeta {
                        path: path.to_string(),
                        length: decoded.len() as i64,
                        hashes: target_file
                            .hashes
                            .iter()
                            .map(|(algorithm, hash)| TargetFileHash {
                                algorithm: algorithm.to_string(),
                                hash: hash.to_string(),
                            })
                            .collect(),
                    },
                    handle: if let Some(handle) = handle {
                        self.file_storage.update(&handle, version, decoded)?;
                        handle
                    } else {
                        self.file_storage.store(version, parsed_path, decoded)?
                    },
                    expiring: false,
                },
            );
        }

        let mut configs = Vec::with_capacity(config_paths.len());
        for config in config_paths.iter() {
            if let Some(target_file) = target_files.get_mut(config) {
                target_file.expiring = false;
                configs.push(target_file.handle.clone());
            } else {
                anyhow::bail!("Found {config} in client_configs response, but it isn't stored.");
            }
        }

        client_state.targets_version = targets_list.signed.version as u64;
        client_state.last_config_paths = config_paths;
        Ok(Some(configs))
    }

    /// Quite generic fetching implementation:
    ///  - runs a request against the Remote Config Server,
    ///  - validates the data,
    ///  - removes unused files
    ///  - checks if the files are already known,
    ///  - stores new files,
    ///  - returns all currently active files.
    ///
    /// It also makes sure that old files are dropped before new files are inserted.
    ///
    /// Returns None if nothing changed. Otherwise Some(active configs).
    pub async fn fetch_once(
        &mut self,
        runtime_id: &str,
        target: &Target,
        product_capabilities: &ConfigProductCapabilities,
        client_id: &str,
        client_state: &mut ConfigClientState,
    ) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        let config_req = self.build_config_request(
            runtime_id,
            target,
            product_capabilities,
            client_id,
            &*client_state,
        );
        match &mut self.mode {
            FetcherMode::Agent => self.fetch_agent(config_req, target, client_state).await,
            #[cfg(feature = "agentless")]
            FetcherMode::Agentless(agentless_fetcher) => {
                #[allow(clippy::expect_used)]
                let client = config_req.client.expect(
                    "RC ConfigFetcher::build_config_request should always return a `Some` client",
                );

                let cache = agentless::TargetCache::new(&self.state, &self.file_storage);
                let res = match agentless_fetcher.fetch_config(client, &cache).await {
                    Ok(r) => r,
                    Err(e) => {
                        client_state.last_error = Some(format!("{e:#}"));
                        // Surface the recommended backoff to the consumer of
                        // `ConfigClientState::server_recommended_refresh_interval`
                        // so it waits before the next attempt. `None` means
                        // "no extra backoff, use the regular interval".
                        if let Some(backoff) = agentless_fetcher.next_backoff() {
                            client_state.refresh_interval = Some(backoff);
                        }
                        return Err(e);
                    }
                };

                client_state.root_version = res.root_version;
                client_state.targets_version = res.target_version;
                client_state.refresh_interval = Some(res.refresh_interval);
                if res.opaque_backend_state != client_state.opaque_backend_state {
                    client_state.opaque_backend_state = res.opaque_backend_state.clone();
                }
                client_state.last_error = None;

                let mut config_paths: HashSet<RemoteConfigPath> = HashSet::new();
                for target_ref in &res.targets {
                    if let Ok(parsed) = RemoteConfigPath::try_parse(&target_ref.path) {
                        config_paths.insert(parsed.into());
                    }
                }

                let configs = cache.collect_handles(&res.targets)?;

                client_state.last_config_paths = config_paths;
                Ok(Some(configs))
            }
        }
    }
}

fn make_agent_configs_endpoint(endpoint: &Endpoint) -> Endpoint {
    let mut parts = endpoint.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static("/v0.7/config"));
    #[allow(clippy::unwrap_used)]
    Endpoint {
        url: http::Uri::from_parts(parts).unwrap(),
        // Nullify the api key since we talk only to the agent
        api_key: None,
        test_token: endpoint.test_token.clone(),
        ..*endpoint
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::fetch::test_server::RemoteConfigServer;
    use crate::RemoteConfigSource;
    use http::Response;
    use std::mem::transmute;
    use std::sync::LazyLock;

    pub(crate) static PATH_FIRST: LazyLock<RemoteConfigPath> = LazyLock::new(|| RemoteConfigPath {
        source: RemoteConfigSource::Employee,
        product: RemoteConfigProduct::ApmTracing,
        config_id: "1234".to_string(),
        name: "config".to_string(),
    });

    pub(crate) static PATH_SECOND: LazyLock<RemoteConfigPath> =
        LazyLock::new(|| RemoteConfigPath {
            source: RemoteConfigSource::Employee,
            product: RemoteConfigProduct::ApmTracing,
            config_id: "9876".to_string(),
            name: "config".to_string(),
        });

    pub(crate) static DUMMY_TARGET: LazyLock<Arc<Target>> = LazyLock::new(|| {
        Arc::new(Target::new(
            "service".to_string(),
            "env".to_string(),
            "1.3.5".to_string(),
            vec![],
            vec![],
        ))
    });
    pub(crate) static DUMMY_TARGET_WITH_PROCESS_TAGS: LazyLock<Arc<Target>> = LazyLock::new(|| {
        Arc::new(Target::new(
            "service".to_string(),
            "env".to_string(),
            "1.3.5".to_string(),
            vec![],
            vec![
                "entrypoint.workdir:libdd-remote-config".to_string(),
                "entrypoint.type:script".to_string(),
            ],
        ))
    });

    static DUMMY_RUNTIME_ID: &str = "3b43524b-a70c-45dc-921d-34504e50c5eb";

    #[derive(Default)]
    pub struct Storage {
        pub files: Mutex<HashMap<Arc<RemoteConfigPath>, Arc<Mutex<DataStore>>>>,
    }

    pub struct PathStore {
        path: Arc<RemoteConfigPath>,
        storage: Arc<Storage>,
        pub data: Arc<Mutex<DataStore>>,
    }

    #[derive(Debug, Eq, PartialEq)]
    pub struct DataStore {
        pub version: u64,
        pub contents: String,
    }

    impl Drop for PathStore {
        fn drop(&mut self) {
            self.storage.files.lock().unwrap().remove(&self.path);
        }
    }

    impl FileStorage for Arc<Storage> {
        type StoredFile = PathStore;

        fn store(
            &self,
            version: u64,
            path: Arc<RemoteConfigPath>,
            contents: Vec<u8>,
        ) -> anyhow::Result<Arc<Self::StoredFile>> {
            let data = Arc::new(Mutex::new(DataStore {
                version,
                contents: String::from_utf8(contents).unwrap(),
            }));
            assert!(self
                .files
                .lock()
                .unwrap()
                .insert(path.clone(), data.clone())
                .is_none());
            Ok(Arc::new(PathStore {
                path: path.clone(),
                storage: self.clone(),
                data,
            }))
        }

        fn update(
            &self,
            file: &Arc<Self::StoredFile>,
            version: u64,
            contents: Vec<u8>,
        ) -> anyhow::Result<()> {
            *file.data.lock().unwrap() = DataStore {
                version,
                contents: String::from_utf8(contents).unwrap(),
            };
            Ok(())
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_inactive() {
        let server = RemoteConfigServer::spawn();
        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::new(server.dummy_options().invariants)),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();

        let mut response = http_common::empty_response(Response::builder()).unwrap();
        *response.status_mut() = StatusCode::NOT_FOUND;
        *server.next_response.lock().unwrap() = Some(response);

        let fetched = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &server.dummy_product_capabilities(),
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(fetched.is_empty());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_fetch_cache() {
        let server = RemoteConfigServer::spawn();

        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let storage = Arc::new(Storage::default());

        let invariants = ConfigInvariants {
            language: "php".to_string(),
            tracer_version: "1.2.3".to_string(),
            endpoint: server.endpoint.clone(),
            agentless: None,
        };
        let product_capabilities = ConfigProductCapabilities::new(
            vec![
                RemoteConfigProduct::ApmTracing,
                RemoteConfigProduct::AgentConfig,
            ],
            vec![RemoteConfigCapabilities::ApmTracingCustomTags],
        );

        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::new(invariants)),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();

        {
            opaque_state.last_error = Some("test".to_string());
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &product_capabilities,
                    "foo",
                    &mut opaque_state,
                )
                .await
                .unwrap()
                .unwrap();

            let req = server.last_request.lock().unwrap();

            let req = req.as_ref().unwrap();
            assert!(req.cached_target_files.is_empty());

            let client = req.client.as_ref().unwrap();
            assert_eq!(client.capabilities, &[128, 0]);
            assert_eq!(client.products, &["APM_TRACING", "AGENT_CONFIG"]);
            assert!(client.is_tracer);
            assert!(!client.is_agent);
            assert_eq!(client.id, "foo");

            let state = client.state.as_ref().unwrap();
            assert_eq!(state.error, "test");
            assert!(state.has_error);
            assert!(state.config_states.is_empty());
            assert!(state.backend_client_state.is_empty());

            let tracer = client.client_tracer.as_ref().unwrap();
            assert_eq!(tracer.service, DUMMY_TARGET.service);
            assert_eq!(tracer.env, DUMMY_TARGET.env);
            assert_eq!(tracer.app_version, DUMMY_TARGET.app_version);
            assert_eq!(tracer.runtime_id, DUMMY_RUNTIME_ID);
            assert_eq!(tracer.language, "php");
            assert_eq!(tracer.tracer_version, "1.2.3");

            assert_eq!(
                String::from_utf8_lossy(&opaque_state.opaque_backend_state),
                "some state"
            );
            assert_eq!(fetched.len(), 1);
            assert_eq!(storage.files.lock().unwrap().len(), 1);

            assert!(Arc::ptr_eq(
                &fetched[0].data,
                storage.files.lock().unwrap().get(&*PATH_FIRST).unwrap()
            ));
            assert_eq!(fetched[0].data.lock().unwrap().contents, "v1");
            assert_eq!(fetched[0].data.lock().unwrap().version, 1);
        }

        {
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &product_capabilities,
                    "foo",
                    &mut opaque_state,
                )
                .await
                .unwrap();
            assert!(fetched.is_none()); // no change

            let req = server.last_request.lock().unwrap();
            let req = req.as_ref().unwrap();
            assert_eq!(req.cached_target_files.len(), 1);

            let client = req.client.as_ref().unwrap();
            assert_eq!(client.capabilities, &[128, 0]);
            assert_eq!(client.products, &["APM_TRACING", "AGENT_CONFIG"]);
            assert!(client.is_tracer);
            assert!(!client.is_agent);
            assert_eq!(client.id, "foo");

            let state = client.state.as_ref().unwrap();
            assert!(!state.has_error);
            assert!(!state.config_states.is_empty());
            assert!(!state.backend_client_state.is_empty());

            let cached = &req.cached_target_files[0];
            assert_eq!(cached.path, &*PATH_FIRST.to_string());
            assert_eq!(cached.length, 2);
            assert_eq!(cached.hashes.len(), 1);
        }

        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
        );
        server.files.lock().unwrap().insert(
            PATH_SECOND.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "X".to_string()),
        );

        {
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &product_capabilities,
                    "foo",
                    &mut opaque_state,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fetched.len(), 2);
            assert_eq!(storage.files.lock().unwrap().len(), 2);

            let (first, second) = if fetched[0].data.lock().unwrap().version == 2 {
                (0, 1)
            } else {
                (1, 0)
            };

            assert!(Arc::ptr_eq(
                &fetched[first].data,
                storage.files.lock().unwrap().get(&*PATH_FIRST).unwrap()
            ));
            assert_eq!(fetched[first].data.lock().unwrap().contents, "v2");
            assert_eq!(fetched[first].data.lock().unwrap().version, 2);

            assert!(Arc::ptr_eq(
                &fetched[second].data,
                storage.files.lock().unwrap().get(&*PATH_SECOND).unwrap()
            ));
            assert_eq!(fetched[second].data.lock().unwrap().contents, "X");
            assert_eq!(fetched[second].data.lock().unwrap().version, 1);
        }

        {
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &product_capabilities,
                    "foo",
                    &mut opaque_state,
                )
                .await
                .unwrap();
            assert!(fetched.is_none()); // no change
        }

        server.files.lock().unwrap().remove(&*PATH_FIRST);

        {
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &product_capabilities,
                    "foo",
                    &mut opaque_state,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fetched.len(), 1);
            assert_eq!(storage.files.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_extra_services_forwarded_in_client_tracer() {
        let server: Arc<RemoteConfigServer> = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage,
            Arc::new(ConfigFetcherState::new(server.dummy_options().invariants)),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();

        // Default: nothing set, agent receives an empty list.
        fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &server.dummy_product_capabilities(),
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap();
        {
            let req = server.last_request.lock().unwrap();
            let tracer = req
                .as_ref()
                .unwrap()
                .client
                .as_ref()
                .unwrap()
                .client_tracer
                .as_ref()
                .unwrap();
            assert!(tracer.extra_services.is_empty());
        }

        // After set_extra_services, the next poll forwards them to the agent.
        opaque_state.set_extra_services(vec!["svc-a".to_string(), "svc-b".to_string()]);
        fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &server.dummy_product_capabilities(),
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap();
        {
            let req = server.last_request.lock().unwrap();
            let tracer = req
                .as_ref()
                .unwrap()
                .client
                .as_ref()
                .unwrap()
                .client_tracer
                .as_ref()
                .unwrap();
            assert_eq!(tracer.extra_services, &["svc-a", "svc-b"]);
        }

        // Replace-semantics: a subsequent set fully overrides the previous list.
        opaque_state.set_extra_services(vec!["svc-c".to_string()]);
        fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &server.dummy_product_capabilities(),
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap();
        {
            let req = server.last_request.lock().unwrap();
            let tracer = req
                .as_ref()
                .unwrap()
                .client
                .as_ref()
                .unwrap()
                .client_tracer
                .as_ref()
                .unwrap();
            assert_eq!(tracer.extra_services, &["svc-c"]);
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_process_tags_forwarded_in_client_tracer() {
        let server: Arc<RemoteConfigServer> = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (
                vec![DUMMY_TARGET_WITH_PROCESS_TAGS.clone()],
                1,
                "v1".to_string(),
            ),
        );

        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage,
            Arc::new(ConfigFetcherState::new(server.dummy_options().invariants)),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();

        let fetched = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET_WITH_PROCESS_TAGS,
                &server.dummy_product_capabilities(),
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(fetched.len(), 1);

        let req = server.last_request.lock().unwrap();
        let req = req.as_ref().unwrap();
        let tracer = req.client.as_ref().unwrap().client_tracer.as_ref().unwrap();
        assert_eq!(
            tracer.process_tags,
            &[
                "entrypoint.workdir:libdd-remote-config",
                "entrypoint.type:script"
            ]
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_capability_encoding() {
        let state = ConfigProductCapabilities::new(vec![], unsafe {
            vec![
                transmute::<u32, RemoteConfigCapabilities>(1u32),
                transmute::<u32, RemoteConfigCapabilities>(24u32),
                transmute::<u32, RemoteConfigCapabilities>(31u32),
            ]
        });
        assert_eq!(state.encoded_capabilities.len(), 4);
        assert_eq!(
            state.encoded_capabilities,
            (2u32 | (1 << 24) | (1 << 31)).to_be_bytes()
        );
    }
}
