use crate::targets::TargetsList;
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use base64::Engine;
use datadog_trace_protobuf::remoteconfig::{
    ClientGetConfigsRequest, ClientGetConfigsResponse, ClientState, ClientTracer, ConfigState,
    TargetFileHash, TargetFileMeta,
};
use ddcommon::{connector, Endpoint};
use hyper::http::uri::{PathAndQuery, Scheme};
use hyper::{Client, StatusCode};
use sha2::{Digest, Sha256, Sha512};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
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
        path: RemoteConfigPath,
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
#[derive(Clone, Hash, Eq, PartialEq)]
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
}

pub struct ConfigFetcherState<S> {
    target_files_by_path: Mutex<HashMap<String, StoredTargetFile<S>>>,
    pub invariants: ConfigInvariants,
    endpoint: Endpoint,
    pub expire_unused_files: bool,
}

pub struct ConfigFetcherFilesLock<'a, S> {
    inner: MutexGuard<'a, HashMap<String, StoredTargetFile<S>>>,
}

impl<'a, S> ConfigFetcherFilesLock<'a, S> {
    pub fn expire_file(&mut self, path: &RemoteConfigPath) {
        self.inner.remove(&path.to_string());
    }
}

impl<S> ConfigFetcherState<S> {
    pub fn new(invariants: ConfigInvariants) -> Self {
        ConfigFetcherState {
            target_files_by_path: Default::default(),
            endpoint: get_product_endpoint(PROD_INTAKE_SUBDOMAIN, &invariants.endpoint),
            invariants,
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
}

pub struct ConfigFetcher<S: FileStorage> {
    pub file_storage: S,
    state: Arc<ConfigFetcherState<S::StoredFile>>,
    /// Timeout after which to report failure, in milliseconds.
    pub timeout: AtomicU32,
    /// Collected interval. May be zero if not provided by the remote config server or fetched yet.
    /// Given in nanoseconds.
    pub interval: AtomicU64,
}

#[derive(Default)]
pub struct OpaqueState {
    client_state: Vec<u8>,
}

impl<S: FileStorage> ConfigFetcher<S> {
    pub fn new(file_storage: S, state: Arc<ConfigFetcherState<S::StoredFile>>) -> Self {
        ConfigFetcher {
            file_storage,
            state,
            timeout: AtomicU32::new(5000),
            interval: AtomicU64::new(0),
        }
    }

    /// Quite generic fetching implementation:
    ///  - runs a request against the Remote Config Server,
    ///  - validates the data,
    ///  - removes unused files
    ///  - checks if the files are already known,
    ///  - stores new files,
    ///  - returns all currently active files.
    /// It also makes sure that old files are dropped before new files are inserted.
    ///
    /// Returns None if nothing changed. Otherwise Some(active configs).
    pub async fn fetch_once(
        &mut self,
        runtime_id: &str,
        target: Arc<Target>,
        config_id: &str,
        last_error: Option<String>,
        opaque_state: &mut OpaqueState,
    ) -> anyhow::Result<Option<Vec<Arc<S::StoredFile>>>> {
        if self.state.endpoint.api_key.is_some() {
            // Using remote config talking to the backend directly is not supported.
            return Ok(Some(vec![]));
        }

        let Target {
            service,
            env,
            app_version,
        } = (*target).clone();

        let mut cached_target_files = vec![];
        let mut config_states = vec![];

        for StoredTargetFile { state, meta, .. } in
            self.state.target_files_by_path.lock().unwrap().values()
        {
            config_states.push(state.clone());
            cached_target_files.push(meta.clone());
        }

        let config_req = ClientGetConfigsRequest {
            client: Some(datadog_trace_protobuf::remoteconfig::Client {
                state: Some(ClientState {
                    root_version: 1,
                    targets_version: 0,
                    config_states,
                    has_error: last_error.is_some(),
                    error: last_error.unwrap_or_default(),
                    backend_client_state: std::mem::take(&mut opaque_state.client_state),
                }),
                id: config_id.into(),
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
                    tags: vec![],
                }),
                is_agent: false,
                client_agent: None,
                last_seen: 0,
                capabilities: self
                    .state
                    .invariants
                    .capabilities
                    .iter()
                    .map(|c| *c as u8)
                    .collect(),
            }),
            cached_target_files,
        };

        let req = self
            .state
            .endpoint
            .into_request_builder(concat!("Sidecar/", env!("CARGO_PKG_VERSION")))?
            .method(http::Method::POST)
            .header(
                http::header::CONTENT_TYPE,
                ddcommon::header::APPLICATION_JSON,
            )
            .body(serde_json::to_string(&config_req)?)?;
        let response = Client::builder()
            .build(connector::Connector::default())
            .request(req)
            .await
            .map_err(|e| {
                anyhow::Error::msg(e).context(format!("Url: {:?}", self.state.endpoint))
            })?;
        let status = response.status();
        let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
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

        opaque_state.client_state = targets_list
            .signed
            .custom
            .opaque_backend_state
            .as_bytes()
            .to_vec();
        if let Some(interval) = targets_list.signed.custom.agent_refresh_interval {
            self.interval.store(interval, Ordering::Relaxed);
        }

        trace!(
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

        if self.state.expire_unused_files {
            let retain: HashSet<_> = response.client_configs.iter().collect();
            target_files.retain(|k, _| retain.contains(k));
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
            let handle = if let Some(StoredTargetFile {
                hash: old_hash,
                handle,
                ..
            }) = target_files.get(path)
            {
                if old_hash == hash {
                    continue;
                }
                Some(handle.clone())
            } else {
                None
            };
            if let Some(raw_file) = incoming_files.get(path) {
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(raw_file) {
                    let computed_hash = hasher(decoded.as_slice());
                    if hash != computed_hash {
                        warn!("Computed hash of file {computed_hash} did not match remote config targets file hash {hash} for path {path}: file: {}", String::from_utf8_lossy(decoded.as_slice()));
                        continue;
                    }

                    match RemoteConfigPath::try_parse(path) {
                        Ok(parsed_path) => {
                            if let Some(version) = target_file.try_parse_version() {
                                debug!("Fetched new remote config file at path {path} targeting {target:?}");

                                target_files.insert(
                                    path.to_string(),
                                    StoredTargetFile {
                                        hash: computed_hash,
                                        state: ConfigState {
                                            id: parsed_path.config_id.to_string(),
                                            version,
                                            product: parsed_path.product.to_string(),
                                            apply_state: 0,
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
                                            self.file_storage.store(
                                                version,
                                                parsed_path,
                                                decoded,
                                            )?
                                        },
                                    },
                                );
                            } else {
                                warn!("Failed parsing version from remote config path {path}");
                            }
                        }
                        Err(e) => {
                            warn!("Failed parsing remote config path: {path} - {e:?}");
                        }
                    }
                } else {
                    warn!(
                        "Failed base64 decoding config for path {path}: {}",
                        String::from_utf8_lossy(raw_file)
                    )
                }
            } else {
                warn!(
                    "Found changed config data for path {path}, but no file; existing files: {:?}",
                    incoming_files.keys().collect::<Vec<_>>()
                )
            }
        }

        let mut configs = Vec::with_capacity(response.client_configs.len());
        for config in response.client_configs.iter() {
            if let Some(StoredTargetFile { handle, .. }) = target_files.get(config) {
                configs.push(handle.clone());
            }
        }

        Ok(Some(configs))
    }
}

fn get_product_endpoint(subdomain: &str, endpoint: &Endpoint) -> Endpoint {
    let mut parts = endpoint.url.clone().into_parts();
    if endpoint.api_key.is_some() {
        if parts.scheme.is_none() {
            parts.scheme = Some(Scheme::HTTPS);
            parts.authority = Some(
                format!("{}.{}", subdomain, parts.authority.unwrap())
                    .parse()
                    .unwrap(),
            );
        }
        parts.path_and_query = Some(PathAndQuery::from_static("/api/v0.1/configurations"));
    } else {
        parts.path_and_query = Some(PathAndQuery::from_static("/v0.7/config"));
    }
    Endpoint {
        url: hyper::Uri::from_parts(parts).unwrap(),
        api_key: endpoint.api_key.clone(),
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
        });
    }

    static DUMMY_RUNTIME_ID: &'static str = "3b43524b-a70c-45dc-921d-34504e50c5eb";

    #[derive(Default)]
    pub struct Storage {
        pub files: Mutex<HashMap<RemoteConfigPath, Arc<Mutex<DataStore>>>>,
    }

    pub struct PathStore {
        path: RemoteConfigPath,
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
            path: RemoteConfigPath,
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
    async fn test_inactive() {
        let server = RemoteConfigServer::spawn();
        let storage = Arc::new(Storage::default());
        let mut fetcher = ConfigFetcher::new(
            storage.clone(),
            Arc::new(ConfigFetcherState::new(server.dummy_invariants())),
        );
        let mut opaque_state = OpaqueState::default();

        let mut response = Response::new(Body::from(""));
        *response.status_mut() = StatusCode::NOT_FOUND;
        *server.next_response.lock().unwrap() = Some(response);

        let fetched = fetcher
            .fetch_once(
                DUMMY_RUNTIME_ID,
                DUMMY_TARGET.clone(),
                "foo",
                Some("test".to_string()),
                &mut opaque_state,
            )
            .await
            .unwrap()
            .unwrap();

        assert!(fetched.is_empty());
    }

    #[tokio::test]
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
        let mut opaque_state = OpaqueState::default();

        {
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    DUMMY_TARGET.clone(),
                    "foo",
                    Some("test".to_string()),
                    &mut opaque_state,
                )
                .await
                .unwrap()
                .unwrap();

            let req = server.last_request.lock().unwrap();
            let req = req.as_ref().unwrap();
            assert!(req.cached_target_files.is_empty());

            let client = req.client.as_ref().unwrap();
            assert_eq!(
                client.capabilities,
                &[RemoteConfigCapabilities::ApmTracingCustomTags as u8]
            );
            assert_eq!(client.products, &["APM_TRACING", "LIVE_DEBUGGING"]);
            assert_eq!(client.is_tracer, true);
            assert_eq!(client.is_agent, false);
            assert_eq!(client.id, "foo");

            let state = client.state.as_ref().unwrap();
            assert_eq!(state.error, "test");
            assert_eq!(state.has_error, true);
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
                String::from_utf8_lossy(&opaque_state.client_state),
                "some state"
            );
            assert_eq!(fetched.len(), 1);
            assert_eq!(storage.files.lock().unwrap().len(), 1);

            assert!(Arc::ptr_eq(
                &fetched[0].data,
                storage.files.lock().unwrap().get(&PATH_FIRST).unwrap()
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
                    None,
                    &mut opaque_state,
                )
                .await
                .unwrap();
            assert!(fetched.is_none()); // no change

            let req = server.last_request.lock().unwrap();
            let req = req.as_ref().unwrap();
            assert_eq!(req.cached_target_files.len(), 1);

            let client = req.client.as_ref().unwrap();
            assert_eq!(
                client.capabilities,
                &[RemoteConfigCapabilities::ApmTracingCustomTags as u8]
            );
            assert_eq!(client.products, &["APM_TRACING", "LIVE_DEBUGGING"]);
            assert_eq!(client.is_tracer, true);
            assert_eq!(client.is_agent, false);
            assert_eq!(client.id, "foo");

            let state = client.state.as_ref().unwrap();
            assert_eq!(state.error, "test");
            assert_eq!(state.has_error, true);
            assert!(state.config_states.is_empty());
            assert!(state.backend_client_state.is_empty());

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
                    None,
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
                storage.files.lock().unwrap().get(&PATH_FIRST).unwrap()
            ));
            assert_eq!(fetched[first].data.lock().unwrap().contents, "v2");
            assert_eq!(fetched[first].data.lock().unwrap().version, 2);

            assert!(Arc::ptr_eq(
                &fetched[second].data,
                storage.files.lock().unwrap().get(&PATH_SECOND).unwrap()
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
                    None,
                    &mut opaque_state,
                )
                .await
                .unwrap();
            assert!(fetched.is_none()); // no change
        }

        server.files.lock().unwrap().remove(&PATH_FIRST);

        {
            let fetched = fetcher
                .fetch_once(
                    DUMMY_RUNTIME_ID,
                    DUMMY_TARGET.clone(),
                    "foo",
                    None,
                    &mut opaque_state,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fetched.len(), 1);
            assert_eq!(storage.files.lock().unwrap().len(), 1);
        }
    }
}
