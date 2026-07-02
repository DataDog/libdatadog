// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for libdd #17: reset TUF clients on error and don't wipe
//! config top-targets on a root rotation. Mirrors the datadog-agent uptane
//! `client_test.go` harness (generate signed config + director repos, feed a
//! `LatestConfigsResponse` to the client), but drives libdatadog's
//! `fetch_config`/`apply` path through a mock HTTP capability.
#![allow(clippy::unwrap_used)]

use super::*;
use crate::fetch::{ConfigFetcherState, ConfigInvariants, FileStorage};
use crate::RemoteConfigPath;
use libdd_capabilities::http::{HttpClientCapability, HttpError};
use libdd_capabilities::maybe_send::MaybeSend;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tuf::crypto::Ed25519PrivateKey;
use tuf::database::Database;
use tuf::interchange::Json;
use tuf::metadata::RawSignedMetadataSet;
use tuf::repo_builder::RepoBuilder;
use tuf::repository::EphemeralRepository;

// ---- mock HTTP capability ------------------------------------------------

#[derive(Clone, Debug)]
struct MockHttp {
    responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
    requests: Arc<Mutex<Vec<remoteconfig::LatestConfigsRequest>>>,
}

impl MockHttp {
    fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
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
}

impl HttpClientCapability for MockHttp {
    fn new_client() -> Self {
        Self::new()
    }

    #[allow(clippy::manual_async_fn)]
    fn request(
        &self,
        req: http::Request<Bytes>,
    ) -> impl Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend {
        let responses = self.responses.clone();
        let requests = self.requests.clone();
        async move {
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

fn test_state() -> ConfigFetcherState<()> {
    ConfigFetcherState::new(ConfigInvariants {
        language: "test".to_string(),
        tracer_version: "0.0.0".to_string(),
        endpoint: Endpoint::from_slice("http://localhost/"),
        agentless: None,
    })
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
/// what triggers the top-targets re-fetch the wedge test exercises.
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
        config_root_bytes: Cow::Owned(config_root),
        director_root_bytes: Cow::Owned(director_root),
        last_config_top_targets: None,
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

fn config_root_version(f: &AgentlessFetcher<MockHttp>) -> u32 {
    f.config_client.database().trusted_root().version()
}

fn config_snapshot_version(f: &AgentlessFetcher<MockHttp>) -> Option<u32> {
    f.config_client
        .database()
        .trusted_snapshot()
        .map(|s| s.version())
}

// ---- tests ---------------------------------------------------------------

/// incident-45734: a config **root rotation** where the backend omits the
/// (unchanged) top-targets must still converge. Before the fix the wipe drops
/// the top-targets and `update()` wedges; the cache re-serves them.
#[tokio::test]
async fn root_rotation_without_top_targets_still_converges() {
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
/// converges — no wedge from a partially-advanced trusted DB.
#[tokio::test]
async fn apply_error_resets_and_recovers() {
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

    // Poll 2 fails (after the config root advanced to v2) and resets.
    assert!(f.fetch_config(dummy_client(), &cache).await.is_err());
    // Reset rebuilt the config client from the pinned root: back to v1, no snapshot.
    assert_eq!(config_root_version(&f), 1);
    assert_eq!(config_snapshot_version(&f), None);
    assert!(f.opaque_backend_state.is_empty());
    assert!(f.products.is_empty());
    assert!(f.last_config_top_targets.is_none());

    // Poll 3 recovers.
    f.fetch_config(dummy_client(), &cache).await.unwrap();
    assert_eq!(config_root_version(&f), 1);
    assert_eq!(config_snapshot_version(&f), Some(1));

    // The post-reset poll reported the clean embedded versions, matching the
    // live trusted DB (snapshot 0, root v1) — not the advanced v2.
    let req3 = http.request_at(2);
    assert_eq!(req3.current_config_snapshot_version, 0);
    assert_eq!(req3.current_config_root_version, 1);
    assert_eq!(req3.current_director_root_version, 1);
}
