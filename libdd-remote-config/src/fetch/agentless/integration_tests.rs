// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Mirrors the datadog-agent uptane
//! `client_test.go` harness (generate signed config + director repos, feed a
//! `LatestConfigsResponse` to the client), but drives libdatadog's
//! `fetch_config`/`apply` path through a mock HTTP capability.
//!
//! These tests exercise root rotation, various input shapes, and
//! target-integrity failure modes.
//!
//! # Scenarios intentionally NOT covered here
//!
//! Some failure modes exercised by the agent's uptane test suite cannot be
//! reproduced meaningfully in this in-memory harness and are therefore left
//! out on purpose. If the underlying assumptions change (e.g. we move to a
//! persistent TUF backend, or expose mutation APIs on the trusted database),
//! these must be revisited:
//!
//! * **On-disk or trusted-database tampering**: the rust-tuf `Database` used by the fetcher is
//!   in-memory and offers no public mutation API, so we cannot flip a byte inside an
//!   already-verified snapshot/targets record and observe how `apply()` reacts. Any regression here
//!   would have to be caught inside rust-tuf itself.
//! * **Persisted-cache tampering across restarts**: the fetcher does not own a persistent cache.
//!   The on-disk representation belongs to the embedding host (`FileStorage` implementation). Tests
//!   here use `NoopStorage`, so anything a hostile filesystem could do between two process
//!   lifetimes (swap files, roll back versions, corrupt bytes) is out of scope and must be covered
//!   by the storage implementation's own tests.
//! * **Transport-level integrity and MITM**: TLS and endpoint authentication are provided by the
//!   `HttpClientCapability` implementation. The mock HTTP capability short-circuits the wire, so
//!   replay, downgrade or certificate-swap attacks are not exercised here.
//! * **Signature forgery or weak-key acceptance**: signing keys are generated fresh in each test,
//!   so we cannot meaningfully test "attacker with the old key" scenarios. Those are the
//!   responsibility of rust-tuf's own test suite.
#![allow(clippy::unwrap_used)]

use super::*;
use crate::fetch::{ConfigFetcherState, ConfigInvariants, FileStorage};
use crate::RemoteConfigPath;
use libdd_capabilities::http::{HttpClientCapability, HttpError};
use libdd_capabilities::maybe_send::MaybeSend;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tuf::crypto::{Ed25519PrivateKey, HashAlgorithm, PrivateKey};
use tuf::database::Database;
use tuf::interchange::Json;
use tuf::metadata::{
    Delegation, MetadataDescription, MetadataPath, RawSignedMetadataSet, SignedMetadata,
    SnapshotMetadata, TargetPath, TargetsMetadataBuilder, TimestampMetadataBuilder,
};
use tuf::repo_builder::RepoBuilder;
use tuf::repository::EphemeralRepository;

// ---- mock HTTP capability ------------------------------------------------

#[derive(Clone, Debug)]
struct MockHttp {
    responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
    requests: Arc<Mutex<Vec<remoteconfig::LatestConfigsRequest>>>,
    /// UUID returned for GET /api/v0.1/org. Some tests want to change it mid-run
    /// (e.g. serve a mismatching UUID) so this is a Mutex, not an atomic snapshot.
    org_uuid: Arc<Mutex<String>>,
    /// Count of GET /api/v0.1/org requests observed, used to assert the
    /// concurrent prefetch (poll 1) and the lazy re-fetch (post root-rotation).
    org_requests: Arc<Mutex<u32>>,
}

impl MockHttp {
    fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            org_uuid: Arc::new(Mutex::new(String::new())),
            org_requests: Arc::new(Mutex::new(0)),
        }
    }

    fn push(&self, resp: &remoteconfig::LatestConfigsResponse) {
        self.responses
            .lock()
            .unwrap()
            .push_back(resp.encode_to_vec());
    }

    fn request_at(&self, i: usize) -> remoteconfig::LatestConfigsRequest {
        self.requests.lock().unwrap()[i].clone()
    }

    fn set_org_uuid(&self, uuid: &str) {
        *self.org_uuid.lock().unwrap() = uuid.to_string();
    }

    fn org_request_count(&self) -> u32 {
        *self.org_requests.lock().unwrap()
    }
}

// Resolving immediately would make every request look timed out, so this sleep never completes:
// the request future always wins.
impl libdd_capabilities::SleepCapability for MockHttp {
    fn new() -> Self {
        panic!("agentless fetcher must use the injected sleep capability")
    }

    fn sleep(
        &self,
        _duration: std::time::Duration,
    ) -> impl std::future::Future<Output = ()> + libdd_capabilities::MaybeSend {
        std::future::pending()
    }
}

impl HttpClientCapability for MockHttp {
    fn new_client() -> Self {
        Self::new()
    }

    fn new_without_connection_pooling() -> Self {
        Self::new()
    }

    #[allow(clippy::manual_async_fn)]
    fn request(
        &self,
        req: http::Request<Bytes>,
    ) -> impl Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend {
        let responses = self.responses.clone();
        let requests = self.requests.clone();
        let org_uuid = self.org_uuid.clone();
        let org_requests = self.org_requests.clone();
        // Capture the path off the request before moving the body.
        let path = req.uri().path().to_owned();
        async move {
            if path == "/api/v0.1/org" {
                *org_requests.lock().unwrap() += 1;
                let resp = remoteconfig::OrgDataResponse {
                    uuid: org_uuid.lock().unwrap().clone(),
                };
                return Ok(http::Response::builder()
                    .status(200)
                    .body(Bytes::from(resp.encode_to_vec()))
                    .unwrap());
            }
            let body = req.into_body();
            if let Ok(parsed) = remoteconfig::LatestConfigsRequest::decode(body) {
                requests.lock().unwrap().push(parsed);
            }
            let bytes = responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("mock http: no queued response");
            Ok(http::Response::builder()
                .status(200)
                .body(Bytes::from(bytes))
                .unwrap())
        }
    }
}

// ---- no-op file storage --------------------------------------------------

#[derive(Default)]
struct NoopStorage;

impl FileStorage for NoopStorage {
    type StoredFile = ();

    fn store(
        &self,
        _version: u64,
        _path: Arc<RemoteConfigPath>,
        _contents: Vec<u8>,
    ) -> anyhow::Result<Arc<Self::StoredFile>> {
        Ok(Arc::new(()))
    }

    fn update(
        &self,
        _file: &Arc<Self::StoredFile>,
        _version: u64,
        _contents: Vec<u8>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn test_state() -> ConfigFetcherState<(), libdd_capabilities_impl::NativeCapabilities> {
    ConfigFetcherState::with_client(
        ConfigInvariants {
            language: "test".to_string(),
            tracer_version: "0.0.0".to_string(),
            endpoint: Endpoint::from_slice("http://localhost/"),
            agentless: None,
        },
        libdd_capabilities_impl::NativeCapabilities::new_client(),
    )
}

// ---- TUF repo generation (mirrors uptane client_test.go) -----------------

fn new_key() -> Ed25519PrivateKey {
    Ed25519PrivateKey::from_pkcs8(&Ed25519PrivateKey::pkcs8().unwrap()).unwrap()
}

/// Build a fresh v1 repo (root/targets/snapshot/timestamp all v1, empty
/// targets, consistent_snapshot=true) signed entirely by `key`.
async fn build_v1(key: &Ed25519PrivateKey) -> RawSignedMetadataSet<Json> {
    let mut repo = EphemeralRepository::<Json>::new();
    RepoBuilder::create(&mut repo)
        .trusted_root_keys(&[key])
        .trusted_targets_keys(&[key])
        .trusted_snapshot_keys(&[key])
        .trusted_timestamp_keys(&[key])
        .commit()
        .await
        .unwrap()
}

/// Rotate only the root (v1 -> v2), keeping the same keys. rust-tuf's
/// `update_root` purges all non-root trusted metadata on this bump, which is
/// what triggers the top-targets re-fetch the stuck test exercises.
async fn rotate_root(
    key: &Ed25519PrivateKey,
    prev: &RawSignedMetadataSet<Json>,
) -> RawSignedMetadataSet<Json> {
    let db = Database::<Json>::from_trusted_metadata(prev).unwrap();
    let mut repo = EphemeralRepository::<Json>::new();
    RepoBuilder::from_database(&mut repo, &db)
        .trusted_root_keys(&[key])
        .trusted_targets_keys(&[key])
        .trusted_snapshot_keys(&[key])
        .trusted_timestamp_keys(&[key])
        .stage_root()
        .unwrap()
        .commit()
        .await
        .unwrap()
}

fn meta_version(raw: &[u8]) -> u64 {
    let v: serde_json::Value = serde_json::from_slice(raw).unwrap();
    v["signed"]["version"].as_u64().unwrap()
}

fn top(raw: &[u8]) -> remoteconfig::TopMeta {
    remoteconfig::TopMeta {
        version: meta_version(raw),
        raw: raw.to_vec(),
    }
}

fn director_metas(set: &RawSignedMetadataSet<Json>) -> remoteconfig::DirectorMetas {
    remoteconfig::DirectorMetas {
        roots: vec![top(set.root().unwrap().as_bytes())],
        timestamp: Some(top(set.timestamp().unwrap().as_bytes())),
        snapshot: Some(top(set.snapshot().unwrap().as_bytes())),
        targets: Some(top(set.targets().unwrap().as_bytes())),
    }
}

/// Build a `LatestConfigsResponse` from raw config metadata plus a director set.
fn response(
    config_roots: &[&[u8]],
    config_timestamp: &[u8],
    config_snapshot: &[u8],
    config_top_targets: Option<&[u8]>,
    delegated: Vec<remoteconfig::DelegatedMeta>,
    director: &RawSignedMetadataSet<Json>,
) -> remoteconfig::LatestConfigsResponse {
    remoteconfig::LatestConfigsResponse {
        config_metas: Some(remoteconfig::ConfigMetas {
            roots: config_roots.iter().map(|r| top(r)).collect(),
            timestamp: Some(top(config_timestamp)),
            snapshot: Some(top(config_snapshot)),
            top_targets: config_top_targets.map(top),
            delegated_targets: delegated,
        }),
        director_metas: Some(director_metas(director)),
        target_files: vec![],
    }
}

/// Construct a fetcher wired to a mock HTTP client and pinned to the given
/// root bytes (bypassing `AgentlessFetcher::new`, whose `C::new_client()`
/// would discard our pre-seeded mock).
async fn fetcher(
    http: MockHttp,
    config_root: Vec<u8>,
    director_root: Vec<u8>,
) -> AgentlessFetcher<MockHttp> {
    AgentlessFetcher {
        endpoint: Endpoint {
            timeout_ms: 30_000,
            ..Endpoint::from_slice("http://localhost/")
        },
        http,
        director_client: TUFClient::with_trusted_root(
            tuf::client::Config::default(),
            &RawSignedMetadata::new(director_root.clone()),
            TUFRepo::new(),
            TUFRepo::new(),
        )
        .await
        .unwrap(),
        config_client: TUFClient::with_trusted_root(
            tuf::client::Config::default(),
            &RawSignedMetadata::new(config_root.clone()),
            TUFRepo::new(),
            TUFRepo::new(),
        )
        .await
        .unwrap(),
        last_config_top_targets: None,
        org_uuid: None,
        org_data_prefetched: false,
        hostname: "test-host".to_string(),
        agent_uuid_override: Some("test-uuid".to_string()),
        products: HashSet::new(),
        opaque_backend_state: Vec::new(),
        refresh_interval: Duration::from_secs(60),
        consecutive_failures: 0,
    }
}

fn dummy_client() -> remoteconfig::Client {
    remoteconfig::Client {
        products: vec!["APM_TRACING".to_string()],
        ..Default::default()
    }
}

fn config_root_version(f: &AgentlessFetcher<MockHttp>) -> u64 {
    f.config_client.database().trusted_root().version()
}

fn config_snapshot_version(f: &AgentlessFetcher<MockHttp>) -> Option<u64> {
    f.config_client
        .database()
        .trusted_snapshot()
        .map(|s| s.version())
}

// ---- tests ---------------------------------------------------------------

/// incident-45734: a config **root rotation** where the backend omits the
/// (unchanged) top-targets must still converge. Before the fix the wipe drops
/// the top-targets and `update()` is stuck; the cache re-serves them.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_root_rotation_without_top_targets_still_converges() {
    let config_key = new_key();
    let director_key = new_key();

    let cfg1 = build_v1(&config_key).await;
    let cfg2 = rotate_root(&config_key, &cfg1).await; // config root v1 -> v2
    let dir1 = build_v1(&director_key).await;

    let http = MockHttp::new();
    // Poll 1: full config metadata.
    http.push(&response(
        &[cfg1.root().unwrap().as_bytes()],
        cfg1.timestamp().unwrap().as_bytes(),
        cfg1.snapshot().unwrap().as_bytes(),
        Some(cfg1.targets().unwrap().as_bytes()),
        vec![],
        &dir1,
    ));
    // Poll 2: config ROOT rotated (v2), top-targets version unchanged so the
    // backend sends NONE; reuse the v1 timestamp/snapshot.
    http.push(&response(
        &[cfg2.root().unwrap().as_bytes()],
        cfg1.timestamp().unwrap().as_bytes(),
        cfg1.snapshot().unwrap().as_bytes(),
        None,
        vec![],
        &dir1,
    ));

    let mut f = fetcher(
        http.clone(),
        cfg1.root().unwrap().as_bytes().to_vec(),
        dir1.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    // Poll 1 succeeds and advances the config DB to root v1 / snapshot v1.
    f.fetch_config(dummy_client(), &cache).await.unwrap();
    assert_eq!(config_root_version(&f), 1);

    // Poll 2 (root rotation, no top-targets) must still converge.
    f.fetch_config(dummy_client(), &cache)
        .await
        .expect("root rotation with omitted top-targets must converge");
    assert_eq!(config_root_version(&f), 2);

    // Step 10: reported versions always match the live trusted DB.
    let req1 = http.request_at(0);
    assert_eq!(req1.current_config_snapshot_version, 0);
    assert_eq!(req1.current_config_root_version, 1);
    assert_eq!(req1.current_director_root_version, 1);
    let req2 = http.request_at(1);
    // After poll 1 the DB advanced: snapshot v1, config root still v1.
    assert_eq!(
        req2.current_config_snapshot_version,
        meta_version(cfg1.snapshot().unwrap().as_bytes())
    );
    assert_eq!(
        req2.current_config_root_version,
        meta_version(cfg1.root().unwrap().as_bytes())
    );
}

/// D-F1: an `apply()` that fails *after* advancing the config trusted DB must
/// leave the fetcher recoverable. The reset rebuilds the clients from the
/// pinned roots, so the next poll reports the clean (embedded) versions and
/// converges — no stuck from a partially-advanced trusted DB.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_apply_error_resets_and_recovers() {
    let config_key = new_key();
    let director_key = new_key();

    let cfg1 = build_v1(&config_key).await;
    let cfg2 = rotate_root(&config_key, &cfg1).await;
    let dir1 = build_v1(&director_key).await;

    let good = |top_targets: Option<&[u8]>, roots: &[&[u8]]| {
        response(
            roots,
            cfg1.timestamp().unwrap().as_bytes(),
            cfg1.snapshot().unwrap().as_bytes(),
            top_targets,
            vec![],
            &dir1,
        )
    };

    let http = MockHttp::new();
    // Poll 1: good, advances to config root v1.
    http.push(&good(
        Some(cfg1.targets().unwrap().as_bytes()),
        &[cfg1.root().unwrap().as_bytes()],
    ));
    // Poll 2: config root rotates to v2 (config update succeeds and advances
    // the trusted root), then a garbage delegated-targets blob makes apply()
    // fail *after* the advance.
    let mut bad = good(
        Some(cfg1.targets().unwrap().as_bytes()),
        &[cfg2.root().unwrap().as_bytes()],
    );
    bad.config_metas.as_mut().unwrap().delegated_targets = vec![remoteconfig::DelegatedMeta {
        version: 1,
        role: "APM_TRACING".to_string(),
        raw: b"not valid tuf metadata".to_vec(),
    }];
    http.push(&bad);
    // Poll 3: good again — must recover.
    http.push(&good(
        Some(cfg1.targets().unwrap().as_bytes()),
        &[cfg1.root().unwrap().as_bytes()],
    ));

    let mut f = fetcher(
        http.clone(),
        cfg1.root().unwrap().as_bytes().to_vec(),
        dir1.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    // Poll 1 ok.
    f.fetch_config(dummy_client(), &cache).await.unwrap();
    assert_eq!(config_root_version(&f), 1);
    assert_eq!(config_snapshot_version(&f), Some(1));
    // A fully successful poll leaves the backoff counter at 0.
    assert_eq!(f.consecutive_failures(), 0);
    assert_eq!(f.next_backoff(), None);

    // Poll 2 fails (after the config root advanced to v2) and resets.
    assert!(f.fetch_config(dummy_client(), &cache).await.is_err());
    // Reset purges snapshot/targets/timestamp/delegations but preserves the
    // trusted root that `update()` already advanced to v2 — recovery does not
    // roll back the trust chain to the embedded root v1.
    assert_eq!(config_root_version(&f), 2);
    assert_eq!(config_snapshot_version(&f), None);
    assert!(f.opaque_backend_state.is_empty());
    assert!(f.products.is_empty());
    assert!(f.last_config_top_targets.is_none());
    // E-1: a successful HTTP fetch that fails `apply()` (verification) must still
    // count as a failed poll, so the client backs off during sustained
    // verification failures instead of hot-looping.
    assert_eq!(f.consecutive_failures(), 1);

    // Poll 3 recovers. The response's `cfg1.root` (v1) is older than the
    // preserved trusted root v2 and is ignored by `update_root`; snapshot,
    // timestamp and targets are re-signed by the same key that root v2 trusts,
    // so they verify and repopulate the purged non-root state.
    f.fetch_config(dummy_client(), &cache).await.unwrap();
    assert_eq!(config_root_version(&f), 2);
    assert_eq!(config_snapshot_version(&f), Some(1));
    // A fully successful poll clears the counter again.
    assert_eq!(f.consecutive_failures(), 0);
    assert_eq!(f.next_backoff(), None);

    // The post-reset poll reported the *preserved* trusted root version (v2)
    // and the cleared snapshot version (0), so the backend does not have to
    // re-send the root rotation chain.
    let req3 = http.request_at(2);
    assert_eq!(req3.current_config_snapshot_version, 0);
    assert_eq!(req3.current_config_root_version, 2);
    assert_eq!(req3.current_director_root_version, 1);
}

/// Build a config repo (v1) that authorizes both `known_path` and
/// `unknown_path` through a single delegated role `role_name` whose glob
/// `paths` cover both products. Returns the signed config metadata set plus the
/// raw delegated-targets blob (to feed as `DelegatedMeta`).
async fn build_config_with_delegation(
    config_key: &Ed25519PrivateKey,
    product_key: &Ed25519PrivateKey,
    role_name: &str,
    entries: &[(&str, &[u8])],
    glob_paths: &[&str],
    target_hashes: &[HashAlgorithm],
) -> (RawSignedMetadataSet<Json>, Vec<u8>) {
    // Delegated targets blob: describes every authorized (path, content).
    let mut builder = TargetsMetadataBuilder::new();
    for (path, content) in entries {
        builder = builder
            .insert_target_from_slice(
                TargetPath::new((*path).to_string()).unwrap(),
                content,
                target_hashes,
            )
            .unwrap();
    }
    let delegated = builder.signed::<Json>(product_key).unwrap();
    let raw_delegated = delegated.to_raw().unwrap().as_bytes().to_vec();

    // Top-level config targets: delegate the glob paths to `role_name`.
    let mut delegation = Delegation::builder(MetadataPath::new(role_name.to_string()).unwrap())
        .key(product_key.public());
    for g in glob_paths {
        delegation = delegation.delegate_path(TargetPath::new((*g).to_string()).unwrap());
    }
    let delegation = delegation.build().unwrap();

    let role_path = MetadataPath::new(role_name.to_string()).unwrap();
    let delegated_desc =
        MetadataDescription::from_slice(&raw_delegated, 1, &[HashAlgorithm::Sha256]).unwrap();

    let mut repo = EphemeralRepository::<Json>::new();
    let set = RepoBuilder::create(&mut repo)
        .trusted_root_keys(&[config_key])
        .trusted_targets_keys(&[config_key])
        .trusted_snapshot_keys(&[config_key])
        .trusted_timestamp_keys(&[config_key])
        .stage_root()
        .unwrap()
        .add_delegation_key(product_key.public().clone())
        .add_delegation_role(delegation)
        .stage_targets()
        .unwrap()
        .stage_snapshot_with_builder(|builder| {
            builder.insert_metadata_description(role_path.clone(), delegated_desc.clone())
        })
        .unwrap()
        .commit()
        .await
        .unwrap();

    (set, raw_delegated)
}

/// Build a director repo (v1) that announces every `(path, content)` entry as a
/// top-level target (sha256), matching the config authorization.
async fn build_director_with_targets(
    director_key: &Ed25519PrivateKey,
    entries: &[(&str, &[u8])],
    target_hashes: &[HashAlgorithm],
) -> RawSignedMetadataSet<Json> {
    let mut repo = EphemeralRepository::<Json>::new();
    let mut builder = RepoBuilder::create(&mut repo)
        .trusted_root_keys(&[director_key])
        .trusted_targets_keys(&[director_key])
        .trusted_snapshot_keys(&[director_key])
        .trusted_timestamp_keys(&[director_key])
        .stage_root_if_necessary()
        .unwrap()
        .target_hash_algorithms(target_hashes);
    for (path, content) in entries {
        builder = builder
            .add_target(
                TargetPath::new((*path).to_string()).unwrap(),
                futures_util::io::Cursor::new(content.to_vec()),
            )
            .await
            .unwrap();
    }
    builder.stage_targets().unwrap().commit().await.unwrap()
}

/// E-F1 / G-F1: a director target for a product the closed `RemoteConfigProduct`
/// enum does not know must not fail the fetch of the other, known targets.
///
/// The cache owns the parsing rules (`TargetCache::is_parseable_path`); `apply()`
/// consults it to drop unparseable/unknown-product targets before they reach
/// `active_targets`, so `collect_handles` never sees a path it can't serve.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_unknown_product_target_is_not_stuck_known_targets() {
    let config_key = new_key();
    let product_key = new_key();
    let director_key = new_key();

    let known_path = "datadog/2/APM_TRACING/cfgid/config";
    let unknown_path = "datadog/2/BRAND_NEW_PRODUCT/cfgid/config";
    let known_content: &[u8] = b"known apm config payload";
    let unknown_content: &[u8] = b"brand new product payload";

    let entries: &[(&str, &[u8])] = &[(known_path, known_content), (unknown_path, unknown_content)];

    // Config authorizes BOTH paths (so `verify_director_against_config` passes);
    // the divergence is purely that libdatadog's product enum can't parse the
    // second one.
    let (cfg, raw_delegated) = build_config_with_delegation(
        &config_key,
        &product_key,
        "APM_TRACING",
        entries,
        &[
            "datadog/*/APM_TRACING/*/*",
            "datadog/*/BRAND_NEW_PRODUCT/*/*",
        ],
        &[HashAlgorithm::Sha256],
    )
    .await;
    let dir = build_director_with_targets(&director_key, entries, &[HashAlgorithm::Sha256]).await;

    let resp = delegated_response(&cfg, raw_delegated, "APM_TRACING", &dir, entries);

    let http = MockHttp::new();
    http.push(&resp);

    let mut f = fetcher(
        http.clone(),
        cfg.root().unwrap().as_bytes().to_vec(),
        dir.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    let res = f
        .fetch_config(dummy_client(), &cache)
        .await
        .expect("a config-authorized unknown-product target must not stuck the fetch");

    let returned: Vec<String> = res.targets.iter().map(|t| t.path.to_string()).collect();
    assert_eq!(
        returned,
        vec![known_path.to_string()],
        "only the known-product target should be active"
    );

    // The active batch is fully servable: `collect_handles` succeeds (no stuck).
    let handles = cache
        .lock()
        .collect_handles(&res.targets)
        .expect("active batch must not stuck collect_handles");
    assert_eq!(handles.len(), 1, "only the known target should be served");
}

/// Assemble a `LatestConfigsResponse` from a config set + its raw delegated
/// blob, a director set, and the `(path, content)` entries served as files.
fn delegated_response(
    cfg: &RawSignedMetadataSet<Json>,
    raw_delegated: Vec<u8>,
    role_name: &str,
    dir: &RawSignedMetadataSet<Json>,
    entries: &[(&str, &[u8])],
) -> remoteconfig::LatestConfigsResponse {
    remoteconfig::LatestConfigsResponse {
        config_metas: Some(remoteconfig::ConfigMetas {
            roots: vec![top(cfg.root().unwrap().as_bytes())],
            timestamp: Some(top(cfg.timestamp().unwrap().as_bytes())),
            snapshot: Some(top(cfg.snapshot().unwrap().as_bytes())),
            top_targets: Some(top(cfg.targets().unwrap().as_bytes())),
            delegated_targets: vec![remoteconfig::DelegatedMeta {
                version: meta_version(&raw_delegated),
                role: role_name.to_string(),
                raw: raw_delegated,
            }],
        }),
        director_metas: Some(director_metas(dir)),
        target_files: entries
            .iter()
            .map(|(path, content)| remoteconfig::File {
                path: (*path).to_string(),
                raw: content.to_vec(),
            })
            .collect(),
    }
}

/// A-F1 (libdd #14): `verify_director_against_config` must require the director
/// and config hash sets to be *exactly equal*. Here the director publishes
/// sha256+sha512 for a target while config pins only sha256. The old
/// overlap-only check (shared algos agree) would accept this — letting the
/// director assert an arbitrary sha512 digest config never authorized — so the
/// whole `apply()` must now fail.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_director_hash_superset_is_rejected() {
    let config_key = new_key();
    let product_key = new_key();
    let director_key = new_key();

    let path = "datadog/2/APM_TRACING/cfgid/config";
    let content: &[u8] = b"apm config payload";
    let entries: &[(&str, &[u8])] = &[(path, content)];

    // Config pins sha256 only.
    let (cfg, raw_delegated) = build_config_with_delegation(
        &config_key,
        &product_key,
        "APM_TRACING",
        entries,
        &["datadog/*/APM_TRACING/*/*"],
        &[HashAlgorithm::Sha256],
    )
    .await;
    // Director publishes a superset: sha256 + sha512 (digests still correct for
    // the content, so only the *set* differs).
    let dir = build_director_with_targets(
        &director_key,
        entries,
        &[HashAlgorithm::Sha256, HashAlgorithm::Sha512],
    )
    .await;

    let resp = delegated_response(&cfg, raw_delegated, "APM_TRACING", &dir, entries);
    let http = MockHttp::new();
    http.push(&resp);

    let mut f = fetcher(
        http.clone(),
        cfg.root().unwrap().as_bytes().to_vec(),
        dir.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    let Err(err) = f.fetch_config(dummy_client(), &cache).await else {
        panic!("director hash superset must be rejected (exact-equality)");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("hash set mismatch"),
        "expected a hash-set mismatch error, got: {msg}"
    );
}

// ---- target-file integrity tests -----------------------------------------
//
// These tests confirm that byte- and metadata-level divergences between the
// wire response, the director metadata, and the config-repo delegations are
// all rejected before any target reaches the cache. They complement the
// hash-set superset test above.

/// Convenience: build a matched (config, director) pair for a single
/// APM_TRACING target and return the pieces the tests below tweak.
async fn matched_pair(
    path: &'static str,
    content: &'static [u8],
) -> (
    RawSignedMetadataSet<Json>,
    Vec<u8>,
    RawSignedMetadataSet<Json>,
) {
    let config_key = new_key();
    let product_key = new_key();
    let director_key = new_key();

    let entries: &[(&str, &[u8])] = &[(path, content)];
    let (cfg, raw_delegated) = build_config_with_delegation(
        &config_key,
        &product_key,
        "APM_TRACING",
        entries,
        &["datadog/*/APM_TRACING/*/*"],
        &[HashAlgorithm::Sha256],
    )
    .await;
    let dir = build_director_with_targets(&director_key, entries, &[HashAlgorithm::Sha256]).await;

    (cfg, raw_delegated, dir)
}

/// Feed a `LatestConfigsResponse` in which the raw bytes served for a target
/// no longer hash to what the director signed. Same length — so the length
/// check passes and the hash check is the one that fires.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_tampered_target_bytes_are_rejected() {
    let path = "datadog/2/APM_TRACING/cfgid/config";
    let content: &[u8] = b"apm config payload";
    let (cfg, raw_delegated, dir) = matched_pair(path, content).await;

    let entries: &[(&str, &[u8])] = &[(path, content)];
    let mut resp = delegated_response(&cfg, raw_delegated, "APM_TRACING", &dir, entries);
    // Same length, different bytes: the director metadata signed the SHA-256
    // of `content`, so any equal-length flip fails the hash check.
    // (`content` must be non-empty, otherwise the XOR flip below is a no-op.)
    let tampered: Vec<u8> = content.iter().map(|b| b ^ 0x01).collect();
    assert_eq!(tampered.len(), content.len());
    resp.target_files[0].raw = tampered;

    let http = MockHttp::new();
    http.push(&resp);

    let mut f = fetcher(
        http.clone(),
        cfg.root().unwrap().as_bytes().to_vec(),
        dir.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    let Err(err) = f.fetch_config(dummy_client(), &cache).await else {
        panic!("tampered target bytes must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("hash did not match"),
        "expected a hash-mismatch error on tampered bytes, got: {msg}"
    );

    // Nothing must have been stored.
    assert!(state.target_files_by_path.lock().unwrap().is_empty());
}

/// Feed a response whose raw bytes for a target have the wrong length.
/// Because the length check runs before the hash check in `fetch_target`,
/// this is the specific error we expect — not a hash mismatch.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_target_length_mismatch_is_rejected() {
    let path = "datadog/2/APM_TRACING/cfgid/config";
    let content: &[u8] = b"apm config payload";
    let (cfg, raw_delegated, dir) = matched_pair(path, content).await;

    let entries: &[(&str, &[u8])] = &[(path, content)];
    let mut resp = delegated_response(&cfg, raw_delegated, "APM_TRACING", &dir, entries);
    // Extend the bytes so the length no longer matches the director metadata.
    let mut too_long = content.to_vec();
    too_long.extend_from_slice(b" extra trailing bytes");
    resp.target_files[0].raw = too_long;

    let http = MockHttp::new();
    http.push(&resp);

    let mut f = fetcher(
        http.clone(),
        cfg.root().unwrap().as_bytes().to_vec(),
        dir.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    let Err(err) = f.fetch_config(dummy_client(), &cache).await else {
        panic!("target with mismatching length must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("bad length for file"),
        "expected a length-mismatch error, got: {msg}"
    );
    assert!(state.target_files_by_path.lock().unwrap().is_empty());
}

/// The director publishes a target whose path is not covered by any config
/// delegation. `verify_director_against_config` must reject the whole apply
/// before we ever hit the byte-level checks.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_director_target_not_in_config_delegations_is_rejected() {
    let config_key = new_key();
    let product_key = new_key();
    let director_key = new_key();

    // Config only delegates APM_TRACING...
    let authorized_path = "datadog/2/APM_TRACING/cfgid/config";
    let authorized_content: &[u8] = b"apm payload";
    let (cfg, raw_delegated) = build_config_with_delegation(
        &config_key,
        &product_key,
        "APM_TRACING",
        &[(authorized_path, authorized_content)],
        &["datadog/*/APM_TRACING/*/*"],
        &[HashAlgorithm::Sha256],
    )
    .await;

    // ...but the director signs BOTH the authorized APM_TRACING target and an
    // ASM target that no delegation ever authorized.
    let unauthorized_path = "datadog/2/ASM/cfgid/config";
    let unauthorized_content: &[u8] = b"asm payload";
    let dir_entries: &[(&str, &[u8])] = &[
        (authorized_path, authorized_content),
        (unauthorized_path, unauthorized_content),
    ];
    let dir =
        build_director_with_targets(&director_key, dir_entries, &[HashAlgorithm::Sha256]).await;

    let resp = delegated_response(&cfg, raw_delegated, "APM_TRACING", &dir, dir_entries);
    let http = MockHttp::new();
    http.push(&resp);

    let mut f = fetcher(
        http.clone(),
        cfg.root().unwrap().as_bytes().to_vec(),
        dir.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    let Err(err) = f.fetch_config(dummy_client(), &cache).await else {
        panic!("unauthorized director target must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not authorized by config delegations"),
        "expected an unauthorized-target error, got: {msg}"
    );
    // The authorized sibling must NOT have been cached: apply() is all-or-nothing.
    assert!(state.target_files_by_path.lock().unwrap().is_empty());
}

/// Director and config both authorize the same path, but their signed
/// metadata disagrees on the target's length (different content per builder,
/// with different sizes). `verify_director_against_config` compares length
/// first, so this is the error we expect — not a hash mismatch.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_director_config_length_disagreement_is_rejected() {
    let config_key = new_key();
    let product_key = new_key();
    let director_key = new_key();

    let path = "datadog/2/APM_TRACING/cfgid/config";
    let config_content: &[u8] = b"short";
    let director_content: &[u8] = b"a distinctly longer payload";
    assert_ne!(config_content.len(), director_content.len());

    let (cfg, raw_delegated) = build_config_with_delegation(
        &config_key,
        &product_key,
        "APM_TRACING",
        &[(path, config_content)],
        &["datadog/*/APM_TRACING/*/*"],
        &[HashAlgorithm::Sha256],
    )
    .await;
    let dir = build_director_with_targets(
        &director_key,
        &[(path, director_content)],
        &[HashAlgorithm::Sha256],
    )
    .await;

    // The wire bytes match the director (that is what the fetcher would try
    // to verify against downstream), but apply() must fail earlier during
    // director/config cross-check.
    let resp = delegated_response(
        &cfg,
        raw_delegated,
        "APM_TRACING",
        &dir,
        &[(path, director_content)],
    );
    let http = MockHttp::new();
    http.push(&resp);

    let mut f = fetcher(
        http.clone(),
        cfg.root().unwrap().as_bytes().to_vec(),
        dir.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    let Err(err) = f.fetch_config(dummy_client(), &cache).await else {
        panic!("director/config length disagreement must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("length mismatch between director and config"),
        "expected a director/config length-mismatch error, got: {msg}"
    );
    assert!(state.target_files_by_path.lock().unwrap().is_empty());
}

/// Director and config agree on hash algorithms and content length but the
/// hash *values* differ (they signed different bytes of the same length).
/// The existing `test_director_hash_superset_is_rejected` covers the case
/// where the algorithm SET differs; this one exercises the equal-cardinality
/// value-disagreement path.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_director_config_hash_value_disagreement_is_rejected() {
    let config_key = new_key();
    let product_key = new_key();
    let director_key = new_key();

    let path = "datadog/2/APM_TRACING/cfgid/config";
    // Same length, different bytes: same-size hash sets but different values.
    let config_content: &[u8] = b"config-side-payload"; // 19 bytes
    let director_content: &[u8] = b"director-sd-payload"; // 19 bytes
    assert_eq!(config_content.len(), director_content.len());
    assert_ne!(config_content, director_content);

    let (cfg, raw_delegated) = build_config_with_delegation(
        &config_key,
        &product_key,
        "APM_TRACING",
        &[(path, config_content)],
        &["datadog/*/APM_TRACING/*/*"],
        &[HashAlgorithm::Sha256],
    )
    .await;
    let dir = build_director_with_targets(
        &director_key,
        &[(path, director_content)],
        &[HashAlgorithm::Sha256],
    )
    .await;

    let resp = delegated_response(
        &cfg,
        raw_delegated,
        "APM_TRACING",
        &dir,
        &[(path, director_content)],
    );
    let http = MockHttp::new();
    http.push(&resp);

    let mut f = fetcher(
        http.clone(),
        cfg.root().unwrap().as_bytes().to_vec(),
        dir.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    let Err(err) = f.fetch_config(dummy_client(), &cache).await else {
        panic!("director/config hash-value disagreement must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("hash set mismatch"),
        "expected a director/config hash-value mismatch error, got: {msg}"
    );
    assert!(state.target_files_by_path.lock().unwrap().is_empty());
}

// ---- org UUID tests -----------------------------------------------------

/// Build a config snapshot + timestamp pair whose signed snapshot carries
/// `custom.org_uuid = uuid`. Reuses the (version, expires, meta) already
/// signed inside `cfg.snapshot()`, adds the custom field, and re-signs with
/// `config_key`. The timestamp is rebuilt from the new snapshot so its
/// (length, hash) description stays consistent.
fn config_snapshot_with_org_uuid(
    cfg: &RawSignedMetadataSet<Json>,
    config_key: &Ed25519PrivateKey,
    uuid: &str,
) -> (Vec<u8>, Vec<u8>) {
    // Grab the `signed` object out of the raw snapshot bytes, inject
    // `custom.org_uuid`, then re-hydrate a `SnapshotMetadata` via serde (the
    // rust-tuf shim flattens unknown top-level fields into `additional_fields`,
    // so our injected `custom` object lands in the right place).
    let mut signed: serde_json::Value =
        serde_json::from_slice(cfg.snapshot().unwrap().as_bytes()).unwrap();
    signed
        .get_mut("signed")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "custom".to_string(),
            serde_json::json!({ "org_uuid": uuid }),
        );
    let new_snap: SnapshotMetadata = serde_json::from_value(signed["signed"].clone()).unwrap();
    let signed_snap = SignedMetadata::<Json, SnapshotMetadata>::new(&new_snap, config_key).unwrap();
    let raw_snap = signed_snap.to_raw().unwrap().as_bytes().to_vec();

    let signed_ts =
        TimestampMetadataBuilder::from_snapshot::<Json>(&signed_snap, &[HashAlgorithm::Sha256])
            .unwrap()
            .signed::<Json>(config_key)
            .unwrap();
    let raw_ts = signed_ts.to_raw().unwrap().as_bytes().to_vec();

    (raw_snap, raw_ts)
}

/// First-poll happy path: the concurrent org-data prefetch returns the same
/// UUID that the snapshot pins, so the fetch converges and exactly one org
/// request was issued.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_org_uuid_match_via_concurrent_prefetch() {
    let config_key = new_key();
    let director_key = new_key();

    let cfg1 = build_v1(&config_key).await;
    let dir1 = build_v1(&director_key).await;
    let (snap_bytes, ts_bytes) = config_snapshot_with_org_uuid(&cfg1, &config_key, "ORG-1");

    let http = MockHttp::new();
    http.set_org_uuid("ORG-1");
    http.push(&response(
        &[cfg1.root().unwrap().as_bytes()],
        &ts_bytes,
        &snap_bytes,
        Some(cfg1.targets().unwrap().as_bytes()),
        vec![],
        &dir1,
    ));

    let mut f = fetcher(
        http.clone(),
        cfg1.root().unwrap().as_bytes().to_vec(),
        dir1.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    f.fetch_config(dummy_client(), &cache)
        .await
        .expect("matching org UUID must not fail the poll");

    // Exactly one org request on the first poll (concurrent prefetch).
    assert_eq!(http.org_request_count(), 1);
}

/// Mismatching org UUID must fail the poll AND reset the fetcher, so a
/// subsequent poll reports the clean (embedded) versions.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_org_uuid_mismatch_fails_and_resets() {
    let config_key = new_key();
    let director_key = new_key();

    let cfg1 = build_v1(&config_key).await;
    let dir1 = build_v1(&director_key).await;
    let (snap_bytes, ts_bytes) = config_snapshot_with_org_uuid(&cfg1, &config_key, "ORG-EXPECTED");

    let http = MockHttp::new();
    http.set_org_uuid("ORG-OTHER");
    http.push(&response(
        &[cfg1.root().unwrap().as_bytes()],
        &ts_bytes,
        &snap_bytes,
        Some(cfg1.targets().unwrap().as_bytes()),
        vec![],
        &dir1,
    ));

    let mut f = fetcher(
        http.clone(),
        cfg1.root().unwrap().as_bytes().to_vec(),
        dir1.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    let Err(err) = f.fetch_config(dummy_client(), &cache).await else {
        panic!("mismatching org UUID must fail the poll");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("org UUID"),
        "expected an org-UUID mismatch error, got: {msg}"
    );

    // Reset restored the pinned-root state.
    assert_eq!(config_root_version(&f), 1);
    assert_eq!(config_snapshot_version(&f), None);
    // The one-shot prefetch flag was rearmed, so a fresh poll would prefetch again.
    assert!(!f.org_data_prefetched);
    assert!(f.org_uuid.is_none());
}

/// A config-root rotation invalidates the pinned UUID (it is keyed by the
/// config trusted-root version), forcing a fresh /api/v0.1/org fetch on the
/// next poll.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_org_uuid_refetched_on_root_rotation() {
    let config_key = new_key();
    let director_key = new_key();

    let cfg1 = build_v1(&config_key).await;
    let cfg2 = rotate_root(&config_key, &cfg1).await; // config root v1 -> v2
    let dir1 = build_v1(&director_key).await;
    let (snap_bytes, ts_bytes) = config_snapshot_with_org_uuid(&cfg1, &config_key, "ORG-1");

    let http = MockHttp::new();
    http.set_org_uuid("ORG-1");
    // Poll 1: config root v1, snapshot pins ORG-1.
    http.push(&response(
        &[cfg1.root().unwrap().as_bytes()],
        &ts_bytes,
        &snap_bytes,
        Some(cfg1.targets().unwrap().as_bytes()),
        vec![],
        &dir1,
    ));
    // Poll 2: config root rotates to v2; same signed snapshot/timestamp
    // (rust-tuf purges non-root metadata on rotation, so the backend has to
    // resend them). The pinned UUID binding keyed at root v1 no longer
    // matches root v2 and forces a fresh /api/v0.1/org fetch.
    http.push(&response(
        &[cfg2.root().unwrap().as_bytes()],
        &ts_bytes,
        &snap_bytes,
        Some(cfg1.targets().unwrap().as_bytes()),
        vec![],
        &dir1,
    ));

    let mut f = fetcher(
        http.clone(),
        cfg1.root().unwrap().as_bytes().to_vec(),
        dir1.root().unwrap().as_bytes().to_vec(),
    )
    .await;
    let state = test_state();
    let storage = NoopStorage;
    let cache = TargetCache::new(&state, &storage);

    f.fetch_config(dummy_client(), &cache).await.unwrap();
    assert_eq!(config_root_version(&f), 1);
    assert_eq!(http.org_request_count(), 1); // concurrent prefetch

    f.fetch_config(dummy_client(), &cache).await.unwrap();
    assert_eq!(config_root_version(&f), 2);
    // A second org fetch happened lazily because the config root rotated.
    assert_eq!(http.org_request_count(), 2);
}
