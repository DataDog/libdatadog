// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::fetch::FileStorage;

use std::{
    borrow::Cow,
    fmt,
    ops::RangeInclusive,
    path::PathBuf,
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
use tracing::{debug, error, warn};
use tuf::repository::RepositoryStorage;
use tuf::{
    metadata::{
        Metadata, MetadataPath, MetadataVersion, RawSignedMetadata, TargetDescription, TargetPath,
    },
    repository::RepositoryProvider as _,
};

// Embedded TUF trust roots, per site
const PROD_CONFIG_ROOT: &[u8] = include_bytes!("../../roots/prod/config_root.json");

const PROD_DIRECTOR_ROOT: &[u8] = include_bytes!("../../roots/prod/director_root.json");

const STAGING_CONFIG_ROOT: &[u8] = include_bytes!("../../roots/staging/config_root.json");

const STAGING_DIRECTOR_ROOT: &[u8] = include_bytes!("../../roots/staging/director_root.json");

const GOV_CONFIG_ROOT: &[u8] = include_bytes!("../../roots/gov/config_root.json");

const GOV_DIRECTOR_ROOT: &[u8] = include_bytes!("../../roots/gov/director_root.json");

/// Datadog site selection used to pick a default TUF trust-root pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    Prod,
    Staging,
    Gov,
}

impl Site {
    /// Map an endpoint authority/host to a Datadog site.
    ///
    /// The configured agentless endpoint authority looks like `config.<site>`
    /// (see `make_agentless_configs_endpoint`), so we strip a leading
    /// `config.` prefix and apply the same rules the agent uses.
    fn from_host(host: &str) -> Self {
        let site = host.strip_prefix("config.").unwrap_or(host);
        if site == "datad0g.com" || site.ends_with(".datad0g.com") {
            Site::Staging
        } else if site == "ddog-gov.com" || site.ends_with(".ddog-gov.com") {
            Site::Gov
        } else {
            Site::Prod
        }
    }

    fn embedded_config_root(self) -> &'static [u8] {
        match self {
            Site::Prod => PROD_CONFIG_ROOT,
            Site::Staging => STAGING_CONFIG_ROOT,
            Site::Gov => GOV_CONFIG_ROOT,
        }
    }

    fn embedded_director_root(self) -> &'static [u8] {
        match self {
            Site::Prod => PROD_DIRECTOR_ROOT,
            Site::Staging => STAGING_DIRECTOR_ROOT,
            Site::Gov => GOV_DIRECTOR_ROOT,
        }
    }
}

/// Read a TUF root override from disk, returning the bytes
fn load_root(override_path: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    let bytes = std::fs::read(override_path)
        .map_err(|e| format_err!("failed to read TUF root override at {override_path:?}: {e}"))?;
    Ok(bytes)
}

/// Fake version sent to RC. We have to do this as the RC backend will not answer if the
/// agent_version field is empty or lower than a certain version.
///
/// This is currently set to the last agent version released
const FAKE_AGENT_VERSION: &str = "7.78.4";

type TUFRepo = tuf::repository::EphemeralRepository<tuf::interchange::Json>;
type TUFClient = tuf::client::Client<tuf::interchange::Json, TUFRepo, TUFRepo>;

// Make a remote config API endpoint from and endpoint where `e.url` is the base dd site
// If the endpoint is not suitable (api key not set, not https), returns N
pub fn make_agentless_configs_endpoint(e: &Endpoint) -> Option<Endpoint> {
    let e = e.clone();
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

#[derive(Clone, Debug, Hash, Eq, PartialEq, Default)]
pub struct AgentlessConfig {
    /// Hostname reported to the RC backend
    /// Must be non empty in agentless mode; an empty value causes
    /// `ConfigFetcherState::new` to downgrade to agent mode.
    pub hostname: String,
    /// Optional path to a TUF repo root JSON to use instead of the
    /// embedded one
    pub config_root_override_path: Option<PathBuf>,
    pub director_root_override_path: Option<PathBuf>,
    /// Override the `agent_uuid` field sent to the RC backend.
    pub agent_uuid: Option<String>,
}

pub type NativeAgentlessFetcher = AgentlessFetcher<libdd_capabilities_impl::NativeHttpClient>;

pub struct AgentlessFetcher<C: HttpClientCapability> {
    http: C,
    opaque_backend_state: Vec<u8>,
    director_client: TUFClient,
    config_client: TUFClient,
    /// Raw signed TUF root bytes used to (re)build the clients. Usually a static
    /// slice for embedded roots, owned only when loaded from an override path.
    config_root_bytes: Cow<'static, [u8]>,
    director_root_bytes: Cow<'static, [u8]>,
    /// Last non-empty config top-targets metadata received from the backend. The
    /// backend only re-sends config top-targets when their version changes; on a
    /// config root rotation rust-tuf purges its trusted top-targets and must
    /// re-fetch them from the remote repo, so we cache and re-serve the last copy
    /// to avoid being stuck (incident-45734).
    last_config_top_targets: Option<remoteconfig::TopMeta>,
    /// Org UUID pinned to a config root version. Root rotation forces a
    /// re-fetch, so a bad pin clears itself on the next rotation.
    org_uuid: Option<OrgUuidBinding>,
    /// Whether the one-shot org-UUID prefetch has run. Cleared by `reset()`.
    org_data_prefetched: bool,
    hostname: String,
    agent_uuid_override: Option<String>,
    products: HashSet<String>,
    refresh_interval: Duration,
    /// Number of consecutive `fetch_config` failures. Reset to 0 on success.
    consecutive_failures: u32,
    endpoint: Endpoint,
}

#[derive(Debug, Clone)]
struct OrgUuidBinding {
    config_root_version: u64,
    uuid: String,
}

#[derive(Debug)]
pub struct ClientTargetRef {
    pub path: String,
    pub version: u64,
    pub primary_hash: String,
    pub length: u64,
}

pub struct ClientResponse {
    pub root_version: u64,
    pub target_version: u64,
    pub opaque_backend_state: Vec<u8>,
    /// All currently active targets; content is already stored in the outer cache.
    pub targets: Vec<ClientTargetRef>,
    pub refresh_interval: Duration,
}

/// A trusted, unexpired TUF target. Produced by [`trusted_targets`].
struct TrustedTarget<'a> {
    path: &'a tuf::metadata::TargetPath,
    length: u64,
    version: u64,
    all_hashes: Vec<(&'static tuf::crypto::HashAlgorithm, tuf::crypto::HashValue)>,
    /// Lowercase hex of the first supported hash; used for cache-hit comparisons.
    primary_hash: String,
}

const CUSTOM_METADATA_EXPIRY_PATH: &str = "expires";

impl<'a> TrustedTarget<'a> {
    fn try_create(path: &'a TargetPath, desc: &'a TargetDescription) -> anyhow::Result<Self> {
        if let Some(expiry) = desc.custom().get(CUSTOM_METADATA_EXPIRY_PATH) {
            let expiry_ts = expiry
                .as_u64()
                .ok_or_else(|| format_err!("expiry not a number"))?;

            // Use saturating arithmetic so a far-future `expires` cannot overflow
            // `u64` (which panics in debug builds and wraps to a fail-open value in
            // release builds). Saturating to `u64::MAX` keeps genuinely far-future
            // targets "not yet expired" while never wrapping below `now`.
            if expiry_ts.saturating_mul(1000) <= now_unix_milli_ts() {
                bail!("expired target at path: {path}")
            }
        }

        let all_hashes = tuf::crypto::retain_supported_hashes(desc.hashes());
        if all_hashes.is_empty() {
            bail!("no supported hash algorithm for target at path: {path}")
        }
        // retain_supported_hashes return order is deterministic.
        let primary_hash = all_hashes[0].1.to_string();

        let version = desc.custom().get("v").and_then(|v| v.as_u64()).unwrap_or(0);

        Ok(Self {
            path,
            length: desc.length(),
            version,
            all_hashes,
            primary_hash,
        })
    }
}

impl<C: HttpClientCapability + Send + Sync> AgentlessFetcher<C> {
    /// Create a new `AgentlessFetcher` client.
    ///
    /// # Errors
    /// Returns an error if TUF root initialization fails.
    pub async fn new(cfg: AgentlessConfig, endpoint: Endpoint) -> anyhow::Result<Self> {
        // Pick the default trust roots based on the endpoint's host and overrides
        let site = endpoint
            .url
            .host()
            .map(Site::from_host)
            .unwrap_or(Site::Prod);

        let config_root_bytes: Cow<'static, [u8]> = match cfg.config_root_override_path.as_deref() {
            Some(p) => Cow::Owned(load_root(p)?),
            None => Cow::Borrowed(site.embedded_config_root()),
        };
        let director_root_bytes: Cow<'static, [u8]> =
            match cfg.director_root_override_path.as_deref() {
                Some(p) => Cow::Owned(load_root(p)?),
                None => Cow::Borrowed(site.embedded_director_root()),
            };

        Ok(Self {
            endpoint,
            http: C::new_client(),
            director_client: TUFClient::with_trusted_root(
                tuf::client::Config::default(),
                &RawSignedMetadata::new(director_root_bytes.to_vec()),
                TUFRepo::new(),
                TUFRepo::new(),
            )
            .await?,
            config_client: TUFClient::with_trusted_root(
                tuf::client::Config::default(),
                &RawSignedMetadata::new(config_root_bytes.to_vec()),
                TUFRepo::new(),
                TUFRepo::new(),
            )
            .await?,
            config_root_bytes,
            director_root_bytes,
            last_config_top_targets: None,
            org_uuid: None,
            org_data_prefetched: false,
            hostname: cfg.hostname,
            agent_uuid_override: cfg.agent_uuid,
            products: HashSet::new(),

            opaque_backend_state: Vec::new(),
            refresh_interval: Duration::from_secs(60),
            consecutive_failures: 0,
        })
    }

    /// Number of consecutive failed `fetch_config` calls. `0` after a success.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Recommended delay before the next `fetch_config` attempt given the
    /// current consecutive-failure count. Returns `None` when no backoff
    /// applies (i.e. either no failures yet, or only a single one).
    pub fn next_backoff(&self) -> Option<Duration> {
        compute_backoff(self.consecutive_failures)
    }

    /// Rebuild both TUF clients from the embedded/override roots and discard all
    /// derived state, restarting the fetcher as if freshly constructed. Called on
    /// `apply()` failure so a partially-advanced trusted database cannot block
    /// subsequent polls
    ///
    /// TODO(rust-tuf): rebuilding from the embedded root discards any newer root
    /// versions we had already verified, so recovery re-reports the embedded root
    /// version and the backend re-sends the rotated roots to be re-verified.
    /// rust-tuf has a private `Database::purge_metadata()` that
    /// clears snapshot/targets/timestamp/delegations while keeping the trusted
    /// root. If that were exposed we could reset non-root state in place and
    /// preserve the advanced root instead of restarting from the embedded one.
    async fn reset(&mut self) -> anyhow::Result<()> {
        self.director_client = TUFClient::with_trusted_root(
            tuf::client::Config::default(),
            &RawSignedMetadata::new(self.director_root_bytes.to_vec()),
            TUFRepo::new(),
            TUFRepo::new(),
        )
        .await?;
        self.config_client = TUFClient::with_trusted_root(
            tuf::client::Config::default(),
            &RawSignedMetadata::new(self.config_root_bytes.to_vec()),
            TUFRepo::new(),
            TUFRepo::new(),
        )
        .await?;
        self.products.clear();
        self.opaque_backend_state.clear();
        self.last_config_top_targets = None;
        self.org_uuid = None;
        self.org_data_prefetched = false;
        Ok(())
    }

    /// Check the config snapshot's `custom.org_uuid` against the UUID served
    /// by `/api/v0.1/org`. No snapshot custom => skip.
    ///
    /// The pinned UUID is keyed by the config trusted-root version, so a root
    /// rotation forces a fresh fetch. `prefetched` reuses the first-poll
    /// concurrent fetch; otherwise the UUID is fetched here.
    async fn verify_org_uuid(&mut self, prefetched: Option<String>) -> anyhow::Result<()> {
        let Some(expected) = self
            .config_client
            .database()
            .trusted_snapshot()
            .ok_or_else(|| format_err!("org UUID check failed: missing trusted snapshot"))?
            .additional_fields()
            .get("custom")
            .and_then(|c| c.get("org_uuid"))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };

        let root_version = self.config_client.database().trusted_root().version();

        let stored: &str = match self.org_uuid.as_ref() {
            Some(b) if b.config_root_version == root_version => &b.uuid,
            _ => {
                let uuid = match prefetched {
                    Some(u) => u,
                    None => self.get_org_data().await?.uuid,
                };
                &self
                    .org_uuid
                    .insert(OrgUuidBinding {
                        config_root_version: root_version,
                        uuid,
                    })
                    .uuid
            }
        };

        anyhow::ensure!(
            stored == expected,
            "org UUID mismatch: intake={stored} snapshot={expected}"
        );
        Ok(())
    }

    async fn fetch_target(&self, target: &TrustedTarget<'_>) -> anyhow::Result<Vec<u8>> {
        let target_path = target.path;

        // Fetch from the remote __unverified__ repo.
        // This is fine because we compare the hash+len against TUF-validated metadata.
        let mut read = self
            .director_client
            .remote_repo()
            .fetch_target(target_path)
            .await?;
        let mut buf = Vec::new();
        read.read_to_end(&mut buf).await?;

        if buf.len() as u64 != target.length {
            bail!("bad length for file at path: {}", target.path)
        }

        let hash_algs = target
            .all_hashes
            .iter()
            .map(|(alg, _val)| (*alg).clone())
            .collect::<Vec<_>>();
        let actual_hashes = tuf::crypto::calculate_hashes_from_slice(&buf, hash_algs.as_slice())?;
        let expected: HashMap<_, _> = target
            .all_hashes
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

        Ok(buf)
    }

    /// Fetch remote config. Newly-downloaded target content is written into
    /// `cache` after having been validated.
    /// The [`ClientResponse`] contains the path, and metadata info of targets
    /// that have been sent to the client.
    ///
    /// This separation between content and metadata is done for 2 reasons:
    /// 1. Remote Config does not re-send target files that we have received in a previous fetch. So
    ///    it should already be in cache.
    /// 2. Currently we only have a single [`remoteconfig::Client`], but targets for mutliple
    ///    clients can be fetched at once, and we need to do a M:N mapping of target files to
    ///    clients, with the same target that can be used by mutliple client
    pub(crate) async fn fetch_config<Storage: FileStorage>(
        &mut self,
        c: remoteconfig::Client,
        cache: &TargetCache<'_, Storage>,
    ) -> anyhow::Result<ClientResponse> {
        // Derive the versions we report to the backend directly from the live
        // trusted databases. A freshly built or just-reset client has no
        // trusted snapshot yet, so it reports snapshot version 0 and the embedded
        // root versions.
        let current_config_snapshot_version = self
            .config_client
            .database()
            .trusted_snapshot()
            .map_or(0, |s| s.version());
        let current_config_root_version = self.config_client.database().trusted_root().version();
        let current_director_root_version =
            self.director_client.database().trusted_root().version();

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

        let (has_error, error) = match c.state.as_ref() {
            Some(state) if state.has_error => (true, state.error.clone()),
            _ => (false, String::new()),
        };

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
            has_error,
            error,
            trace_agent_env: String::new(),
            org_uuid: String::new(),
            tags: vec![],
            agent_uuid: self
                .agent_uuid_override
                .as_deref()
                .unwrap_or_else(|| libdd_common::machine_id::get_machine_id())
                .to_owned(),
        };
        // During first poll only fetch org data in parallel with the config request
        // to hide its latency. Later polls (and prefetch failures) fall back to
        // the sequential fetch in `verify_org_uuid`.
        let (response_result, prefetched_org_uuid) = if !self.org_data_prefetched {
            self.org_data_prefetched = true;
            let (r, org) = futures::join!(self.get_latest_config(request), self.get_org_data());
            let org_uuid = match org {
                Ok(d) => Some(d.uuid),
                Err(e) => {
                    debug!("org data prefetch failed, will fetch lazily: {e:#}");
                    None
                }
            };
            (r, org_uuid)
        } else {
            (self.get_latest_config(request).await, None)
        };
        let response = match response_result {
            Ok(r) => r,
            Err(e) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                return Err(e);
            }
        };

        let active_targets = match self.apply(&response, cache, prefetched_org_uuid).await {
            Ok(t) => t,
            Err(e) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                // On any `apply()` failure the trusted databases may have been advanced
                // in place and incrementally, leaving them inconsistent with the
                // versions we would report next poll.
                // Reset both clients so the next poll restarts from a clean state
                if let Err(reset_err) = self.reset().await {
                    error!("failed to reset TUF clients after apply error: {reset_err}");
                }
                return Err(e);
            }
        };
        self.consecutive_failures = 0;

        self.products = all_products;

        // TODO:
        // In the future we will want to query configs for multiple clients (for PHP, which can have
        // many processes use the same rc client).
        // This means we will need to dispatch the different files based on filter predicates
        // which we currently do not parse.

        Ok(ClientResponse {
            root_version: self.config_client.database().trusted_root().version(),
            target_version: self
                .config_client
                .database()
                .trusted_targets()
                .ok_or(anyhow::anyhow!("Missing target data"))?
                .version(),
            opaque_backend_state: self.opaque_backend_state.clone(),
            targets: active_targets,
            refresh_interval: self.refresh_interval,
        })
    }

    async fn get_org_data(&self) -> anyhow::Result<remoteconfig::OrgDataResponse> {
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
        let path = PathAndQuery::from_static("/api/v0.1/configurations");
        let body = Bytes::from(req.encode_to_vec());
        let res = self.send_request(Method::POST, path, body).await?;
        let res = parse_rc_response(res)?;
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
        let timeout = Duration::from_millis(self.endpoint.timeout_ms);
        let response = tokio::time::timeout(timeout, self.http.request(req))
            .await
            .map_err(|_| {
                format_err!(
                    "Remote config request timed out after {}ms",
                    self.endpoint.timeout_ms,
                )
            })??;
        Ok(response)
    }

    /// Update the TUF-clients state to add the new data fetched from the intake,
    /// verify it with tuf-rust and verify target files against the TUF signed information.
    ///
    /// After this function returns Ok, the TUF client trusted database should be in-sync
    /// with data fetched from the backend
    async fn apply<S: FileStorage>(
        &mut self,
        response: &remoteconfig::LatestConfigsResponse,
        cache: &TargetCache<'_, S>,
        prefetched_org_uuid: Option<String>,
    ) -> anyhow::Result<Vec<ClientTargetRef>> {
        // At a high level,  we're populating the "remote" repos with the metadata
        // that we received from upstream (which does not validate it), and then using the clients'
        // `update` methods to synchronize that metadata to the "local" repos, during which
        // validation is performed.

        let root_path = MetadataPath::root();
        let timestamp_path = MetadataPath::timestamp();
        let snapshot_path = MetadataPath::snapshot();
        let targets_path = MetadataPath::targets();

        let Some(metas) = response.config_metas.as_ref() else {
            bail!("missing config meta from LatestConfigsResponse")
        };
        // The backend diffs config top-targets against the snapshot version we
        // report: it sends them only when their version changed, otherwise the
        // field is empty.
        //
        // rust-tuf purges its trusted top-targets whenever the
        // config root rotates and then re-fetches them from the remote repo; if
        // we wipe the remote repo (below) and the backend sent none, that
        // re-fetch finds nothing and `update()` is stuck (incident-45734). Re-serve
        // the last cached copy when the response omits them.
        let config_top_targets = if metas.top_targets.is_some() {
            &metas.top_targets
        } else {
            &self.last_config_top_targets
        };

        let config_repo_mut = self.config_client.remote_repo_mut();
        *config_repo_mut = TUFRepo::new();

        store(config_repo_mut, &root_path, &metas.roots).await?;
        store_noversion(config_repo_mut, &timestamp_path, &metas.timestamp).await?;
        store(config_repo_mut, &snapshot_path, &metas.snapshot).await?;
        store(config_repo_mut, &targets_path, config_top_targets).await?;
        // Delegated targets are stored later, after verifying the top-level signatures.

        let Some(metas) = response.director_metas.as_ref() else {
            bail!("missing director meta from LatestConfigsResponse")
        };

        let director_remote_repo = self.director_client.remote_repo_mut();
        *director_remote_repo = TUFRepo::new();
        for target_file in &response.target_files {
            let trimmed_path = trim_hash_target_path(&target_file.path)?;
            director_remote_repo
                .store_target(
                    &TargetPath::new(&trimmed_path)?,
                    &mut target_file.raw.as_slice(),
                )
                .await?;
        }

        store(director_remote_repo, &root_path, &metas.roots).await?;
        store_noversion(director_remote_repo, &timestamp_path, &metas.timestamp).await?;
        store(director_remote_repo, &snapshot_path, &metas.snapshot).await?;
        store(director_remote_repo, &targets_path, &metas.targets).await?;

        // Verification of top level metadata for each individual repo happens here
        self.config_client.update().await?;
        self.director_client.update().await?;

        let now = chrono::Utc::now();
        let parent = MetadataPath::targets();

        // Ingest each delegated targets blob into the config DB. This enforces the
        // per-product signing keys: `update_delegated_targets` verifies signatures,
        // expiry and version monotonicity before inserting into `trusted_delegations`.
        if let Some(metas) = response.config_metas.as_ref() {
            for dm in &metas.delegated_targets {
                let role = MetadataPath::new(dm.role.clone())
                    .map_err(|e| format_err!("bad delegated role name {:?}: {e}", dm.role))?;
                let raw = RawSignedMetadata::new(dm.raw.clone());
                self.config_client
                    .database_mut()
                    .update_delegated_targets(&now, &parent, &role, &raw)
                    .map_err(|e: tuf::Error| {
                        format_err!("failed to verify config delegation {}: {e}", dm.role)
                    })?;
            }
        }

        // Enforce that each director-announced target is also authorized by the
        // config repo's per-product delegated keys, not just the single director key.
        verify_director_against_config(&self.config_client, &self.director_client)?;
        self.verify_org_uuid(prefetched_org_uuid).await?;

        let targets: Vec<TrustedTarget<'_>> = trusted_targets(&self.director_client)?
            .filter(|t| {
                let parseable = cache.is_parseable_path(t.path.as_str());
                if !parseable {
                    warn!(
                        "Skipping unparseable/unknown-product remote config path: {}",
                        t.path.as_str()
                    );
                }
                parseable
            })
            .collect();

        let cached_paths: hashbrown::HashSet<&str> = cache.is_cached_batch(
            targets
                .iter()
                .map(|t| (t.path.as_str(), t.primary_hash.as_str(), t.length)),
        );

        let mut new_targets: Vec<NewTarget> = Vec::new();
        for t in &targets {
            if cached_paths.contains(t.path.as_str()) {
                continue;
            }
            let content = self.fetch_target(t).await?;
            new_targets.push(NewTarget {
                path: t.path.as_str().to_owned(),
                version: t.version,
                primary_hash: t.primary_hash.clone(),
                hashes: t
                    .all_hashes
                    .iter()
                    .map(|(alg, hash)| (hash_algorithm_to_str(alg).to_owned(), hash.to_string()))
                    .collect(),
                content,
            });
        }
        cache.store_batch(new_targets)?;

        let active_path_strs: hashbrown::HashSet<&str> =
            targets.iter().map(|t| t.path.as_str()).collect();
        cache.retain_only(&active_path_strs);

        let active_targets: Vec<ClientTargetRef> = targets
            .iter()
            .map(|t| ClientTargetRef {
                path: t.path.as_str().to_owned(),
                version: t.version,
                primary_hash: t.primary_hash.clone(),
                length: t.length,
            })
            .collect();

        // The Remote Config service uses a `custom` field at the top-level of the targets
        // metadata to store this field which we are supposed to echo back to the server.
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

        // Commit the top-targets cache only now that `apply()` has fully
        // succeeded, so a mid-way failure (followed by `reset()`) never leaves a
        // stale cached copy
        if let Some(config_metas) = response.config_metas.as_ref() {
            if config_metas.top_targets.is_some() {
                self.last_config_top_targets = config_metas.top_targets.clone();
            }
        }

        Ok(active_targets)
    }
}

const REFRESH_INTERVAL_BOUNDS: RangeInclusive<Duration> =
    Duration::from_secs(1)..=Duration::from_secs(60);

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
            .map(Duration::from_secs)
            // Mirror the agent: silently drop values outside `[1s, 1m]`
            .filter(|d| REFRESH_INTERVAL_BOUNDS.contains(d)),
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

/// Compute the backoff delay to wait before the next `fetch_config` attempt,
/// given the number of consecutive failures observed so far.
fn compute_backoff(consecutive_failures: u32) -> Option<Duration> {
    match consecutive_failures {
        0 => None,
        1 => Some(jitter_secs(30, 60)),
        2 => Some(jitter_secs(60, 120)),
        3 => Some(jitter_secs(120, 240)),
        _ => Some(Duration::from_secs(240)),
    }
}

/// Random duration in `[min_secs, max_secs]`
fn jitter_secs(min_secs: u64, max_secs: u64) -> Duration {
    use rand::Rng;
    Duration::from_secs(rand::thread_rng().gen_range(min_secs..=max_secs))
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

/// Cross-verify the director's announced targets against the config repo's
/// per-product delegation tree.
///
/// Datadog's RC uses two TUF repos: the *director* has a flat top-level
/// `targets` signed by a single key, while the *config repo* authorizes
/// everything through per-product delegated roles (`APM_TRACING`, `ASM_DD`, …).
/// Trusting only the director collapses the model to one key, so every director
/// target must also be authorized — with the same `(length, hashes)` — by the
/// config delegation tree.
///
/// We can't use the built-in `Database::target_description` walker because it
/// doesn't filter delegations by `paths` before checking `trusted_delegations`,
/// and its matcher handles only directory-prefix patterns, not the globs
/// Datadog uses (`datadog/*/APM_TRACING/*/*`).
///
/// Assumes the config repo is flat (delegated roles are direct children of the
/// top-level targets); nested delegations would require making this recursive.
fn verify_director_against_config(
    config_client: &TUFClient,
    director_client: &TUFClient,
) -> anyhow::Result<()> {
    let top_config_targets = config_client
        .database()
        .trusted_targets()
        .ok_or_else(|| format_err!("config client has no trusted top-level targets"))?;
    let trusted_delegations = config_client.database().trusted_delegations();

    let director_targets = director_client
        .database()
        .trusted_targets()
        .ok_or_else(|| format_err!("director client has no trusted targets"))?;

    for (path, dir_desc) in director_targets.targets() {
        let cfg_desc = lookup_config_target(path, top_config_targets, trusted_delegations)
            .ok_or_else(|| {
                format_err!("director target {path} is not authorized by config delegations")
            })?;

        if cfg_desc.length() != dir_desc.length() {
            bail!(
                "length mismatch between director and config for {path}: director={}, config={}",
                dir_desc.length(),
                cfg_desc.length()
            );
        }

        // Check that the director and config hases sets are equal
        if dir_desc.hashes() != cfg_desc.hashes() {
            bail!("hash set mismatch between director and config for {path}");
        }
    }

    Ok(())
}

/// Resolve `path` against the (flat) config delegation tree, returning the
/// target description from the first matching delegation that lists it.
/// Returns `None` if no delegation authorizes the path.
fn lookup_config_target<'a>(
    path: &TargetPath,
    top: &'a tuf::verify::Verified<tuf::metadata::TargetsMetadata>,
    trusted_delegations: &'a std::collections::HashMap<
        MetadataPath,
        tuf::verify::Verified<tuf::metadata::TargetsMetadata>,
    >,
) -> Option<&'a TargetDescription> {
    // Direct hit on the top-level targets (Datadog's are empty, but be safe).
    if let Some(d) = top.targets().get(path) {
        return Some(d);
    }

    // Spec-style preorder walk over the (ordered) delegation list.
    for delegation in top.delegations().roles() {
        let matches_scope = delegation
            .paths()
            .iter()
            .any(|pat| target_matches_pattern(path.as_str(), pat.as_str()));
        if !matches_scope {
            continue;
        }

        if let Some(meta) = trusted_delegations.get(delegation.name()) {
            if let Some(d) = meta.targets().get(path) {
                return Some(d);
            }
        }

        // Scope matched but path not found: per TUF, a `terminating` delegation
        // stops the search rather than falling through to siblings.
        if delegation.terminating() {
            return None;
        }
    }

    None
}

/// TUF-style glob path matching. `*` matches any run of characters within a
/// single `/`-delimited segment; segments must otherwise match literally.
/// E.g. `datadog/*/APM_TRACING/*/*` matches
/// `datadog/556989/APM_TRACING/<id>/<hash>` but not
/// `datadog/x/y/APM_TRACING/<id>/<hash>`.
fn target_matches_pattern(path: &str, pattern: &str) -> bool {
    let mut p_segs = pattern.split('/');
    let mut t_segs = path.split('/');
    loop {
        match (p_segs.next(), t_segs.next()) {
            (None, None) => return true,
            (Some(p), Some(t)) if segment_matches(p, t) => continue,
            _ => return false,
        }
    }
}

/// Match a single path segment against a single pattern segment. `*` in the
/// pattern matches zero-or-more characters (within the segment, since segments
/// don't contain `/`).
fn segment_matches(pattern: &str, segment: &str) -> bool {
    // Fast paths.
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == segment;
    }

    // General case: literals split by `*`, anchored at both ends.
    let parts: Vec<&str> = pattern.split('*').collect();
    let first = parts[0];
    if !segment.starts_with(first) {
        return false;
    }
    let last = parts[parts.len() - 1];
    let mut cursor = &segment[first.len()..];
    // Middle literals must appear in order, non-overlapping.
    for mid in &parts[1..parts.len() - 1] {
        if mid.is_empty() {
            continue;
        }
        match cursor.find(mid) {
            Some(i) => cursor = &cursor[i + mid.len()..],
            None => return false,
        }
    }
    cursor.ends_with(last) && cursor.len() >= last.len()
}

/// Return all currently trusted, unexpired targets.  Targets that are expired or that lack a
/// supported hash algorithm are skipped with a debug log.
fn trusted_targets(
    director_client: &TUFClient,
) -> anyhow::Result<impl Iterator<Item = TrustedTarget<'_>> + '_> {
    Ok(director_client
        .database()
        .trusted_targets()
        .ok_or_else(|| format_err!("missing targets from TUF director client"))?
        .targets()
        .iter()
        .filter_map(|(path, desc)| {
            TrustedTarget::try_create(path, desc)
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
            MetadataVersion::Number(tm.version),
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

fn hash_algorithm_to_str(alg: &tuf::crypto::HashAlgorithm) -> &str {
    match alg {
        tuf::crypto::HashAlgorithm::Sha256 => "sha256",
        tuf::crypto::HashAlgorithm::Sha512 => "sha512",
        tuf::crypto::HashAlgorithm::Unknown(s) => s.as_str(),
        _ => "unknown",
    }
}

/// Strip the leading `<hash>.` prefix from the basename of a TUF target path.
/// For instance "datadog/2/<product>/<id>/<hash>.config"` => `"datadog/2/<product>/<id>/config"`
///
/// See https://datadoghq.atlassian.net/browse/RC-1859 for more information.
fn trim_hash_target_path(target_path: &str) -> anyhow::Result<String> {
    let (parent, basename) = target_path
        .rsplit_once('/')
        .ok_or_else(|| format_err!("invalid target: {target_path}"))?;
    if basename.is_empty() {
        bail!("invalid target: {target_path}")
    }

    // Strip the leading `<hash>.` component if present. If the basename
    // contains no `.`, keep it as-is (matches the previous behaviour).
    let basename_trimmed = basename.split_once('.').map_or(basename, |(_, rest)| rest);

    Ok(format!("{parent}/{basename_trimmed}"))
}

pub(crate) struct NewTarget {
    pub path: String,
    pub version: u64,
    /// Lowercase hex of the primary hash.
    pub primary_hash: String,
    /// All `(algorithm_name, hex_hash)` pairs for the target.
    pub hashes: Vec<(String, String)>,
    pub content: Vec<u8>,
}

pub(crate) use cache::TargetCache;

/// This module serves as a way to decouple the agentless logic from the rest of this crate
/// This is done for two purposes:
/// * Making review easier by allowing independent review of the RC checking alone
/// * Being able to isolate the agentless logic in it's own crate eventually so that we can reuse it
///   in bottlecap/ obs-pipeline without the rest of the code
mod cache {
    use std::sync::Arc;

    use hashbrown::HashMap;
    use libdd_common::MutexExt as _;
    use libdd_trace_protobuf::remoteconfig::{ConfigState, TargetFileHash, TargetFileMeta};
    use std::sync::Mutex;
    use tracing::warn;

    use crate::{
        fetch::{ClientTargetRef, ConfigFetcherState, FileStorage, NewTarget, StoredTargetFile},
        RemoteConfigPath,
    };

    pub(crate) struct TargetCache<'a, Storage: FileStorage> {
        files: &'a Mutex<HashMap<Arc<RemoteConfigPath>, StoredTargetFile<Storage::StoredFile>>>,
        storage: &'a Storage,
        expire_unused_files: bool,
    }

    impl<'a, S: FileStorage> TargetCache<'a, S> {
        pub(crate) fn new(state: &'a ConfigFetcherState<S::StoredFile>, storage: &'a S) -> Self {
            TargetCache {
                files: &state.target_files_by_path,
                storage,
                expire_unused_files: state.expire_unused_files,
            }
        }

        /// Returns the TUF path strings whose `(primary_hash, len)` already matches the cache.
        pub(crate) fn is_cached_batch<'b>(
            &self,
            candidates: impl IntoIterator<Item = (&'b str, &'b str, u64)>,
        ) -> hashbrown::HashSet<&'b str> {
            let files = self.files.lock_or_panic();
            candidates
                .into_iter()
                .filter_map(|(path, primary_hash, len)| {
                    let parsed = RemoteConfigPath::try_parse(path).ok()?;
                    let stored = files.get(&parsed)?;
                    if stored.hash == primary_hash && stored.meta.length as u64 == len {
                        Some(path)
                    } else {
                        None
                    }
                })
                .collect()
        }

        pub(crate) fn store_batch(
            &self,
            targets: impl IntoIterator<Item = NewTarget>,
        ) -> anyhow::Result<()> {
            let mut files = self.files.lock_or_panic();
            for NewTarget {
                path,
                version,
                primary_hash,
                hashes,
                content,
            } in targets
            {
                let parsed_path = match RemoteConfigPath::try_parse(&path) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!("store_batch: failed to parse remote config path {path}: {e:?}");
                        continue;
                    }
                };
                let parsed_path: Arc<RemoteConfigPath> = Arc::new(parsed_path.into());
                let length = content.len() as i64;
                let new_handle = if let Some(existing) = files.get(&parsed_path) {
                    self.storage
                        .update(&existing.handle, version, content)
                        .map(|()| existing.handle.clone())?
                } else {
                    self.storage.store(version, parsed_path.clone(), content)?
                };
                files.insert(
                    parsed_path.clone(),
                    StoredTargetFile {
                        hash: primary_hash,
                        state: ConfigState {
                            id: parsed_path.config_id.to_string(),
                            version,
                            product: parsed_path.product.to_string(),
                            apply_state: 2, // Acknowledged
                            apply_error: String::new(),
                        },
                        meta: TargetFileMeta {
                            path,
                            length,
                            hashes: hashes
                                .into_iter()
                                .map(|(algorithm, hash)| TargetFileHash { algorithm, hash })
                                .collect(),
                        },
                        handle: new_handle,
                        expiring: false,
                    },
                );
            }
            Ok(())
        }

        /// Evict every entry whose TUF path is not in `active_paths`. No-op when
        /// `expire_unused_files` is `false`.
        pub(crate) fn retain_only(&self, active_paths: &hashbrown::HashSet<&str>) {
            if !self.expire_unused_files {
                return;
            }
            self.files
                .lock_or_panic()
                .retain(|_, stored| active_paths.contains(stored.meta.path.as_str()));
        }

        /// Collect `Arc<S::StoredFile>` handles for every target in `targets`, verifying
        /// stored hash and length match, and marking each entry as non-expiring.
        pub(crate) fn collect_handles(
            &self,
            targets: &[ClientTargetRef],
        ) -> anyhow::Result<Vec<Arc<S::StoredFile>>> {
            let mut files = self.files.lock_or_panic();
            let mut handles = Vec::with_capacity(targets.len());
            for target in targets {
                let parsed = RemoteConfigPath::try_parse(&target.path).map_err(|e| {
                    anyhow::format_err!("collect_handles: bad path {}: {e:?}", target.path)
                })?;
                let stored = files.get_mut(&parsed).ok_or_else(|| {
                    anyhow::format_err!(
                        "collect_handles: path {} not found in cache after fetch",
                        target.path
                    )
                })?;
                if stored.hash != target.primary_hash || stored.meta.length as u64 != target.length
                {
                    anyhow::bail!(
                    "collect_handles: cache mismatch for {}: stored hash={} len={}, expected hash={} len={}",
                    target.path, stored.hash, stored.meta.length,
                    target.primary_hash, target.length
                );
                }
                stored.expiring = false;
                handles.push(stored.handle.clone());
            }
            Ok(handles)
        }

        pub(crate) fn is_parseable_path(&self, path: &str) -> bool {
            RemoteConfigPath::try_parse(path).is_ok()
        }
    }
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
pub fn debug_latest_configs_response(
    resp: &remoteconfig::LatestConfigsResponse,
) -> impl fmt::Debug + '_ {
    DebugLatestConfigsResponse(resp)
}

#[cfg(test)]
mod tests {
    use libdd_common::Endpoint;

    use crate::fetch::AgentlessConfig;
    use crate::fetch::NativeAgentlessFetcher;

    use super::trim_hash_target_path;
    use super::Site;

    #[tokio::test]
    async fn test_create_fetcher_for_site() {
        for site in ["datad0g.com", "datadoghq.com", "ddog-gov.com"] {
            NativeAgentlessFetcher::new(
                AgentlessConfig {
                    hostname: "hostname".to_string(),
                    config_root_override_path: None,
                    director_root_override_path: None,
                    agent_uuid: None,
                },
                Endpoint::agentless(site, "abc".to_string()).unwrap(),
            )
            .await
            .unwrap_or_else(|e| panic!("failed to instantiate fetcher for site {site}: {e}"));
        }
    }

    #[test]
    fn strips_hash_prefix() {
        assert_eq!(
            trim_hash_target_path("datadog/2/APM_TRACING/abcd/deadbeef.config").unwrap(),
            "datadog/2/APM_TRACING/abcd/config"
        );
    }

    #[test]
    fn no_hash_prefix_is_kept() {
        assert_eq!(
            trim_hash_target_path("datadog/2/APM_TRACING/abcd/config").unwrap(),
            "datadog/2/APM_TRACING/abcd/config"
        );
    }

    #[test]
    fn backslash_is_not_a_separator() {
        // Windows-style separators must NOT be treated as path separators.
        // The whole string is the basename here.
        assert!(trim_hash_target_path(r"datadog\2\foo.bar").is_err());
    }

    #[test]
    fn empty_or_no_slash_is_error() {
        assert!(trim_hash_target_path("").is_err());
        assert!(trim_hash_target_path("deadbeef.config").is_err());
    }

    #[test]
    fn trailing_slash_is_error() {
        assert!(trim_hash_target_path("datadog/2/foo/").is_err());
    }

    #[test]
    fn test_compute_backoff() {
        use super::compute_backoff;
        use std::time::Duration;

        assert_eq!(compute_backoff(0), None);
        assert_eq!(compute_backoff(1), None);

        let b2 = compute_backoff(2).unwrap();
        assert!((Duration::from_secs(30)..=Duration::from_secs(60)).contains(&b2));

        let b3 = compute_backoff(3).unwrap();
        assert!((Duration::from_secs(60)..=Duration::from_secs(120)).contains(&b3));

        assert_eq!(compute_backoff(4), Some(Duration::from_secs(120)));
        assert_eq!(compute_backoff(42), Some(Duration::from_secs(120)));
    }

    #[test]
    fn test_target_matches_pattern() {
        use super::target_matches_pattern as m;

        // Real Datadog delegation pattern.
        assert!(m(
            "datadog/556989/APM_TRACING/abc/def",
            "datadog/*/APM_TRACING/*/*"
        ));

        // Wrong product segment.
        assert!(!m(
            "datadog/556989/ASM_DD/abc/def",
            "datadog/*/APM_TRACING/*/*"
        ));

        // Extra path segment must not match (`*` doesn't cross `/`).
        assert!(!m(
            "datadog/x/y/APM_TRACING/abc/def",
            "datadog/*/APM_TRACING/*/*"
        ));

        // Missing path segment.
        assert!(!m(
            "datadog/556989/APM_TRACING/abc",
            "datadog/*/APM_TRACING/*/*"
        ));

        // Employee-prefix delegation.
        assert!(m("employee/ASM_DD/abc/def", "employee/ASM_DD/*/*"));
        assert!(!m("employee/CWS_DD/abc/def", "employee/ASM_DD/*/*"));

        // Partial-segment wildcards.
        assert!(super::segment_matches("foo*bar", "foo123bar"));
        assert!(super::segment_matches("foo*", "foobar"));
        assert!(super::segment_matches("*bar", "foobar"));
        assert!(super::segment_matches(
            "ba*bar k*g of the *elepha*ts*",
            "babar king of the elephants"
        ));
        assert!(!super::segment_matches("foo*bar", "fobar"));
        assert!(!super::segment_matches(" *foobar**", "fobar"));
        // `*` does not cross `/`.
        assert!(!m("foo/bar", "foo*bar"));
    }

    #[test]
    fn test_site_from_host() {
        assert_eq!(Site::from_host("config.datadoghq.com"), Site::Prod);
        assert_eq!(Site::from_host("config.us3.datadoghq.com"), Site::Prod);
        assert_eq!(Site::from_host("config.datadoghq.eu"), Site::Prod);
        assert_eq!(Site::from_host("config.datad0g.com"), Site::Staging);
        assert_eq!(Site::from_host("datad0g.com"), Site::Staging);
        assert_eq!(Site::from_host("config.ddog-gov.com"), Site::Gov);
        assert_eq!(Site::from_host("config.foo.ddog-gov.com"), Site::Gov);
    }
}

#[cfg(test)]
mod integration_tests;
