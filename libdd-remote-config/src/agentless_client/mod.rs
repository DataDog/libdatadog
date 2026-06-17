// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, format_err};
use base64::Engine;
use futures::AsyncReadExt as _;
use hashbrown::{HashMap, HashSet};
use http::{
    header,
    uri::{Authority, PathAndQuery},
    Method, Request, Uri,
};
use libdd_capabilities::{Bytes, HttpClientCapability};
use libdd_common::Endpoint;
use libdd_trace_protobuf::remoteconfig;
use prost::Message;
use serde_json::Value;
use tracing::debug;
use tuf::repository::RepositoryStorage;
use tuf::{
    metadata::{
        Metadata, MetadataPath, MetadataVersion, RawSignedMetadata, TargetDescription, TargetPath,
    },
    repository::RepositoryProvider as _,
};

#[allow(dead_code)] // used in tests and reserved for TUF config-repo init
const CONFIG_ROOT: &[u8] = include_bytes!("../../roots/prod/config_root.json");
const CONFIG_ROOT_VERSION: u64 = 16;
const DIRECTOR_ROOT: &[u8] = include_bytes!("../../roots/prod/director_root.json");
const DIRECTOR_ROOT_VERSION: u64 = 15;

const FAKE_AGENT_VERSION: &str = "7.78.4";

type TUFRepo = tuf::repository::EphemeralRepository<tuf::interchange::Json>;
type TUFClient = tuf::client::Client<tuf::interchange::Json, TUFRepo, TUFRepo>;

// Make a remote config API endpoint from and endpoint where `e.url` is the base dd site
// If the endpoint is not suitable (api key not set, not https), returns N
pub fn make_agentless_configs_endpoint(e: &Endpoint) -> Option<Endpoint> {
    let e = e.clone();
    dbg!(&e);
    if !(e.url.scheme_str().is_some_and(|s| s == "https")
        && e.url.authority().is_some()
        && e.api_key.is_some())
    {
        return None;
    }

    let mut parts = e.url.into_parts();
    parts.authority =
        Some(Authority::try_from(format!("config.{}", parts.authority?.as_str())).ok()?);
    parts.path_and_query = Some(PathAndQuery::from_static("/api/v0.1/configurations"));

    Some(Endpoint {
        url: Uri::from_parts(parts).ok()?,
        ..e
    })
}

#[derive(Clone)]
pub struct AgentlessConfig {
    pub hostname: String,
}

pub type NativeAgentlessFetcher = AgentlessFetcher<libdd_capabilities_impl::NativeHttpClient>;

pub struct AgentlessFetcher<C: HttpClientCapability> {
    http: C,
    initialized: bool,
    opaque_backend_state: Vec<u8>,
    director_client: TUFClient,
    config_client: TUFClient,
    hostname: String,
    products: HashSet<String>,
    refresh_interval: Duration,
    endpoint: Endpoint,
    // TODO: Not sure this is needed if the wrapped client already caches files?
    target_cache: HashMap<tuf::metadata::TargetPath, CachedFile>,
}

struct CachedFile {
    hashes: Vec<(&'static tuf::crypto::HashAlgorithm, tuf::crypto::HashValue)>,
    target_file: Vec<u8>,
    version: u64,
}

pub struct ClientTargetResponse<'a> {
    pub path: &'a str,
    pub version: u64,
    pub hashes: &'a [(&'static tuf::crypto::HashAlgorithm, tuf::crypto::HashValue)],
    pub content: &'a [u8],
}

pub struct ClientResponse<'a> {
    pub root_version: u64,
    pub target_version: u64,
    pub opaque_backend_state: &'a [u8],
    pub targets: Vec<ClientTargetResponse<'a>>,
    pub refresh_interval: Duration,
}

struct BorrowedTufTarget<'a> {
    pub path: &'a tuf::metadata::TargetPath,
    pub desc: &'a tuf::metadata::TargetDescription,
}

const CUSTOM_METADATA_EXPIRY_PATH: &str = "expires";

impl<'a> BorrowedTufTarget<'a> {
    pub fn try_create(path: &'a TargetPath, desc: &'a TargetDescription) -> anyhow::Result<Self> {
        if let Some(expiry) = desc.custom().get(CUSTOM_METADATA_EXPIRY_PATH) {
            let expiry_ts = expiry
                .as_u64()
                .ok_or_else(|| format_err!("expiry not a number"))?;

            if expiry_ts * 1000 <= now_unix_milli_ts() {
                bail!("expired target at path: {path}")
            }
        }

        Ok(Self { path, desc })
    }
}

enum FetchTargetResult {
    Cached,
    New(CachedFile),
}

impl<C: HttpClientCapability + Send + Sync> AgentlessFetcher<C> {
    /// Create a new `AgentlessFetcher` client.
    ///
    /// # Errors
    /// Returns an error if TUF root initialization fails.
    /// This can happen for instance if the trust root certificates have expired
    pub async fn new(cfg: AgentlessConfig, endpoint: Endpoint) -> anyhow::Result<Self> {
        Ok(Self {
            endpoint,
            http: C::new_client(),
            director_client: TUFClient::with_trusted_root(
                tuf::client::Config::default(),
                &RawSignedMetadata::new(DIRECTOR_ROOT.to_vec()),
                TUFRepo::new(),
                TUFRepo::new(),
            )
            .await?,
            config_client: TUFClient::with_trusted_root(
                tuf::client::Config::default(),
                &RawSignedMetadata::new(CONFIG_ROOT.to_vec()),
                TUFRepo::new(),
                TUFRepo::new(),
            )
            .await?,
            hostname: cfg.hostname,
            products: HashSet::new(),
            target_cache: HashMap::new(),

            opaque_backend_state: Vec::new(),
            refresh_interval: Duration::from_secs(60),
            initialized: false,
        })
    }

    /// Return the value of a particular target , checking both its length and
    /// hashes against the metadata in the config repo.
    ///
    /// If it is already in the cache, return `Cached`
    async fn fetch_target(
        &self,
        target: &BorrowedTufTarget<'_>,
    ) -> anyhow::Result<FetchTargetResult> {
        let expected_hashes = tuf::crypto::retain_supported_hashes(target.desc.hashes());
        if expected_hashes.is_empty() {
            bail!("no supported hash for path: {}", target.path);
        }
        let (target_hash_algo, target_hash) = &expected_hashes[0];
        let target_path = target.path;

        let version = target
            .desc
            .custom()
            .get("v")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        if let Some(item) = self.target_cache.get(target_path) {
            if item
                .hashes
                .iter()
                .find(|(alg, _)| alg == target_hash_algo)
                .is_some_and(|(_, h)| h == target_hash)
                && item.target_file.len() as u64 == target.desc.length()
            {
                return Ok(FetchTargetResult::Cached);
            }
        }

        // Fetch from the content from the remote __Unverified__ repo
        // This is fine as we are comparing the (hash + len) with a validated
        // target
        let mut read = self
            .director_client
            .remote_repo()
            .fetch_target(target_path)
            .await?;
        let mut buf = Vec::new();
        read.read_to_end(&mut buf).await?;

        let expected_len = target.desc.length() as usize;
        if buf.len() != expected_len {
            bail!("bad length for file at path: {}", target.path)
        }

        {
            let hash_algs = expected_hashes
                .iter()
                .map(|(alg, _val)| (*alg).clone())
                .collect::<Vec<_>>();
            let actual_hashes =
                tuf::crypto::calculate_hashes_from_slice(&buf, hash_algs.as_slice())?;
            let expected: HashMap<_, _> = expected_hashes
                .iter()
                .map(|(alg, val)| (alg, val))
                .collect();

            if !(actual_hashes.len() == expected.len()
                && actual_hashes
                    .iter()
                    .all(|(k, v)| expected.get(&k).is_some_and(|e| *e == v)))
            {
                bail!("hash did not match: {}", target.path)
            }
        }

        Ok(FetchTargetResult::New(CachedFile {
            hashes: expected_hashes,
            target_file: buf,
            version,
        }))
    }

    pub async fn fetch_config(
        &mut self,
        c: remoteconfig::Client,
    ) -> anyhow::Result<ClientResponse<'_>> {
        let (
            current_config_snapshot_version,
            current_config_root_version,
            current_director_root_version,
        ) = if self.initialized {
            (
                u64::from(
                    self.config_client
                        .database()
                        .trusted_snapshot()
                        .ok_or(anyhow::anyhow!("Missing snapshot data"))?
                        .version(),
                ),
                u64::from(self.config_client.database().trusted_root().version()),
                u64::from(self.director_client.database().trusted_root().version()),
            )
        } else {
            (0, CONFIG_ROOT_VERSION, DIRECTOR_ROOT_VERSION)
        };

        let all_products = c.products.iter().fold(HashSet::new(), |mut acc, p| {
            acc.get_or_insert_with(p, String::clone);
            acc
        });
        let new_products = all_products
            .difference(&self.products)
            .cloned()
            .collect::<Vec<_>>();
        let old_products = self
            .products
            .intersection(&all_products)
            .cloned()
            .collect::<Vec<_>>();

        let now = now_unix_milli_ts();

        let request = remoteconfig::LatestConfigsRequest {
            hostname: self.hostname.clone(),
            current_config_snapshot_version,
            current_config_root_version,
            current_director_root_version,
            products: old_products,
            new_products,
            backend_client_state: self.opaque_backend_state.clone(),
            active_clients: vec![remoteconfig::Client {
                last_seen: now,
                ..c
            }],
            agent_version: FAKE_AGENT_VERSION.to_owned(),
            has_error: false,
            error: String::new(),
            trace_agent_env: String::new(),
            org_uuid: String::new(),
            tags: vec![],
            agent_uuid: String::new(),
        };
        let response = self.get_latest_config(request).await?;

        self.apply(&response).await?;
        if !self.initialized {
            self.initialized = true;
        }
        self.products = all_products;

        // TODO:
        // In the future we will want to query configs for mutliple clients (for PHP, which can have
        // many processes use the same rc client)
        // This means we will need to dispatch the different files based on filter predicates
        // which we currently do not parse

        Ok(ClientResponse {
            root_version: u64::from(self.config_client.database().trusted_root().version()),
            target_version: u64::from(
                self.config_client
                    .database()
                    .trusted_targets()
                    .ok_or(anyhow::anyhow!("Missing target data"))?
                    .version(),
            ),
            opaque_backend_state: &self.opaque_backend_state,
            targets: self
                .target_cache
                .iter()
                .map(|(p, t)| ClientTargetResponse {
                    path: p.as_str(),
                    version: t.version,
                    hashes: &t.hashes,
                    content: t.target_file.as_slice(),
                })
                .collect(),
            refresh_interval: self.refresh_interval,
        })
    }

    /// Query the Remote Config org-status endpoint.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response cannot be decoded.
    pub async fn get_org_status(&self) -> anyhow::Result<remoteconfig::OrgStatusResponse> {
        let path = PathAndQuery::from_static("/api/v0.1/status");
        let res = self.send_request(Method::GET, path, Bytes::new()).await?;
        parse_rc_response(res)
    }

    pub async fn get_org_data(&self) -> anyhow::Result<remoteconfig::OrgDataResponse> {
        let path = PathAndQuery::from_static("/api/v0.1/org");
        let res = self.send_request(Method::GET, path, Bytes::new()).await?;
        parse_rc_response(res)
    }

    /// Fetch the latest Remote Config for this client.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response cannot be decoded.
    async fn get_latest_config(
        &self,
        req: remoteconfig::LatestConfigsRequest,
    ) -> anyhow::Result<remoteconfig::LatestConfigsResponse> {
        dbg!(&req);
        let path = PathAndQuery::from_static("/api/v0.1/configurations");
        let body = Bytes::from(req.encode_to_vec());
        let res = self.send_request(Method::POST, path, body).await?;
        let res = parse_rc_response(res)?;
        dbg!(debug_latest_configs_response(&res));
        Ok(res)
    }

    #[allow(clippy::future_not_send)]
    async fn send_request(
        &self,
        method: Method,
        path: PathAndQuery,
        body: Bytes,
    ) -> anyhow::Result<http::Response<Bytes>> {
        let req = self
            .endpoint
            .set_standard_headers(
                Request::builder(),
                concat!("Libdatadog/", env!("CARGO_PKG_VERSION")),
            )
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .uri(url_with_path(self.endpoint.url.clone(), path)?)
            .method(method)
            .body(body)?;
        Ok(self.http.request(req).await?)
    }

    async fn apply(
        &mut self,
        response: &remoteconfig::LatestConfigsResponse,
    ) -> anyhow::Result<()> {
        // At a high level, what we're doing here is populating the "remote" repos with the metadata
        // that we received from upstream (which does not validate it), and then using the clients'
        // `update` methods to synchronize that metadata to the "local" repos, during which
        // validation is performed.

        let root_path = MetadataPath::root();
        let timestamp_path = MetadataPath::timestamp();
        let snapshot_path = MetadataPath::snapshot();
        let targets_path = MetadataPath::targets();

        let repo = self.director_client.remote_repo_mut();
        *repo = TUFRepo::new();
        for target_file in &response.target_files {
            let trimmed_path = trim_hash_target_path(&target_file.path)?;
            let trimmed_target_path = TargetPath::new(&trimmed_path)?;
            repo.store_target(&trimmed_target_path, &mut target_file.raw.as_slice())
                .await?;

            // let trimmed_path = trim_hash_target_path(&target_file.path)?;
            // let trimmed_target_path = TargetPath::new(&trimmed_path)?;
            repo.store_target(
                &TargetPath::new(&target_file.path)?,
                &mut target_file.raw.as_slice(),
            )
            .await?;
        }

        let config_repo_mut = self.config_client.remote_repo_mut();
        *config_repo_mut = TUFRepo::new();
        let Some(metas) = response.config_metas.as_ref() else {
            bail!("missing config meta from LatestConfigsResponse")
        };

        store(config_repo_mut, &root_path, &metas.roots).await?;
        store_noversion(config_repo_mut, &timestamp_path, &metas.timestamp).await?;
        store(config_repo_mut, &snapshot_path, &metas.snapshot).await?;
        store(config_repo_mut, &targets_path, &metas.top_targets).await?;
        // TODO: We do not store the delegated targets metadata
        // This will need to be revisited in order to support proper Uptane
        // verification of the full configuration data.
        // store(repo, &targets_path, &metas.delegated_targets).await?;

        let director_remote_repo = self.director_client.remote_repo_mut();
        let Some(metas) = response.director_metas.as_ref() else {
            bail!("missing director meta from LatestConfigsResponse")
        };

        store(director_remote_repo, &root_path, &metas.roots).await?;
        store_noversion(director_remote_repo, &timestamp_path, &metas.timestamp).await?;
        store(director_remote_repo, &snapshot_path, &metas.snapshot).await?;
        store(director_remote_repo, &targets_path, &metas.targets).await?;

        self.config_client.update().await?;
        self.director_client.update().await?;

        let mut new_target_path_set = HashSet::new();
        for target in trusted_targets(&self.director_client)? {
            new_target_path_set.insert(target.path);
            match self.fetch_target(&target).await? {
                FetchTargetResult::Cached => {}
                FetchTargetResult::New(cached_target) => {
                    self.target_cache.insert(target.path.clone(), cached_target);
                }
            }
        }
        self.target_cache
            .retain(|key, _| new_target_path_set.contains(key));

        // The Remote Config service uses a `custom` field at the top-level of the targets metadata
        // to store this field which we are supposed to echo back to the server. That `custom` field
        // is not explicitly part of the TUF spec, which is why we need to pull it out of the
        // `additional_fields` catch-all here.
        if let Some((opaque_backend_state, refresh_interval)) =
            get_director_custom(&self.director_client)
        {
            if let Some(opaque_backend_state) = opaque_backend_state {
                self.opaque_backend_state = opaque_backend_state;
            }
            if let Some(refresh_interval) = refresh_interval {
                self.refresh_interval = refresh_interval;
            }
        }

        Ok(())
    }
}

fn get_director_custom(director_client: &TUFClient) -> Option<(Option<Vec<u8>>, Option<Duration>)> {
    let custom = director_client
        .database()
        .trusted_targets()?
        .additional_fields()
        .get("custom")?;

    Some((
        custom
            .get("opaque_backend_state")
            .and_then(Value::as_str)
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok()),
        custom
            .get("agent_refresh_interval")
            .and_then(Value::as_u64)
            .map(Duration::from_secs),
    ))
}

fn url_with_path(base: http::Uri, path: PathAndQuery) -> anyhow::Result<http::Uri> {
    let mut parts = base.into_parts();
    parts.path_and_query = Some(path);
    Ok(http::Uri::from_parts(parts)?)
}

fn parse_rc_response<T: prost::Message + Default>(
    response: http::Response<Bytes>,
) -> anyhow::Result<T> {
    let status = response.status().as_u16();
    let body = response.into_body();
    if !(200..300).contains(&status) {
        bail!(
            "Non 2XX status code: {}\n{}",
            status,
            String::from_utf8_lossy(&body)
        )
    }

    Ok(T::decode(body)?)
}

fn now_unix_milli_ts() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// Return the available, unexpired target paths and their descriptions based on the current
/// metadata.
fn trusted_targets(
    director_client: &TUFClient,
) -> anyhow::Result<impl Iterator<Item = BorrowedTufTarget<'_>> + '_> {
    Ok(director_client
        .database()
        .trusted_targets()
        .ok_or_else(|| format_err!("missing targets from TUF director client"))?
        .targets()
        .iter()
        .filter_map(|(path, desc)| {
            BorrowedTufTarget::try_create(path, desc)
                .inspect_err(|e| {
                    debug!(%path, "Skipping target: error {}", e);
                })
                .ok()
        }))
}

async fn store<'a, T>(repo: &mut TUFRepo, path: &MetadataPath, tms: T) -> anyhow::Result<()>
where
    T: IntoIterator<Item = &'a remoteconfig::TopMeta> + 'a,
{
    for tm in tms {
        repo.store_metadata(
            path,
            MetadataVersion::Number(tm.version as u32),
            &mut tm.raw.as_slice(),
        )
        .await?;
    }
    Ok(())
}

async fn store_noversion(
    repo: &mut TUFRepo,
    path: &MetadataPath,
    tms: &Option<remoteconfig::TopMeta>,
) -> anyhow::Result<()> {
    if let Some(tm) = tms {
        repo.store_metadata(path, MetadataVersion::None, &mut tm.raw.as_slice())
            .await?;
    }
    Ok(())
}

/// See https://datadoghq.atlassian.net/browse/RC-1859 for more information.
fn trim_hash_target_path(target_path: &str) -> anyhow::Result<String> {
    let path = std::path::Path::new(target_path);
    // Get the last component
    let last_component = path
        .components()
        .next_back()
        .ok_or_else(|| format_err!("invalid target: {target_path}"))?;
    let basename = match last_component {
        std::path::Component::Normal(name) => name
            .to_str()
            .ok_or_else(|| format_err!("invalid target: {target_path}"))?,
        _ => return Err(format_err!("invalid target: {target_path}")),
    };

    // Split the basename at the first occurrence of '.'
    let split: Vec<&str> = basename.splitn(2, '.').collect();
    let basename_trimmed = if split.len() > 1 { split[1] } else { basename };

    // Reconstruct the whole path
    let parent = path
        .parent()
        .ok_or_else(|| format_err!("invalid target: {target_path}"))?;
    let mut result_path = parent.components().as_path().to_path_buf();
    result_path.push(basename_trimmed);
    Ok(result_path.to_str().unwrap_or_default().to_string())
}

// ── Debug helpers: render `raw: Vec<u8>` fields as JSON ────────────────────

struct RawJson<'a>(&'a [u8]);

impl fmt::Debug for RawJson<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let RawJson(bytes) = self;
        match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(v) => write!(f, "{v:#}"),
            Err(_) => write!(f, "<{} non-JSON bytes>", bytes.len()),
        }
    }
}

struct DebugTopMeta<'a>(&'a remoteconfig::TopMeta);

impl fmt::Debug for DebugTopMeta<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remoteconfig::TopMeta { version, raw } = self.0;
        f.debug_struct("TopMeta")
            .field("version", version)
            .field("raw", &RawJson(raw))
            .finish()
    }
}

struct DebugDelegatedMeta<'a>(&'a remoteconfig::DelegatedMeta);

impl fmt::Debug for DebugDelegatedMeta<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remoteconfig::DelegatedMeta { version, role, raw } = self.0;
        f.debug_struct("DelegatedMeta")
            .field("version", version)
            .field("role", role)
            .field("raw", &RawJson(raw))
            .finish()
    }
}

struct DebugFile<'a>(&'a remoteconfig::File);

impl fmt::Debug for DebugFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remoteconfig::File { path, raw } = self.0;
        f.debug_struct("File")
            .field("path", path)
            .field("raw", &RawJson(raw))
            .finish()
    }
}

struct DebugConfigMetas<'a>(&'a remoteconfig::ConfigMetas);

impl fmt::Debug for DebugConfigMetas<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remoteconfig::ConfigMetas {
            roots,
            timestamp,
            snapshot,
            top_targets,
            delegated_targets,
        } = self.0;
        f.debug_struct("ConfigMetas")
            .field("roots", &roots.iter().map(DebugTopMeta).collect::<Vec<_>>())
            .field("timestamp", &timestamp.as_ref().map(DebugTopMeta))
            .field("snapshot", &snapshot.as_ref().map(DebugTopMeta))
            .field("top_targets", &top_targets.as_ref().map(DebugTopMeta))
            .field(
                "delegated_targets",
                &delegated_targets
                    .iter()
                    .map(DebugDelegatedMeta)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

struct DebugDirectorMetas<'a>(&'a remoteconfig::DirectorMetas);

impl fmt::Debug for DebugDirectorMetas<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remoteconfig::DirectorMetas {
            roots,
            timestamp,
            snapshot,
            targets,
        } = &self.0;
        f.debug_struct("DirectorMetas")
            .field("roots", &roots.iter().map(DebugTopMeta).collect::<Vec<_>>())
            .field("timestamp", &timestamp.as_ref().map(DebugTopMeta))
            .field("snapshot", &snapshot.as_ref().map(DebugTopMeta))
            .field("targets", &targets.as_ref().map(DebugTopMeta))
            .finish()
    }
}

struct DebugLatestConfigsResponse<'a>(&'a remoteconfig::LatestConfigsResponse);

impl fmt::Debug for DebugLatestConfigsResponse<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remoteconfig::LatestConfigsResponse {
            config_metas,
            director_metas,
            target_files,
        } = &self.0;
        f.debug_struct("LatestConfigsResponse")
            .field("config_metas", &config_metas.as_ref().map(DebugConfigMetas))
            .field(
                "director_metas",
                &director_metas.as_ref().map(DebugDirectorMetas),
            )
            .field(
                "target_files",
                &target_files.iter().map(DebugFile).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Returns a value that implements [`fmt::Debug`] for [`remoteconfig::LatestConfigsResponse`],
/// rendering every `raw` byte field as a parsed JSON value instead of a raw byte array.
///
/// Use with the standard formatting machinery:
///
/// ```rust,ignore
/// println!("{:#?}", debug_latest_configs_response(&response));
/// ```
pub fn debug_latest_configs_response(
    resp: &remoteconfig::LatestConfigsResponse,
) -> impl fmt::Debug + '_ {
    DebugLatestConfigsResponse(resp)
}

#[cfg(test)]
mod tests {
    use super::{CONFIG_ROOT, CONFIG_ROOT_VERSION, DIRECTOR_ROOT, DIRECTOR_ROOT_VERSION};

    #[test]
    fn test_root_version_match() {
        let config_root: serde_json::Value = serde_json::from_slice(CONFIG_ROOT).unwrap();
        assert_eq!(config_root["signed"]["version"], CONFIG_ROOT_VERSION);

        let director_root: serde_json::Value = serde_json::from_slice(DIRECTOR_ROOT).unwrap();
        assert_eq!(director_root["signed"]["version"], DIRECTOR_ROOT_VERSION);
    }
}
