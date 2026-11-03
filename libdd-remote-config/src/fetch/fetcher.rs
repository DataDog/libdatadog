// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "agentless")]
use super::agentless;
#[cfg(not(target_arch = "wasm32"))]
use super::shared::RefcountedFile;

use crate::targets::{Root, TargetData, TargetsList};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use anyhow::Context;
use base64::Engine;
use hashbrown::HashMap;
use http::StatusCode;
use http::uri::PathAndQuery;
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::{Endpoint, MutexExt};
use libdd_trace_protobuf::remoteconfig::{
    ClientGetConfigsRequest, ClientGetConfigsResponse, ClientState, ClientTracer, ConfigState,
    ConfigStatus, TargetFileHash, TargetFileMeta,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::ops::Add;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::MutexGuard;
use std::sync::{Arc, Mutex};
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
    /// The version of the file. The apply state of a version belongs to each client holding it,
    /// see [`ConfigClientState`].
    pub(crate) version: u64,
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

pub struct ConfigFetcherState<S, C: HttpClientCapability + SleepCapability> {
    pub(in crate::fetch) target_files_by_path:
        Mutex<HashMap<Arc<RemoteConfigPath>, StoredTargetFile<S>>>,
    pub invariants: ConfigInvariants,
    endpoint: Endpoint,
    pub expire_unused_files: bool,
    pub(super) http_client: C,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ConfigFetcherStateStats {
    /// Paths with a stored file, counting a single version per path.
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

#[cfg(not(target_arch = "wasm32"))]
pub struct ConfigFetcherFilesLock<'a, S> {
    inner: MutexGuard<'a, HashMap<Arc<RemoteConfigPath>, StoredTargetFile<S>>>,
}

// A path's entry may already hold a newer version than the file being expired: these only act on
// the entry if it still holds that very file.
#[cfg(not(target_arch = "wasm32"))]
impl<S: RefcountedFile> ConfigFetcherFilesLock<'_, S> {
    /// Actually remove the file from the known files.
    /// It may only be expired if already marked as expiring.
    pub fn expire_file(&mut self, file: &Arc<S>) {
        let path = &file.refcount().path;
        if let Some(target_file) = self.inner.get(path) {
            if !target_file.expiring || !Arc::ptr_eq(&target_file.handle, file) {
                return;
            }
        } else {
            return;
        }
        self.inner.remove(path);
    }

    /// Stop advertising the file as known. It's the predecessor to expire_file().
    pub fn mark_expiring(&mut self, file: &Arc<S>) {
        if let Some(target_file) = self.inner.get_mut(&file.refcount().path)
            && Arc::ptr_eq(&target_file.handle, file)
        {
            target_file.expiring = true;
        }
    }
}

impl<S, C: HttpClientCapability + SleepCapability> ConfigFetcherState<S, C> {
    pub fn with_client(invariants: ConfigInvariants, http_client: C) -> Self {
        let (endpoint, agentless) = match &invariants.agentless {
            Some(agentless_cfg) => {
                #[cfg(feature = "agentless")]
                {
                    (
                        agentless_cfg.agentless_endpoint().clone(),
                        Some(agentless_cfg.clone()),
                    )
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
            http_client,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
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

    pub fn stats(&self) -> ConfigFetcherStateStats {
        ConfigFetcherStateStats {
            active_files: self.target_files_by_path.lock_or_panic().len() as u32,
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum FetcherMode<C: HttpClientCapability + SleepCapability> {
    Agent(PhantomData<C>),
    #[cfg(feature = "agentless")]
    Agentless(agentless::AgentlessFetcher<C>),
}

pub struct ConfigFetcher<S: FileStorage, C: HttpClientCapability + SleepCapability> {
    pub file_storage: S,
    state: Arc<ConfigFetcherState<S::StoredFile, C>>,
    mode: FetcherMode<C>,
}

/// The file versions a client holds, one per path, each with the apply state reported for it.
/// Cloning shares the holdings, so that outcomes can be set while the client fetches.
#[derive(Clone, Default)]
pub(crate) struct ClientHoldings(Arc<Mutex<HashMap<RemoteConfigPath, Holding>>>);

struct Holding {
    /// Hash of the contents of the held version.
    hash: String,
    state: ConfigState,
}

impl ClientHoldings {
    /// Sets the apply state of the holding at `path`, if it is at `version` (or, if `None`, at
    /// whichever version is held). Returns whether a holding was updated.
    pub(crate) fn set(
        &self,
        path: &RemoteConfigPath,
        version: Option<u64>,
        state: ConfigApplyState,
    ) -> bool {
        let mut holdings = self.0.lock_or_panic();
        let Some(holding) = holdings.get_mut(path) else {
            debug!("Discarding the apply state of {path} (version {version:?}): not held");
            return false;
        };
        if let Some(version) = version
            && holding.state.version != version
        {
            debug!(
                "Discarding the apply state of {path} version {version}: version {} is held",
                holding.state.version
            );
            return false;
        }
        let (apply_state, apply_error) = match state {
            ConfigApplyState::Unacknowledged => (1, String::new()),
            ConfigApplyState::Acknowledged => (2, String::new()),
            ConfigApplyState::Error(error) => (3, error),
        };
        holding.state.apply_state = apply_state;
        holding.state.apply_error = apply_error;
        true
    }

    fn states(&self) -> Vec<ConfigState> {
        self.0
            .lock_or_panic()
            .values()
            .map(|holding| holding.state.clone())
            .collect()
    }

    pub(crate) fn clear(&self) {
        self.0.lock_or_panic().clear();
    }

    /// Replaces the holdings by the given (path, version, hash) ones. A version already held
    /// keeps its apply state, and so does a new version with unchanged contents: like the Go
    /// client, a version-only change keeps the outcome of applying those contents. Other
    /// versions start acknowledged.
    pub(crate) fn replace(&self, held: impl IntoIterator<Item = (RemoteConfigPath, u64, String)>) {
        let mut holdings = self.0.lock_or_panic();
        let mut previous = std::mem::take(&mut *holdings);
        for (path, version, hash) in held {
            let state = match previous.remove(&path) {
                Some(Holding { hash: prev, state }) if prev == hash => {
                    ConfigState { version, ..state }
                }
                _ => ConfigState {
                    id: path.config_id().to_string(),
                    version,
                    product: path.product().to_string(),
                    apply_state: 2, // Acknowledged
                    apply_error: String::new(),
                },
            };
            holdings.insert(path, Holding { hash, state });
        }
    }
}

/// The state of one client (one client id) of the remote config server. Besides what it reports
/// to the server, it keeps the file versions the client holds, each with its own apply state:
/// several clients holding the same version don't share their outcome.
pub struct ConfigClientState {
    opaque_backend_state: Vec<u8>,
    pub(crate) holdings: ClientHoldings,
    targets_version: u64,
    root_version: u64,
    last_error: Option<String>,
    /// Services discovered at runtime. Sent to the agent on each poll so it can route configs
    /// targeting those services to this client. Updated out-of-band by the consumer
    pub(super) extra_services: Vec<String>,
    /// Server-recommended interval between consecutive polls.
    refresh_interval: Option<Duration>,
}

impl Default for ConfigClientState {
    fn default() -> Self {
        ConfigClientState {
            opaque_backend_state: vec![],
            holdings: Default::default(),
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

    /// Sets the apply state of the file version this client holds at `path`, reported to the
    /// server with the following requests. Discarded, returning false, unless the client still
    /// holds `version` of the file: an outcome never applies to the version which replaced it.
    pub fn set_config_state(
        &self,
        path: &RemoteConfigPath,
        version: u64,
        state: ConfigApplyState,
    ) -> bool {
        self.holdings.set(path, Some(version), state)
    }

    /// Sets the apply state of whichever version of the file this client currently holds at
    /// `path`. Prefer [`Self::set_config_state`] when the outcome may be set after the client
    /// fetched again: by then it may hold another version.
    pub fn set_current_config_state(
        &self,
        path: &RemoteConfigPath,
        state: ConfigApplyState,
    ) -> bool {
        self.holdings.set(path, None, state)
    }
}

impl<S: FileStorage, C: HttpClientCapability + SleepCapability> ConfigFetcher<S, C> {
    /// Create a new config fetcher in agent mode.
    /// Use this when you know agentless mode is disabled and you need a sync constructor.
    pub fn new_no_agentless(
        file_storage: S,
        state: Arc<ConfigFetcherState<S::StoredFile, C>>,
    ) -> Self {
        ConfigFetcher {
            file_storage,
            state,
            mode: FetcherMode::Agent(PhantomData),
        }
    }

    /// Create a config fetcher in agentless mode around a pre-built agentless fetcher.
    #[cfg(all(test, feature = "agentless"))]
    pub(crate) fn with_agentless_fetcher(
        file_storage: S,
        state: Arc<ConfigFetcherState<S::StoredFile, C>>,
        agentless_fetcher: agentless::AgentlessFetcher<C>,
    ) -> Self {
        ConfigFetcher {
            file_storage,
            state,
            mode: FetcherMode::Agentless(agentless_fetcher),
        }
    }

    /// Create a new config fetcher.
    pub async fn new(
        file_storage: S,
        state: Arc<ConfigFetcherState<S::StoredFile, C>>,
    ) -> anyhow::Result<Self> {
        #[cfg(feature = "agentless")]
        let mode: FetcherMode<C> = match &state.invariants.agentless {
            Some(agentless_cfg) => FetcherMode::Agentless(
                agentless::AgentlessFetcher::new(
                    agentless_cfg.clone(),
                    state.endpoint.clone(),
                    state.http_client.clone(),
                )
                .await?,
            ),
            None => FetcherMode::Agent(PhantomData),
        };
        #[cfg(not(feature = "agentless"))]
        let mode: FetcherMode<C> = FetcherMode::Agent(PhantomData);

        Ok(ConfigFetcher {
            file_storage,
            state,
            mode,
        })
    }

    /// Destructures the fetcher into its file storage and state.
    pub fn into_parts(self) -> (S, Arc<ConfigFetcherState<S::StoredFile, C>>) {
        (self.file_storage, self.state)
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
        {
            let target_files = self.state.target_files_by_path.lock_or_panic();
            for StoredTargetFile { meta, expiring, .. } in target_files.values() {
                // The agent rejects the whole request if a cached file has a length below 1.
                // Not advertising empty files only makes the agent resend their empty body.
                if !expiring && meta.length > 0 {
                    cached_target_files.push(meta.clone());
                }
            }
        }
        let config_states = client_state.holdings.states();
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
        trace!("Submitting remote config request");
        let req = self
            .state
            .endpoint
            .to_request_builder(concat!("Libdatadog/", env!("CARGO_PKG_VERSION")))?
            .method(http::Method::POST)
            .header(
                http::header::CONTENT_TYPE,
                libdd_common::header::APPLICATION_JSON,
            )
            .body(bytes::Bytes::from(serde_json::to_string(&config_req)?))?;
        let sleeper = <C as SleepCapability>::new();
        let response = tokio::select! {
            biased;
            result = self.state.http_client.request(req) => result
                .map_err(|e| anyhow::Error::msg(e).context(format!("Url: {:?}", self.state.endpoint)))?,
            _ = sleeper.sleep(Duration::from_millis(self.state.endpoint.timeout_ms)) => {
                anyhow::bail!(
                    "Remote config request timed out after {}ms. Url: {:?}",
                    self.state.endpoint.timeout_ms,
                    self.state.endpoint
                )
            }
        };
        let status = response.status();
        let body_bytes = response.into_body();
        // The trace-agent answers 204 when the core agent had no response for it.
        if status == StatusCode::NO_CONTENT {
            trace!("Requested remote config and got no content");
            return Ok(None);
        }
        if status != StatusCode::OK {
            // Not active
            if status == StatusCode::NOT_FOUND {
                trace!("Requested remote config and but remote config not active");
                if self.state.expire_unused_files {
                    self.state.target_files_by_path.lock_or_panic().clear();
                }
                client_state.holdings.clear();
                client_state.targets_version = 0;
                client_state.opaque_backend_state.clear();
                return Ok(Some(vec![]));
            }

            let response_body = String::from_utf8_lossy(&body_bytes);
            anyhow::bail!("Server did not accept remote config request: {response_body}");
        }
        client_state.last_error = None;

        let result = self.process_agent_response(&body_bytes, target, client_state);
        if let Err(e) = &result {
            // Contrary to transport errors, failures to process the response are reported back
            // to the agent with the next request.
            client_state.last_error = Some(format!("{e:#}"));
        }
        result
    }

    fn process_agent_response(
        &mut self,
        body_bytes: &[u8],
        target: &Target,
        client_state: &mut ConfigClientState,
    ) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        // Nothing changed
        if body_bytes.len() <= 3 {
            trace!("Requested remote config and got an empty reply");
            return Ok(None);
        }

        let response: ClientGetConfigsResponse = serde_json::from_slice(body_bytes)?;

        if response.config_status == ConfigStatus::Expired as i32 {
            debug!(
                "Agent served remote config from an expired cache for target {target:?}, \
                 reporting all configurations as removed"
            );
            if self.state.expire_unused_files {
                self.state.target_files_by_path.lock_or_panic().clear();
            }
            client_state.holdings.clear();
            // Holding nothing, the client must not claim the targets it had: the agent would
            // answer that nothing changed, and the configs would never come back.
            client_state.targets_version = 0;
            client_state.opaque_backend_state.clear();
            return Ok(Some(vec![]));
        }

        let decoded_targets =
            base64::engine::general_purpose::STANDARD.decode(response.targets.as_slice())?;
        let targets_list = TargetsList::try_parse(decoded_targets.as_slice()).map_err(|e| {
            anyhow::Error::msg(e).context(format!(
                "Decoded targets reply: {}",
                String::from_utf8_lossy(decoded_targets.as_slice())
            ))
        })?;

        // Processing either completes or leaves the client state and the stored files as they
        // were: everything is validated first, and only then committed.
        let root_version = response.roots.iter().try_fold(
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

        let opaque_backend_state = targets_list
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

        let mut config_paths: HashSet<RemoteConfigPath> = HashSet::new();
        for path in response.client_configs.iter() {
            match RemoteConfigPath::try_parse(path) {
                Ok(parsed) => {
                    config_paths.insert(parsed.into());
                }
                Err(e) => warn!("Failed parsing remote config path: {path} - {e:?}"),
            }
        }

        // Files no longer listed are expired once the response is committed; until then, they
        // must be ignored.
        let expire_unused_files = self.state.expire_unused_files;
        let kept = |path: &RemoteConfigPath| !expire_unused_files || config_paths.contains(path);

        // Like the Go client, an error in any file rejects the whole update: it is reported to the
        // agent with the next request and, as the client keeps its targets version, the agent
        // resends the targets.
        let mut version_changes = vec![];
        let mut new_files = vec![];
        struct NewFile<'a, H> {
            path: &'a str,
            parsed_path: Arc<RemoteConfigPath>,
            target_file: TargetData<'a>,
            handle: Option<Arc<H>>,
            decoded: Vec<u8>,
            computed_hash: String,
            version: u64,
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
            let stored = target_files
                .get_key_value(&parsed_path)
                .filter(|(path, _)| kept(path))
                .map(|(_, stored)| stored);
            let handle = if let Some(stored) = stored {
                if stored.hash == hash {
                    if let Some(version) = target_file.try_parse_version() {
                        version_changes.push((parsed_path, version));
                    }
                    continue;
                }
                Some(stored.handle.clone())
            } else {
                None
            };
            let Some(raw_file) = incoming_files.get(path) else {
                // If the file isn't there, it's not meant for us.
                continue;
            };
            let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(raw_file) else {
                anyhow::bail!("Failed base64 decoding config for path {path}");
            };
            let computed_hash = hasher(decoded.as_slice());
            if hash != computed_hash {
                anyhow::bail!(
                    "Computed hash of file {computed_hash} did not match remote config targets \
                    file hash {hash} for path {path}"
                );
            }
            let Some(version) = target_file.try_parse_version() else {
                anyhow::bail!("Failed parsing version from remote config path {path}");
            };
            debug!("Fetched new remote config file at path {path} targeting {target:?}");
            new_files.push(NewFile {
                path,
                parsed_path: Arc::new(parsed_path.into()),
                target_file,
                handle,
                decoded,
                computed_hash,
                version,
            });
        }

        // A listed config must be either held already, or carried by this response.
        for config in config_paths.iter() {
            if !target_files.contains_key(config)
                && !new_files.iter().any(|f| f.path == config.as_str())
            {
                anyhow::bail!("Found {config} in client_configs response, but it isn't stored.");
            }
        }

        // Commit. Only storing files can still fail. New versions are stored first, without being
        // referenced anywhere yet: should one fail, the handles created so far are dropped, which
        // releases them, and nothing else has changed.
        let mut to_store = vec![];
        let mut in_place = vec![];
        for mut file in new_files {
            match file.handle.take() {
                // Only this fetcher's target uses the file: its contents are replaced in place.
                Some(handle) if expire_unused_files => in_place.push((handle, file)),
                // With shared state, other targets may still list the previous version until
                // their own next fetch. Store the new version as a new file; the previous one
                // expires once no target holds it anymore.
                _ => to_store.push(file),
            }
        }
        let mut stored_files = Vec::with_capacity(to_store.len() + in_place.len());
        for mut file in to_store {
            let decoded = std::mem::take(&mut file.decoded);
            let length = decoded.len() as i64;
            let handle = self
                .file_storage
                .store(file.version, file.parsed_path.clone(), decoded)
                .with_context(|| format!("Failed storing remote config {}", file.path))?;
            stored_files.push((file, length, handle));
        }
        // In-place updates cannot be undone, so they come last. Should one of them fail, those
        // done before it have replaced their contents already, while the client keeps describing
        // the previous version; as the update is rejected, the agent resends it and the next poll
        // updates them again. This limitation goes away with FileStorage::update(), which is to be
        // removed.
        for (handle, mut file) in in_place {
            let decoded = std::mem::take(&mut file.decoded);
            let length = decoded.len() as i64;
            self.file_storage
                .update(&handle, file.version, decoded)
                .with_context(|| format!("Failed storing remote config {}", file.path))?;
            stored_files.push((file, length, handle));
        }

        // Nothing can fail from here on.
        if expire_unused_files {
            target_files.retain(|k, _| config_paths.contains(k.as_ref()));
        }
        for (path, version) in version_changes {
            if let Some(stored) = target_files.get_mut(&path) {
                // Like the Go client, report a version-only change without handing out a new
                // file. Each client keeps the apply outcome of the unchanged contents (see
                // ClientHoldings::replace); clients still holding the previous version keep
                // reporting it until their own next fetch.
                stored.version = version;
            }
        }
        for (
            NewFile {
                path,
                parsed_path,
                target_file,
                computed_hash,
                version,
                ..
            },
            length,
            handle,
        ) in stored_files
        {
            target_files.insert(
                parsed_path.clone(),
                StoredTargetFile {
                    hash: computed_hash,
                    version,
                    meta: TargetFileMeta {
                        path: path.to_string(),
                        length,
                        hashes: target_file
                            .hashes
                            .iter()
                            .map(|(algorithm, hash)| TargetFileHash {
                                algorithm: algorithm.to_string(),
                                hash: hash.to_string(),
                            })
                            .collect(),
                    },
                    handle,
                    expiring: false,
                },
            );
        }

        let mut configs = Vec::with_capacity(config_paths.len());
        let mut held = Vec::with_capacity(config_paths.len());
        for config in config_paths.iter() {
            if let Some(target_file) = target_files.get_mut(config) {
                target_file.expiring = false;
                configs.push(target_file.handle.clone());
                held.push((
                    config.clone(),
                    target_file.version,
                    target_file.hash.clone(),
                ));
            }
        }
        drop(target_files);

        client_state.root_version = root_version;
        client_state.targets_version = targets_list.signed.version as u64;
        client_state.opaque_backend_state = opaque_backend_state;
        client_state.holdings.replace(held);
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
            FetcherMode::Agent(_) => self.fetch_agent(config_req, target, client_state).await,
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

                let (configs, held) = cache.lock().collect_handles(&res.targets)?;

                client_state.holdings.replace(held);
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
    use http::Response;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_common::http_common;
    use std::mem::transmute;
    use std::sync::LazyLock;

    pub(crate) static PATH_FIRST: LazyLock<RemoteConfigPath> = LazyLock::new(|| {
        RemoteConfigPath::parse("employee/APM_TRACING/1234/config")
            .expect("PATH_FIRST is a valid remote config path")
    });

    pub(crate) static PATH_SECOND: LazyLock<RemoteConfigPath> = LazyLock::new(|| {
        RemoteConfigPath::parse("employee/APM_TRACING/9876/config")
            .expect("PATH_SECOND is a valid remote config path")
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
        /// Storing or updating this path fails.
        pub fail_path: Mutex<Option<RemoteConfigPath>>,
    }

    impl Storage {
        fn check_failure(&self, path: &RemoteConfigPath) -> anyhow::Result<()> {
            if self.fail_path.lock().unwrap().as_ref() == Some(path) {
                anyhow::bail!("storage failure for {path}");
            }
            Ok(())
        }
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
            let mut files = self.storage.files.lock().unwrap();
            // A newer version of the path may have been stored while this one was alive.
            if files
                .get(&self.path)
                .is_some_and(|data| Arc::ptr_eq(data, &self.data))
            {
                files.remove(&self.path);
            }
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
            self.check_failure(&path)?;
            let data = Arc::new(Mutex::new(DataStore {
                version,
                contents: String::from_utf8(contents).unwrap(),
            }));
            // A previous version may still be alive, held by targets that haven't fetched
            // the new one yet; the map tracks the latest version of each path.
            self.files
                .lock()
                .unwrap()
                .insert(path.clone(), data.clone());
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
            self.check_failure(&file.path)?;
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
            Arc::new(ConfigFetcherState::with_client(
                server.dummy_options().invariants,
                NativeCapabilities::new_periodic(),
            )),
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
    async fn test_no_content_means_unchanged() {
        let server = RemoteConfigServer::spawn();
        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::with_client(
                server.dummy_options().invariants,
                NativeCapabilities::new_periodic(),
            )),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();

        // The trace-agent answers 204 when the core agent returned no response.
        let mut response = http_common::empty_response(Response::builder()).unwrap();
        *response.status_mut() = StatusCode::NO_CONTENT;
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
            .unwrap();

        assert!(fetched.is_none(), "204 means nothing changed");
        assert!(opaque_state.last_error.is_none());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_expired_config_status_drops_all_configs() {
        let server = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::with_client(
                server.dummy_options().invariants,
                NativeCapabilities::new_periodic(),
            )),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();

        let capabilities = server.dummy_product_capabilities();

        // Each fetch is scoped so the returned handles are dropped, mirroring a consumer that
        // releases the previous generation of configs.
        {
            // The config applies normally while the agent is healthy.
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &capabilities,
                    "foo",
                    &mut opaque_state,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fetched.len(), 1);
            assert_eq!(storage.files.lock().unwrap().len(), 1);
        }

        {
            // The agent starts serving from an expired cache: everything must be reported gone.
            *server.config_status.lock().unwrap() = ConfigStatus::Expired;
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &capabilities,
                    "foo",
                    &mut opaque_state,
                )
                .await
                .unwrap()
                .unwrap();
            assert!(
                fetched.is_empty(),
                "expired configuration must be treated as removed"
            );
            assert!(
                storage.files.lock().unwrap().is_empty(),
                "expired configuration must be dropped from storage"
            );
        }

        {
            // The agent recovers: the configuration must be fetched and applied anew. It is no
            // longer advertised as cached or applied, so the agent re-sends its contents.
            *server.config_status.lock().unwrap() = ConfigStatus::Ok;
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &capabilities,
                    "foo",
                    &mut opaque_state,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fetched.len(), 1);
            assert_eq!(fetched[0].data.lock().unwrap().contents, "v1");

            let req = server.last_request.lock().unwrap();
            let req = req.as_ref().unwrap();
            assert!(
                req.cached_target_files.is_empty(),
                "dropped configs must not be advertised as cached"
            );
            assert!(
                req.client
                    .as_ref()
                    .unwrap()
                    .state
                    .as_ref()
                    .unwrap()
                    .config_states
                    .is_empty(),
                "dropped configs must not be reported as applied"
            );
            assert_eq!(
                req.client
                    .as_ref()
                    .unwrap()
                    .state
                    .as_ref()
                    .unwrap()
                    .targets_version,
                0,
                "with every config dropped, the client must not claim the targets it had"
            );
        }
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
            Arc::new(ConfigFetcherState::with_client(
                invariants,
                NativeCapabilities::new_periodic(),
            )),
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
    async fn test_empty_file_not_advertised_with_zero_length() {
        let server = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "".to_string()),
        );
        server.files.lock().unwrap().insert(
            PATH_SECOND.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "X".to_string()),
        );

        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::with_client(
                server.dummy_options().invariants,
                NativeCapabilities::new_periodic(),
            )),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();
        let capabilities = server.dummy_product_capabilities();

        let fetched = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.len(), 2, "the empty config is stored too");
        assert!(opaque_state.last_error.is_none());

        fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap();
        let req = server.last_request.lock().unwrap();
        let cached = &req.as_ref().unwrap().cached_target_files;
        // The agent rejects the whole request if any cached file has a length below 1.
        assert!(
            cached.iter().all(|f| f.length >= 1),
            "a 0-byte file must not be advertised with length 0: {cached:?}"
        );
        assert!(
            cached.iter().any(|f| f.path == PATH_SECOND.to_string()),
            "non-empty files are still advertised: {cached:?}"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_version_only_change_is_reported() {
        let server = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "same".to_string()),
        );

        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::with_client(
                server.dummy_options().invariants,
                NativeCapabilities::new_periodic(),
            )),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();
        let capabilities = server.dummy_product_capabilities();

        let first = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.len(), 1);
        assert!(opaque_state.set_config_state(
            &PATH_FIRST,
            1,
            ConfigApplyState::Error("bad".to_string())
        ));
        // Another client sharing the fetcher state, which will lag behind.
        let mut lagging_state = ConfigClientState::default();
        fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "bar",
                &mut lagging_state,
            )
            .await
            .unwrap();

        // Only the version changes; the contents (and thus the hash) stay the same.
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 2, "same".to_string()),
        );
        let second = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.len(), 1);
        assert!(
            Arc::ptr_eq(&first[0], &second[0]),
            "unchanged contents must not be handed out as a new file"
        );

        fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap();
        let states = server
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
            .clone();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].version, 2, "the new version must be reported");
        assert_eq!(
            (states[0].apply_state, states[0].apply_error.as_str()),
            (3, "bad"),
            "the apply outcome of the unchanged contents is kept"
        );

        // The lagging client still holds version 1, and must report that in its request.
        fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "bar",
                &mut lagging_state,
            )
            .await
            .unwrap();
        let states = server.reported_config_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].0, 1, "a lagging client reports what it holds");

        // Having fetched, it reports the new version with its own outcome.
        fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "bar",
                &mut lagging_state,
            )
            .await
            .unwrap();
        assert_eq!(
            server.reported_config_states(),
            [(2, 2, String::new())],
            "a client keeps its own outcome across a version-only change"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_failed_file_is_retried() {
        let server = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::with_client(
                server.dummy_options().invariants,
                NativeCapabilities::new_periodic(),
            )),
        )
        .await
        .unwrap();
        let mut opaque_state = ConfigClientState::default();
        let capabilities = server.dummy_product_capabilities();

        let fetched = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.len(), 1);
        drop(fetched);
        let applied_targets_version = opaque_state.targets_version;

        // The next targets change the first file, whose body is served corrupted, and add a
        // second, valid one.
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
        );
        server.files.lock().unwrap().insert(
            PATH_SECOND.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "X".to_string()),
        );
        *server.corrupt_next_body.lock().unwrap() = Some(PATH_FIRST.clone());

        let result = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await;
        assert!(
            result.is_err(),
            "a file error rejects the whole update, the valid file included"
        );
        assert!(opaque_state.last_error.is_some());
        assert_eq!(
            opaque_state.targets_version, applied_targets_version,
            "the client keeps claiming the targets it holds"
        );
        assert_eq!(
            opaque_state
                .holdings
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|(path, holding)| (path.clone(), holding.state.version))
                .collect::<Vec<_>>(),
            [(PATH_FIRST.clone(), 1)],
            "the client keeps reporting the previous state"
        );

        // The agent resends the targets, now with a valid body, and everything is applied.
        let fetched = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap()
            .expect("the rejected update must be fetched again");
        let mut contents: Vec<_> = fetched
            .iter()
            .map(|f| f.data.lock().unwrap().contents.clone())
            .collect();
        contents.sort();
        assert_eq!(contents, ["X", "v2"]);
        assert!(opaque_state.last_error.is_none());
        assert_ne!(opaque_state.targets_version, applied_targets_version);
        let req = server.last_request.lock().unwrap();
        let state = req
            .as_ref()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .state
            .as_ref()
            .unwrap();
        assert!(state.has_error, "the failure is reported to the agent");
        assert_eq!(state.targets_version, applied_targets_version);
        assert_eq!(
            state
                .config_states
                .iter()
                .map(|s| (s.id.as_str(), s.version))
                .collect::<Vec<_>>(),
            [("1234", 1)],
            "the agent is told the client still holds the previous version"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_failed_response_leaves_state_unchanged() {
        for failure in [
            ResponseFailure::UnheldClientConfig,
            ResponseFailure::CorruptFile,
            ResponseFailure::Storage,
        ] {
            failed_response_leaves_state_unchanged(true, failure).await;
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_failed_response_leaves_shared_state_unchanged() {
        for failure in [
            ResponseFailure::UnheldClientConfig,
            ResponseFailure::CorruptFile,
            ResponseFailure::Storage,
        ] {
            failed_response_leaves_state_unchanged(false, failure).await;
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum ResponseFailure {
        /// client_configs lists a path the response carries neither metadata nor a body for.
        UnheldClientConfig,
        /// The body of the added file doesn't match its hash.
        CorruptFile,
        /// Storing a file fails.
        Storage,
    }

    async fn failed_response_leaves_state_unchanged(
        expire_unused_files: bool,
        failure: ResponseFailure,
    ) {
        let path_third = RemoteConfigPath::parse("employee/APM_TRACING/5678/config").unwrap();
        let server = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );
        server.files.lock().unwrap().insert(
            PATH_SECOND.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "X".to_string()),
        );

        let storage = Arc::new(Storage::default());
        let mut state = ConfigFetcherState::with_client(
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        state.expire_unused_files = expire_unused_files;
        let state = Arc::new(state);
        let mut fetcher = ConfigFetcher::new(storage.clone(), state.clone())
            .await
            .unwrap();
        let mut opaque_state = ConfigClientState::default();
        let capabilities = server.dummy_product_capabilities();

        let held = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(held.len(), 2);

        let stored_files = |storage: &Storage| {
            let mut files: Vec<_> = storage
                .files
                .lock()
                .unwrap()
                .iter()
                .map(|(p, d)| {
                    let d = d.lock().unwrap();
                    (p.to_string(), d.version, d.contents.clone())
                })
                .collect();
            files.sort();
            files
        };
        let known_files = || {
            let mut files: Vec<_> = state
                .target_files_by_path
                .lock()
                .unwrap()
                .iter()
                .map(|(p, f)| (p.to_string(), f.hash.clone(), f.expiring))
                .collect();
            files.sort();
            files
        };
        let files_before = stored_files(&storage);
        let known_before = known_files();

        // A response removing a file, changing one, adding one, and failing.
        server.files.lock().unwrap().remove(&*PATH_FIRST);
        server.files.lock().unwrap().insert(
            PATH_SECOND.clone(),
            (vec![DUMMY_TARGET.clone()], 2, "Y".to_string()),
        );
        server.files.lock().unwrap().insert(
            path_third.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "Z".to_string()),
        );
        let expected_error = match failure {
            ResponseFailure::UnheldClientConfig => {
                *server.extra_client_configs.lock().unwrap() =
                    vec!["employee/APM_TRACING/5555/config".to_string()];
                "isn't stored"
            }
            ResponseFailure::CorruptFile => {
                *server.corrupt_next_body.lock().unwrap() = Some(path_third.clone());
                "did not match"
            }
            ResponseFailure::Storage => {
                // With expire_unused_files, the changed file is updated in place, which must not
                // happen before the added file failed to be stored. Otherwise, both are stored as
                // new files, and whichever was stored first must be released.
                *storage.fail_path.lock().unwrap() = Some(if expire_unused_files {
                    path_third.clone()
                } else {
                    PATH_SECOND.clone()
                });
                "storage failure"
            }
        };

        let Err(err) = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await
        else {
            panic!("the response must be rejected ({failure:?})");
        };
        assert!(
            format!("{err:#}").contains(expected_error),
            "{failure:?}: {err:#}"
        );
        let failed_req_state = server
            .last_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();

        assert_eq!(
            stored_files(&storage),
            files_before,
            "{failure:?}: no half-stored files"
        );
        assert_eq!(
            known_files(),
            known_before,
            "{failure:?}: no pruned nor added entries"
        );
        assert_eq!(
            held.iter()
                .map(|f| f.data.lock().unwrap().contents.clone())
                .collect::<HashSet<_>>(),
            HashSet::from(["v1".to_string(), "X".to_string()]),
            "{failure:?}: the held files keep their contents"
        );

        // The next request reports the same protocol state as the one before the failure.
        let refetch = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut opaque_state,
            )
            .await;
        // A corrupt body is served once only; the other failures persist.
        assert_eq!(
            refetch.is_err(),
            failure != ResponseFailure::CorruptFile,
            "{failure:?}: the agent sends the same targets again"
        );
        let req = server.last_request.lock().unwrap();
        let req = req.as_ref().unwrap();
        let client_state = |r: &ClientGetConfigsRequest| {
            let s = r.client.as_ref().unwrap().state.as_ref().unwrap();
            let mut config_states = s.config_states.clone();
            config_states.sort_by(|a, b| a.id.cmp(&b.id));
            (
                s.root_version,
                s.targets_version,
                s.backend_client_state.clone(),
                config_states,
            )
        };
        let cached = |r: &ClientGetConfigsRequest| {
            let mut c = r.cached_target_files.clone();
            c.sort_by(|a, b| a.path.cmp(&b.path));
            c
        };
        assert_eq!(
            client_state(req),
            client_state(&failed_req_state),
            "{failure:?}"
        );
        assert_eq!(cached(req), cached(&failed_req_state), "{failure:?}");
        assert!(
            req.client
                .as_ref()
                .unwrap()
                .state
                .as_ref()
                .unwrap()
                .has_error
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_lagging_client_advertises_newest_version() {
        let server = RemoteConfigServer::spawn();
        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 1, "v1".to_string()),
        );

        let storage = Arc::new(Storage::default());
        // Shared state, as used by the shared/multitarget fetchers: several clients, each
        // possibly holding a different version of a path.
        let mut state = ConfigFetcherState::with_client(
            server.dummy_options().invariants,
            NativeCapabilities::new_periodic(),
        );
        state.expire_unused_files = false;
        let mut fetcher = ConfigFetcher::new(storage.clone(), Arc::new(state))
            .await
            .unwrap();
        let capabilities = server.dummy_product_capabilities();
        let mut up_to_date = ConfigClientState::default();
        let mut lagging = ConfigClientState::default();

        for client_state in [&mut up_to_date, &mut lagging] {
            fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    &DUMMY_TARGET,
                    &capabilities,
                    "foo",
                    client_state,
                )
                .await
                .unwrap()
                .unwrap();
        }

        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (vec![DUMMY_TARGET.clone()], 2, "v2".to_string()),
        );
        let newest = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut up_to_date,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(newest[0].data.lock().unwrap().contents, "v2");

        // The lagging client still holds v1, but v2 is what the agent will serve it: the agent
        // only compares cached files with its current targets, so v1 would be of no use.
        let fetched = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                &DUMMY_TARGET,
                &capabilities,
                "foo",
                &mut lagging,
            )
            .await
            .unwrap()
            .unwrap();
        let req = server.last_request.lock().unwrap();
        let req = req.as_ref().unwrap();
        let states = &req
            .client
            .as_ref()
            .unwrap()
            .state
            .as_ref()
            .unwrap()
            .config_states;
        assert_eq!(
            states.iter().map(|s| s.version).collect::<Vec<_>>(),
            [1],
            "the lagging client reports the version it holds"
        );
        let v2_hash = format!("{:x}", Sha256::digest("v2"));
        assert_eq!(
            req.cached_target_files,
            [TargetFileMeta {
                path: PATH_FIRST.to_string(),
                length: 2,
                hashes: vec![TargetFileHash {
                    algorithm: "sha256".to_string(),
                    hash: v2_hash,
                }],
            }],
            "the newest version is advertised, matching the agent's current targets, so its \
             body isn't sent again"
        );
        assert_eq!(fetched.len(), 1);
        assert!(
            Arc::ptr_eq(&fetched[0], &newest[0]),
            "the lagging client gets the already stored newest version"
        );
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
            Arc::new(ConfigFetcherState::with_client(
                server.dummy_options().invariants,
                NativeCapabilities::new_periodic(),
            )),
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
            Arc::new(ConfigFetcherState::with_client(
                server.dummy_options().invariants,
                NativeCapabilities::new_periodic(),
            )),
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

    #[test]
    fn test_dependent_flag_capability_encoding() {
        let legacy = ConfigProductCapabilities::new(
            vec![RemoteConfigProduct::FfeFlags],
            vec![RemoteConfigCapabilities::FfeFlagConfigurationRules],
        );
        assert_eq!(legacy.encoded_capabilities, [0x40, 0, 0, 0, 0, 0]);

        let capable = ConfigProductCapabilities::new(
            vec![RemoteConfigProduct::FfeFlags],
            vec![
                RemoteConfigCapabilities::FfeFlagConfigurationRules,
                RemoteConfigCapabilities::FfeFlagDependentFlagVariantKeyV1,
            ],
        );
        assert_eq!(capable.encoded_capabilities, [0x04, 0x40, 0, 0, 0, 0, 0]);
        assert_eq!(
            serde_json::to_string(&RemoteConfigCapabilities::FfeFlagDependentFlagVariantKeyV1)
                .unwrap(),
            r#""FFE_FLAG_DEPENDENT_FLAG_VARIANT_KEY_V1""#,
        );
    }
}
