// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::targets::TargetsList;
use crate::{
    RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigPathRef, RemoteConfigPathType,
    RemoteConfigProduct, Target,
};
use base64::Engine;
use datadog_trace_protobuf::remoteconfig::{
    ClientGetConfigsRequest, ClientGetConfigsResponse, ClientState, ClientTracer, ConfigState,
    TargetFileHash, TargetFileMeta,
};
use ddcommon_net1::{connector, Endpoint};
use http::uri::Scheme;
use hyper::body::HttpBody;
use hyper::http::uri::PathAndQuery;
use hyper::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::collections::{HashMap, HashSet};
use std::mem::transmute;
use std::ops::Add;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tracing::{debug, trace, warn};

const PROD_INTAKE_SUBDOMAIN: &str = "config";

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
    pub products: Vec<RemoteConfigProduct>,
    pub capabilities: Vec<RemoteConfigCapabilities>,
}

struct StoredTargetFile<S> {
    hash: String,
    handle: Arc<S>,
    state: ConfigState,
    meta: TargetFileMeta,
    expiring: bool,
}

pub enum ConfigApplyState {
    Unacknowledged,
    Acknowledged,
    Error(String),
}

pub struct ConfigFetcherState<S> {
    target_files_by_path: Mutex<HashMap<Arc<RemoteConfigPath>, StoredTargetFile<S>>>,
    pub invariants: ConfigInvariants,
    endpoint: Endpoint,
    encoded_capabilities: Vec<u8>,
    pub expire_unused_files: bool,
}

#[derive(Default, Serialize, Deserialize)]
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
        let capability_len = invariants
            .capabilities
            .iter()
            .map(|c| *c as usize / 8 + 1)
            .max()
            .unwrap_or(0);
        let mut encoded_capabilities = vec![0; capability_len];
        for capability in invariants.capabilities.iter().map(|c| *c as usize) {
            encoded_capabilities[capability_len - (capability >> 3) - 1] |= 1 << (capability & 7);
        }
        ConfigFetcherState {
            target_files_by_path: Default::default(),
            endpoint: get_product_endpoint(PROD_INTAKE_SUBDOMAIN, &invariants.endpoint),
            invariants,
            encoded_capabilities,
            expire_unused_files: true,
        }
    }

    /// To remove unused remote files manually. Must not be called when auto expiration is active.
    /// Note: careful attention must be paid when using this API in order to not deadlock:
    /// - This files_lock() must always be called prior to locking any data structure locked within
    ///   FileStorage::store().
    /// - Also, files_lock() must not be called from within FileStorage::store().
    pub fn files_lock(&self) -> ConfigFetcherFilesLock<S> {
        assert!(!self.expire_unused_files);
        ConfigFetcherFilesLock {
            inner: self.target_files_by_path.lock().unwrap(),
        }
    }

    /// Sets the apply state on a stored file.
    pub fn set_config_state(&self, file: &RemoteConfigPath, state: ConfigApplyState) {
        if let Some(target_file) = self.target_files_by_path.lock().unwrap().get_mut(file) {
            match state {
                ConfigApplyState::Unacknowledged => {
                    target_file.state.apply_state = 1;
                    target_file.state.apply_error = "".to_string();
                }
                ConfigApplyState::Acknowledged => {
                    target_file.state.apply_state = 1;
                    target_file.state.apply_error = "".to_string();
                }
                ConfigApplyState::Error(error) => {
                    target_file.state.apply_state = 1;
                    target_file.state.apply_error = error;
                }
            }
        }
    }

    pub fn stats(&self) -> ConfigFetcherStateStats {
        ConfigFetcherStateStats {
            active_files: self.target_files_by_path.lock().unwrap().len() as u32,
        }
    }
}

pub struct ConfigFetcher<S: FileStorage> {
    pub file_storage: S,
    state: Arc<ConfigFetcherState<S::StoredFile>>,
}

#[derive(Default)]
pub struct ConfigClientState {
    opaque_backend_state: Vec<u8>,
    last_configs: Vec<String>,
    // 'static because it actually depends on last_configs, and rust doesn't like self-referencing
    last_config_paths: HashSet<RemoteConfigPathRef<'static>>,
    targets_version: u64,
    last_error: Option<String>,
}

impl<S: FileStorage> ConfigFetcher<S> {
    pub fn new(file_storage: S, state: Arc<ConfigFetcherState<S::StoredFile>>) -> Self {
        ConfigFetcher {
            file_storage,
            state,
        }
    }

    /// Sets the apply state on a stored file.
    pub fn set_config_state(&self, file: &RemoteConfigPath, state: ConfigApplyState) {
        self.state.set_config_state(file, state)
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
        target: Arc<Target>,
        client_id: &str,
        opaque_state: &mut ConfigClientState,
    ) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        if self.state.endpoint.api_key.is_some() {
            // Using remote config talking to the backend directly is not supported.
            return Ok(Some(vec![]));
        }

        let Target {
            service,
            env,
            app_version,
            tags,
        } = (*target).clone();

        let mut cached_target_files = vec![];
        let mut config_states = vec![];

        {
            let target_files = self.state.target_files_by_path.lock().unwrap();
            for StoredTargetFile { meta, expiring, .. } in target_files.values() {
                if !expiring {
                    cached_target_files.push(meta.clone());
                }
            }

            for config in opaque_state.last_config_paths.iter() {
                if let Some(StoredTargetFile { state, .. }) =
                    target_files.get(config as &dyn RemoteConfigPathType)
                {
                    config_states.push(state.clone());
                }
            }
        }

        let config_req = ClientGetConfigsRequest {
            client: Some(datadog_trace_protobuf::remoteconfig::Client {
                state: Some(ClientState {
                    root_version: 1,
                    targets_version: opaque_state.targets_version,
                    config_states,
                    has_error: opaque_state.last_error.is_some(),
                    error: opaque_state.last_error.take().unwrap_or_default(),
                    backend_client_state: std::mem::take(&mut opaque_state.opaque_backend_state),
                }),
                id: client_id.into(),
                products: self
                    .state
                    .invariants
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
                    extra_services: vec![],
                    env,
                    app_version,
                    tags: tags.iter().map(|t| t.to_string()).collect(),
                }),
                is_agent: false,
                client_agent: None,
                last_seen: 0,
                capabilities: self.state.encoded_capabilities.clone(),
            }),
            cached_target_files,
        };

        trace!("Submitting remote config request: {config_req:?}");

        let req = self
            .state
            .endpoint
            .into_request_builder(concat!("Sidecar/", env!("CARGO_PKG_VERSION")))?
            .method(http::Method::POST)
            .header(
                http::header::CONTENT_TYPE,
                ddcommon_net1::header::APPLICATION_JSON,
            )
            .body(serde_json::to_string(&config_req)?)?;
        let response = tokio::time::timeout(
            Duration::from_millis(self.state.endpoint.timeout_ms),
            Client::builder()
                .build(connector::Connector::default())
                .request(req),
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

            let response_body = String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
            anyhow::bail!("Server did not accept remote config request: {response_body}");
        }

        // Nothing changed
        if body_bytes.len() <= 3 {
            trace!("Requested remote config and got an empty reply");
            return Ok(None);
        }

        let response: ClientGetConfigsResponse =
            serde_json::from_str(&String::from_utf8_lossy(body_bytes.as_ref()))?;

        let decoded_targets =
            base64::engine::general_purpose::STANDARD.decode(response.targets.as_slice())?;
        let targets_list = TargetsList::try_parse(decoded_targets.as_slice()).map_err(|e| {
            anyhow::Error::msg(e).context(format!(
                "Decoded targets reply: {}",
                String::from_utf8_lossy(decoded_targets.as_slice())
            ))
        })?;

        opaque_state.opaque_backend_state = targets_list
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
        let mut target_files = self.state.target_files_by_path.lock().unwrap();

        let mut config_paths: HashSet<RemoteConfigPathRef<'static>> = HashSet::new();
        for path in response.client_configs.iter() {
            match RemoteConfigPath::try_parse(path) {
                // SAFTEY: The lifetime of RemoteConfigPathRef is tied to the config_paths
                // Vec<String>
                Ok(parsed) => {
                    config_paths.insert(unsafe {
                        transmute::<RemoteConfigPathRef<'_>, RemoteConfigPathRef<'_>>(parsed)
                    });
                }
                Err(e) => warn!("Failed parsing remote config path: {path} - {e:?}"),
            }
        }

        if self.state.expire_unused_files {
            target_files.retain(|k, _| config_paths.contains(&(&**k).into()));
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
            }) = target_files.get(&parsed_path as &dyn RemoteConfigPathType)
            {
                if old_hash == hash {
                    continue;
                }
                Some(handle.clone())
            } else {
                None
            };
            // If the file isn't there, it's not meant for us.
            if let Some(raw_file) = incoming_files.get(path) {
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(raw_file) {
                    let computed_hash = hasher(decoded.as_slice());
                    if hash != computed_hash {
                        anyhow::bail!("Computed hash of file {computed_hash} did not match remote config targets file hash {hash} for path {path}: file: {}", String::from_utf8_lossy(decoded.as_slice()));
                    }
                    if let Some(version) = target_file.try_parse_version() {
                        debug!(
                            "Fetched new remote config file at path {path} targeting {target:?}"
                        );

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
                    } else {
                        anyhow::bail!("Failed parsing version from remote config path {path}");
                    }
                } else {
                    anyhow::bail!(
                        "Failed base64 decoding config for path {path}: {}",
                        String::from_utf8_lossy(raw_file)
                    )
                }
            }
        }

        let mut configs = Vec::with_capacity(config_paths.len());
        for config in config_paths.iter() {
            if let Some(target_file) = target_files.get_mut(config as &dyn RemoteConfigPathType) {
                target_file.expiring = false;
                configs.push(target_file.handle.clone());
            } else {
                anyhow::bail!("Found {config} in client_configs response, but it isn't stored.");
            }
        }

        opaque_state.targets_version = targets_list.signed.version as u64;
        opaque_state.last_configs = response.client_configs;
        opaque_state.last_config_paths = config_paths;
        Ok(Some(configs))
    }
}

fn get_product_endpoint(subdomain: &str, endpoint: &Endpoint) -> Endpoint {
    let mut parts = endpoint.url.clone().into_parts();
    if parts.authority.is_some() && parts.scheme.is_none() {
        parts.scheme = Some(Scheme::HTTPS);
        parts.authority = Some(
            format!("{}.{}", subdomain, parts.authority.unwrap())
                .parse()
                .unwrap(),
        );
    }
    parts.path_and_query = Some(PathAndQuery::from_static("/v0.7/config"));
    Endpoint {
        url: hyper::Uri::from_parts(parts).unwrap(),
        api_key: endpoint.api_key.clone(),
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
    use hyper::Body;
    use lazy_static::lazy_static;

    lazy_static! {
        pub static ref PATH_FIRST: RemoteConfigPath = RemoteConfigPath {
            source: RemoteConfigSource::Employee,
            product: RemoteConfigProduct::ApmTracing,
            config_id: "1234".to_string(),
            name: "config".to_string(),
        };
        pub static ref PATH_SECOND: RemoteConfigPath = RemoteConfigPath {
            source: RemoteConfigSource::Employee,
            product: RemoteConfigProduct::ApmTracing,
            config_id: "9876".to_string(),
            name: "config".to_string(),
        };
        pub static ref DUMMY_TARGET: Arc<Target> = Arc::new(Target {
            service: "service".to_string(),
            env: "env".to_string(),
            app_version: "1.3.5".to_string(),
            tags: vec![],
        });
    }

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
            Arc::new(ConfigFetcherState::new(server.dummy_invariants())),
        );
        let mut opaque_state = ConfigClientState::default();

        let mut response = Response::new(Body::from(""));
        *response.status_mut() = StatusCode::NOT_FOUND;
        *server.next_response.lock().unwrap() = Some(response);

        let fetched = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                DUMMY_TARGET.clone(),
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
            products: vec![
                RemoteConfigProduct::ApmTracing,
                RemoteConfigProduct::LiveDebugger,
            ],
            capabilities: vec![RemoteConfigCapabilities::ApmTracingCustomTags],
        };

        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::new(invariants)),
        );
        let mut opaque_state = ConfigClientState::default();

        {
            opaque_state.last_error = Some("test".to_string());
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    DUMMY_TARGET.clone(),
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
            assert_eq!(client.products, &["APM_TRACING", "LIVE_DEBUGGING"]);
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
                    DUMMY_TARGET.clone(),
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
            assert_eq!(client.products, &["APM_TRACING", "LIVE_DEBUGGING"]);
            assert!(client.is_tracer);
            assert!(!client.is_agent);
            assert_eq!(client.id, "foo");

            let state = client.state.as_ref().unwrap();
            assert!(!state.has_error);
            assert!(!state.config_states.is_empty());
            assert!(!state.backend_client_state.is_empty());

            let cached = &req.cached_target_files[0];
            assert_eq!(cached.path, PATH_FIRST.to_string());
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
                    DUMMY_TARGET.clone(),
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
                    DUMMY_TARGET.clone(),
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
                    DUMMY_TARGET.clone(),
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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_capability_encoding() {
        let state = ConfigFetcherState::<()>::new(ConfigInvariants {
            language: "".to_string(),
            tracer_version: "".to_string(),
            endpoint: Default::default(),
            products: vec![],
            capabilities: unsafe {
                vec![
                    transmute::<u32, RemoteConfigCapabilities>(1u32),
                    transmute::<u32, RemoteConfigCapabilities>(24u32),
                    transmute::<u32, RemoteConfigCapabilities>(31u32),
                ]
            },
        });
        assert_eq!(state.encoded_capabilities.len(), 4);
        assert_eq!(
            state.encoded_capabilities,
            (2u32 | 1 << 24 | 1 << 31).to_be_bytes()
        );
    }
}
