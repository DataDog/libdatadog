// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::one_way_shared_memory::{
    open_named_shm, OneWayShmReader, OneWayShmWriter, ReaderOpener,
};
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use datadog_remote_config::{RemoteConfigPath, RemoteConfigValue};
use datadog_trace_protobuf::remoteconfig::{
    ClientGetConfigsRequest, ClientGetConfigsResponse, ClientState, ClientTracer,
};
use ddcommon::{connector, Endpoint};
use futures::FutureExt;
use http::uri::{PathAndQuery, Scheme};
use http::StatusCode;
use hyper::{Body, Client};
use manual_future::{ManualFuture, ManualFutureCompleter};
use std::collections::hash_map::{DefaultHasher, Entry};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::default::Default;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::select;
use tokio::time::{sleep, Instant};
use tracing::log::error;
use zwohash::{HashSet, ZwoHasher};

const PROD_INTAKE_SUBDOMAIN: &str = "config";

#[derive(Clone, Hash, Eq, PartialEq)]
pub struct RemoteConfigIdentifier {
    pub language: String,
    pub tracer_version: String,
    pub endpoint: Endpoint,
}

pub struct RemoteConfigWriter(OneWayShmWriter<NamedShmHandle>);
pub struct RemoteConfigReader(OneWayShmReader<NamedShmHandle, Option<RemoteConfigIdentifier>>);

fn path_for_remote_config(id: &RemoteConfigIdentifier) -> CString {
    // We need a stable hash so that the outcome is independent of the process
    let mut hasher = ZwoHasher::default();
    id.hash(&mut hasher);
    CString::new(format!("/libdatadog-remote-config-{}", hasher.finish())).unwrap()
}

impl RemoteConfigReader {
    pub fn new(id: RemoteConfigIdentifier) -> RemoteConfigReader {
        RemoteConfigReader(OneWayShmReader::new(
            open_named_shm(&path_for_remote_config(&id)).ok(),
            Some(id),
        ))
    }

    pub fn read(&mut self) -> (bool, &[u8]) {
        self.0.read()
    }
}

impl RemoteConfigWriter {
    pub fn new(id: &RemoteConfigIdentifier) -> io::Result<RemoteConfigWriter> {
        Ok(RemoteConfigWriter(OneWayShmWriter::<NamedShmHandle>::new(
            path_for_remote_config(id),
        )?))
    }

    pub fn write(&self, contents: &[u8]) {
        self.0.write(contents)
    }
}

impl ReaderOpener<NamedShmHandle>
    for OneWayShmReader<NamedShmHandle, Option<RemoteConfigIdentifier>>
{
    fn open(&self) -> Option<MappedMem<NamedShmHandle>> {
        self.extra
            .as_ref()
            .and_then(|id| open_named_shm(&path_for_remote_config(id)).ok())
    }
}

enum ServiceRefcount {
    InUse(u32),
    Removed(Instant),
}

#[derive(Default)]
struct RemoteConfigServices {
    /// All services by envs in use - the instant is the mapping into last_service_used, if it exists
    services_by_env: HashMap<String, HashMap<String, ServiceRefcount>>,
    /// Ensure we may free old services
    last_used: BTreeMap<Instant, (String, String)>,
}

#[derive(Default)]
pub struct RemoteConfigs {
    /// Only instantiated if agent has remote config enabled at all
    writer: Mutex<Option<RemoteConfigWriter>>,
    /// All config file names to apply; TBD: matching by service / env
    configs: Mutex<Vec<String>>,
    /// Keyed by runtime_id
    runtimes: Mutex<HashMap<String, RemoteConfigInfo>>,
    pub remote_config_timeout: AtomicU64,
    pub remote_config_interval: AtomicU64,
    services: Mutex<RemoteConfigServices>,
    /// One SHM file per config, keyed by id. SHM Files are named by a hash of their contents and immutable.
    target_files_by_id: Mutex<HashMap<String, (u64, NamedShmHandle)>>,
}

struct RemoteConfigInfo {
    id: RemoteConfigIdentifier,
    service: String,
    env: String,
    app_version: String,
    complete: ManualFutureCompleter<()>,
}

// we collect services per env, so that we always query, for each runtime + env, all the services
// adding runtimes increases amount of services, removing services after a while

// one request per (runtime_id, RemoteConfigIdentifier) tuple: extra_services are all services pertaining to that env
// refcounting RemoteConfigIdentifier tuples by their unique runtime_id

impl RemoteConfigs {
    fn remove_service(services: &mut RemoteConfigServices, env: &str, service: &str) {
        if let Some(services_refcounts) = services.services_by_env.get_mut(env) {
            if let Entry::Occupied(mut service_refcount) = services_refcounts.entry(service.into())
            {
                if let ServiceRefcount::InUse(refcount) = service_refcount.get() {
                    service_refcount.insert(if *refcount == 1 {
                        let now = Instant::now();
                        services.last_used.insert(now, (env.into(), service.into()));
                        ServiceRefcount::Removed(now)
                    } else {
                        ServiceRefcount::InUse(refcount - 1)
                    });
                }
            }
        }
    }

    fn add_service(services: &mut RemoteConfigServices, env: &str, service: &str) {
        match services.services_by_env.entry(env.into()) {
            Entry::Occupied(mut e) => match e.get_mut().entry(service.into()) {
                Entry::Occupied(mut e) => match e.get() {
                    ServiceRefcount::InUse(refcount) => {
                        e.insert(ServiceRefcount::InUse(refcount + 1));
                    }
                    ServiceRefcount::Removed(instant) => {
                        services.last_used.remove(instant);
                        e.insert(ServiceRefcount::InUse(1));
                    }
                },
                Entry::Vacant(e) => {
                    e.insert(ServiceRefcount::InUse(1));
                }
            },
            Entry::Vacant(e) => {
                e.insert(HashMap::from([(service.into(), ServiceRefcount::InUse(1))]));
            }
        }
    }

    pub fn add_runtime(
        self: &Arc<Self>,
        id: RemoteConfigIdentifier,
        runtime_id: String,
        env: String,
        service: String,
        app_version: String,
    ) {
        match self.runtimes.lock().unwrap().entry(runtime_id) {
            Entry::Occupied(mut e) => {
                let active_service = &e.get().service;
                let active_env = &e.get().env;
                if active_service != &service || active_env != &env {
                    let mut services = self.services.lock().unwrap();
                    Self::remove_service(&mut services, active_env, active_service);
                    Self::add_service(&mut services, &env, &service);
                    e.get_mut().service = service;
                    e.get_mut().env = env;
                }
                e.get_mut().app_version = app_version;
            }
            Entry::Vacant(e) => {
                if id.endpoint.url.scheme().map(|s| s.as_str() != "file") == Some(true) {
                    Self::add_service(&mut self.services.lock().unwrap(), &env, &service);
                    let complete = self.read_to_shm(e.key().clone());
                    e.insert(RemoteConfigInfo {
                        id,
                        complete,
                        service,
                        env,
                        app_version,
                    });
                }
            }
        }
    }

    pub fn delete_runtime(self: &Arc<Self>, runtime_id: &str) {
        if let Some(info) = self.runtimes.lock().unwrap().remove(runtime_id) {
            tokio::spawn(info.complete.complete(()));
            Self::remove_service(&mut self.services.lock().unwrap(), &info.env, &info.service);
        }
    }

    // TODO: interrupt mechanism for clients to notify about updates
    fn read_to_shm(self: &Arc<Self>, runtime_id: String) -> ManualFutureCompleter<()> {
        let (future, completer) = ManualFuture::new();

        let this = self.clone();
        tokio::spawn(async move {
            let future = future.shared();
            let id = match this.runtimes.lock().unwrap().get(&runtime_id) {
                None => return,
                Some(endpoint) => endpoint,
            }
            .id
            .clone();
            let endpoint = get_product_endpoint(PROD_INTAKE_SUBDOMAIN, &id.endpoint);

            let config_id = uuid::Uuid::new_v4().to_string();

            loop {
                select! {
                    _ = future.clone() => { break }
                    // TODO: RC itself also provides a refresh interval hint - can we make use of that?
                    _ = sleep(Duration::from_millis(this.remote_config_interval.load(Ordering::Relaxed))) => {}
                }

                async fn fetch_once(
                    rc: Arc<RemoteConfigs>,
                    endpoint: &Endpoint,
                    id: &RemoteConfigIdentifier,
                    runtime_id: &str,
                    config_id: &str,
                ) -> anyhow::Result<()> {
                    let (service, env, app_version) = {
                        if let Some(RemoteConfigInfo {
                            service,
                            env,
                            app_version,
                            ..
                        }) = rc.runtimes.lock().unwrap().get(runtime_id)
                        {
                            (service.clone(), env.clone(), app_version.clone())
                        } else {
                            return Ok(());
                        }
                    };

                    // TODO: more fields, incl. cached_target_files
                    let config_req = ClientGetConfigsRequest {
                        client: Some(datadog_trace_protobuf::remoteconfig::Client {
                            state: Some(ClientState {
                                root_version: 1,
                                targets_version: 0,
                                config_states: vec![],
                                has_error: false,
                                error: "".to_string(),
                                backend_client_state: vec![], // TODO: set that
                            }),
                            id: config_id.into(),
                            // TODO all requested products
                            products: vec!["LIVE_DEBUGGING".to_string()],
                            is_tracer: true,
                            client_tracer: Some(ClientTracer {
                                runtime_id: runtime_id.to_string(),
                                language: id.language.to_string(),
                                tracer_version: id.tracer_version.clone(),
                                service,
                                extra_services: rc
                                    .services
                                    .lock()
                                    .unwrap()
                                    .services_by_env
                                    .get(&env)
                                    .map(|s| s.keys().map(Into::into).collect())
                                    .unwrap_or(vec![]),
                                env,
                                app_version,
                                tags: vec![],
                            }),
                            is_agent: false,
                            client_agent: None,
                            last_seen: 0,
                            capabilities: vec![],
                        }),
                        cached_target_files: vec![],
                    };
                    let json = serde_json::to_string(&config_req)?;

                    // TODO: directly talking to endpoint
                    let req = endpoint
                        .into_request_builder(concat!("Sidecar/", env!("CARGO_PKG_VERSION")))?;
                    let response = Client::builder()
                        .build(connector::Connector::default())
                        .request(req.body(Body::from(json))?)
                        .await?;
                    let status = response.status();
                    let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                    if status != StatusCode::OK {
                        let response_body =
                            String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                        anyhow::bail!("Server did not accept traces: {response_body}");
                    }

                    // Agent remote config not active or broken or similar
                    if body_bytes.len() <= 3 {
                        return Ok(());
                    }

                    {
                        let response: ClientGetConfigsResponse =
                            serde_json::from_str(&String::from_utf8_lossy(body_bytes.as_ref()))?;

                        let mut target_files = rc.target_files_by_id.lock().unwrap();
                        for file in response.target_files {
                            let mut hasher = DefaultHasher::new();
                            file.raw.hash(&mut hasher);
                            let hash = hasher.finish();
                            if let Some((old_hash, _)) = target_files.get(&file.path) {
                                if *old_hash == hash {
                                    continue;
                                }
                            }

                            if let Ok(decoded) = base64::engine::general_purpose::STANDARD
                                .decode(file.raw.as_slice())
                            {
                                let name = format!(
                                    "/libdatadog-remote-config-file-{}-{}",
                                    hash,
                                    BASE64_URL_SAFE_NO_PAD.encode(&file.path)
                                );
                                let mut handle =
                                    NamedShmHandle::create(CString::new(name)?, decoded.len())?
                                        .map()?;
                                handle.as_slice_mut().copy_from_slice(decoded.as_slice());
                                target_files.insert(file.path, (hash, handle.into()));
                            }
                        }

                        // TODO: Intelligent logic mapping which service/env/runtime_id get which configs
                        let mut configs = vec![];
                        for config in response.client_configs {
                            if let Some((_, target_file)) = target_files.get(&config) {
                                configs.push(
                                    String::from_utf8_lossy(target_file.get_path()).to_string(),
                                );
                            }
                        }
                        configs.sort();
                        *rc.configs.lock().unwrap() = configs;
                    }

                    // TODO: A writer per endpoint
                    let mut writer = rc.writer.lock().unwrap();
                    let writer = if let Some(writer) = writer.as_ref() {
                        writer
                    } else {
                        writer.insert(RemoteConfigWriter::new(&id)?)
                    };

                    let serialized = {
                        let configs = rc.configs.lock().unwrap();
                        serde_json::to_string(&*configs)?
                    };

                    if writer.0.as_slice() != serialized.as_bytes() {
                        writer.write(serialized.as_bytes());
                    }
                    Ok(())
                }
                if let Err(e) =
                    fetch_once(this.clone(), &endpoint, &id, &runtime_id, &config_id).await
                {
                    error!("{}", e.to_string())
                }
            }
        });

        completer
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

fn read_config(path: &str) -> anyhow::Result<Option<RemoteConfigValue>> {
    let mapped = NamedShmHandle::open(&CString::new(path)?)?.map()?;

    if let Some(rc_path) = path.split("-").nth(5) {
        let rc_path = String::from_utf8(BASE64_URL_SAFE_NO_PAD.decode(rc_path)?)?;
        Ok(RemoteConfigValue::try_parse(&rc_path, mapped.as_slice()))
    } else {
        anyhow::bail!("could not read config; {} has less than six dashes", path);
    }
}

pub struct RemoteConfigManager {
    reader: RemoteConfigReader,
    active_configs: HashSet<String>,
    last_read_configs: BTreeSet<String>,
    check_configs: Vec<String>,
}

pub enum RemoteConfigUpdate {
    None,
    Update(RemoteConfigValue),
    Remove(RemoteConfigPath),
}

impl RemoteConfigManager {
    pub fn new(id: RemoteConfigIdentifier) -> RemoteConfigManager {
        RemoteConfigManager {
            reader: RemoteConfigReader::new(id),
            active_configs: HashSet::default(),
            last_read_configs: BTreeSet::default(),
            check_configs: vec![],
        }
    }

    pub fn fetch_update(&mut self) -> RemoteConfigUpdate {
        let (changed, data) = self.reader.read();
        if changed {
            if let Ok(configs) = serde_json::from_slice(data) {
                self.last_read_configs = configs;
                self.check_configs = self.active_configs.iter().map(Into::into).collect();
            }
        }

        while let Some(config) = self.check_configs.pop() {
            if !self.last_read_configs.contains(&config) {
                self.active_configs.remove(&config);
                return RemoteConfigUpdate::Remove(RemoteConfigPath::try_parse(&config).unwrap());
            }
        }

        // pop_last is still unstable
        while let Some(config) = self.last_read_configs.iter().nth(0).map(Clone::clone) {
            self.last_read_configs.remove(&config);
            if !self.active_configs.contains(&config) {
                if let Some(parsed) = read_config(&config).ok().and_then(|o| o) {
                    self.active_configs.insert(config);
                    return RemoteConfigUpdate::Update(parsed);
                }
            }
        }

        RemoteConfigUpdate::None
    }
}
