# Sidecar Telemetry Lifecycle Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Start application telemetry with the initial PHP configuration while preserving per-runtime direct telemetry attribution, sharing AppSec metric definitions at session scope, and cleaning up runtime-owned resources.

**Architecture:** Application actions received before configuration are held in a pending cache and promoted atomically to one active application worker when configuration or a terminal Stop arrives. Direct logs and metrics keep runtime-scoped workers, while their metric definitions live in session/service/environment scopes and are copied into each runtime worker. Runtime and session shutdown remove owned workers and definitions, and application shared-memory setup retries with a one-minute backoff.

**Tech Stack:** Rust 2021, Tokio, `libdd-telemetry`, `datadog-sidecar`, `httpmock`, named shared memory, Cargo workspace tests.

## Global constraints

- Keep direct logs and metrics attributed to their original `InstanceId`.
- Do not change telemetry protocol or backend payload schemas.
- Do not change dd-trace-php helper code in this pull request.
- Scope metric definitions by `(session ID, service, environment)`.
- Cap each definition scope at `libdd_telemetry::worker::MAX_ITEMS`.
- Retry failed application shared-memory creation at most once every 60 seconds.
- Keep `active_telemetry_clients` application-only.
- Do not hold cache, registration, or session mutexes across asynchronous worker operations.
- Every production-code change follows a failing test that demonstrates the missing behavior.

## File structure

- `datadog-sidecar/src/service/telemetry.rs`
  - Own application pending actions, active telemetry clients, runtime metrics/log workers, shared metric definitions, and shared-memory retry state.
  - Add unit tests for pending startup, metric-definition sharing, bounded definitions, cleanup, and shared-memory recovery.
- `datadog-sidecar/src/service/sidecar_server.rs`
  - Route action batches through the pending application lifecycle.
  - Connect runtime and session shutdown to metrics/log cleanup.
  - Preserve the public meaning of sidecar telemetry counts.
  - Add integration-style tests around payloads and server lifecycle.
- `docs/superpowers/specs/2026-07-24-telemetry-lifecycle-isolation-design.md`
  - Source of truth for behavior and constraints. No implementation edits are planned.

---

### Task 1: Hold application actions until initial configuration

**Files:**
- Modify: `datadog-sidecar/src/service/telemetry.rs:354-847`
- Modify: `datadog-sidecar/src/service/sidecar_server.rs:425-675`
- Test: `datadog-sidecar/src/service/sidecar_server.rs:1434-1533`
- Test: `datadog-sidecar/src/service/telemetry.rs:1287-1403`

**Interfaces:**
- Produces: `InitialTelemetryData::from_actions(&[SidecarAction]) -> InitialTelemetryData`
- Produces: `ApplicationTelemetryDispatch::{Pending, Ready { client, actions, created }}`
- Produces: `TelemetryCachedClientSet::get_or_create_for_actions(...) -> ApplicationTelemetryDispatch`
- Preserves: `TelemetryCachedClientSet::get_or_create(...)` for callers that explicitly construct an already-configured application worker.

- [ ] **Step 1: Add a failing Composer-before-config server test**

Add this test beside `initial_config_reaches_app_started_through_enqueue_actions`:

```rust
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn composer_before_config_waits_for_configured_app_started() {
    const SERVICE: &str = "composer-before-config";
    const ENV: &str = "test";
    const TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

    let http_server = MockServer::start_async().await;
    let configured_start = http_server
        .mock_async(|when, then| {
            when.method(POST)
                .path(TELEMETRY_PATH)
                .body_includes("\"request_type\":\"app-started\"")
                .body_includes("\"name\":\"php_config\"");
            then.status(202);
        })
        .await;
    let empty_start = http_server
        .mock_async(|when, then| {
            when.method(POST)
                .path(TELEMETRY_PATH)
                .body_includes("\"request_type\":\"app-started\"")
                .body_excludes("\"name\":\"php_config\"");
            then.status(202);
        })
        .await;

    let handler = test_handler(SidecarServer::default());
    let instance_id = InstanceId::new("session", "runtime");
    let queue_id = QueueId::from(1);
    let session = handler.server.get_session(&instance_id.session_id);
    *session.session_config.lock_or_panic() = Some({
        let mut config = Config::default();
        config
            .set_endpoint_uri(http_server.url("/").parse().unwrap())
            .unwrap();
        config
    });
    handler
        .server
        .get_runtime(&instance_id)
        .lock_applications()
        .entry(queue_id)
        .or_default()
        .set_metadata(
            ENV.to_string(),
            String::new(),
            SERVICE.to_string(),
            Vec::new(),
        );

    handler
        .enqueue_actions(
            instance_id.clone(),
            queue_id,
            vec![SidecarAction::PhpComposerTelemetryFile(
                PathBuf::from("/missing/vendor/composer/installed.json"),
            )],
        )
        .await;
    sleep(TokioDuration::from_millis(50)).await;
    assert_eq!(configured_start.calls_async().await, 0);
    assert_eq!(empty_start.calls_async().await, 0);

    handler
        .enqueue_actions(
            instance_id,
            queue_id,
            vec![SidecarAction::Telemetry(TelemetryActions::AddConfig(
                libdd_telemetry::data::Configuration {
                    name: "php_config".to_string(),
                    value: "present".to_string(),
                    origin: libdd_telemetry::data::ConfigurationOrigin::Default,
                    config_id: None,
                    seq_id: None,
                },
            ))],
        )
        .await;

    timeout(TokioDuration::from_secs(5), async {
        while configured_start.calls_async().await != 1 {
            sleep(TokioDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("configured app-started request");
    assert_eq!(empty_start.calls_async().await, 0);
}
```

- [ ] **Step 2: Run the Composer test and verify the current code fails**

Run:

```bash
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::composer_before_config_waits_for_configured_app_started \
  -- --exact --nocapture
```

Expected: FAIL because the Composer batch creates a worker and the empty
`app-started` mock records one call before the configuration batch.

- [ ] **Step 3: Add a failing initial dependency/integration payload test**

Add a second server test that sends an `AddDependency` and `AddIntegration` batch,
asserts no request is emitted, then sends `AddConfig`. Configure mocks for:

```rust
let complete_start = http_server
    .mock_async(|when, then| {
        when.method(POST)
            .path(TELEMETRY_PATH)
            .body_includes("\"request_type\":\"app-started\"")
            .body_includes("\"name\":\"startup-dependency\"")
            .body_includes("\"name\":\"startup-integration\"")
            .body_includes("\"name\":\"startup-config\"");
        then.status(202);
    })
    .await;
let dependency_change = http_server
    .mock_async(|when, then| {
        when.method(POST)
            .path(TELEMETRY_PATH)
            .body_includes("\"request_type\":\"app-dependencies-loaded\"");
        then.status(202);
    })
    .await;
let integration_change = http_server
    .mock_async(|when, then| {
        when.method(POST)
            .path(TELEMETRY_PATH)
            .body_includes("\"request_type\":\"app-integrations-change\"");
        then.status(202);
    })
    .await;
```

After the configured start arrives, send `LifecycleAction::FlushData`, wait for
`CollectStats`, and assert:

```rust
assert_eq!(complete_start.calls_async().await, 1);
assert_eq!(dependency_change.calls_async().await, 0);
assert_eq!(integration_change.calls_async().await, 0);
```

- [ ] **Step 4: Run the startup-data test and verify it fails**

Run:

```bash
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::pending_startup_data_is_seeded_before_start \
  -- --exact --nocapture
```

Expected: FAIL because the first dependency/integration batch starts the worker
without configuration and later emits change payloads.

- [ ] **Step 5: Add pending application state and initial-data extraction**

In `telemetry.rs`, add:

```rust
#[derive(Default)]
struct InitialTelemetryData {
    configurations: Vec<data::Configuration>,
    dependencies: Vec<data::Dependency>,
    integrations: Vec<data::Integration>,
}

impl InitialTelemetryData {
    fn from_actions(actions: &[SidecarAction]) -> Self {
        let mut initial = Self::default();
        for action in actions {
            match action {
                SidecarAction::Telemetry(TelemetryActions::AddConfig(value)) => {
                    initial.configurations.push(value.clone());
                }
                SidecarAction::Telemetry(TelemetryActions::AddDependency(value)) => {
                    initial.dependencies.push(value.clone());
                }
                SidecarAction::Telemetry(TelemetryActions::AddIntegration(value)) => {
                    initial.integrations.push(value.clone());
                }
                _ => {}
            }
        }
        initial
    }

    fn contains_seeded_action(action: &SidecarAction) -> bool {
        matches!(
            action,
            SidecarAction::Telemetry(
                TelemetryActions::AddConfig(_)
                    | TelemetryActions::AddDependency(_)
                    | TelemetryActions::AddIntegration(_)
            )
        )
    }
}

struct PendingTelemetryActions {
    last_used: Instant,
    actions: Vec<SidecarAction>,
}

pub(crate) enum ApplicationTelemetryDispatch {
    Pending,
    Ready {
        client: Arc<Mutex<Option<TelemetryCachedClient>>>,
        actions: Vec<SidecarAction>,
        created: bool,
    },
}
```

Add `pending: Arc<Mutex<HashMap<(ServiceString, EnvString), PendingTelemetryActions>>>`
to `TelemetryCachedClientSet`. Initialize it in `with_cleanup`, clone its `Arc`
in `Clone`, and retain entries by the same TTL in the cleanup task.

- [ ] **Step 6: Seed all direct startup data before queuing Start**

Change the application constructor to accept `InitialTelemetryData`:

```rust
fn new(
    service: &str,
    env: &str,
    instance_id: &InstanceId,
    runtime_meta: &RuntimeMetadata,
    get_config: impl FnOnce() -> Config,
    initial: InitialTelemetryData,
    process_tags: Vec<Tag>,
) -> anyhow::Result<Self> {
    let mut builder =
        Self::worker_builder(service, env, instance_id, runtime_meta, process_tags);
    builder.config = get_config();
    builder.configurations.extend(initial.configurations);
    builder.dependencies.extend(initial.dependencies);
    builder.integrations.extend(initial.integrations);

    let (handle, _join) = builder.spawn();
    handle.send_start()?;
    let shm_writer =
        match OneWayShmWriter::<NamedShmHandle>::new(path_for_telemetry(service, env)) {
            Ok(writer) => Some(writer),
            Err(error) => {
                warn!("Failed to create telemetry shared-memory writer: {error:?}");
                None
            }
        };

    Ok(Self {
        worker: handle,
        shm_writer,
        shared: TelemetryCachedClientShmData::default(),
        telemetry_metrics: HashMap::new(),
        handle: None,
        stopping: false,
    })
}
```

Keep the shared-memory writer failure non-fatal. Only a failure to enqueue
`Start` returns `Err`.

- [ ] **Step 7: Implement atomic pending-to-active promotion**

Add this method to `TelemetryCachedClientSet`:

```rust
pub(crate) fn get_or_create_for_actions(
    &self,
    service: &str,
    env: &str,
    instance_id: &InstanceId,
    runtime_meta: &RuntimeMetadata,
    actions: Vec<SidecarAction>,
    get_config: impl FnOnce() -> Config,
    process_tags: Vec<Tag>,
) -> ApplicationTelemetryDispatch
```

Implementation rules:

1. Lock `inner`, form the application cache key, and return its active,
   non-stopping client with the current actions and `created: false`.
2. Remove an inactive or stopping entry from `inner`.
3. While `inner` remains locked, lock `pending`, append the current actions to
   the service/environment pending entry, and refresh `last_used`.
4. Promote only if the combined actions contain `AddConfig` or
   `LifecycleAction::Stop`. Otherwise return `Pending`.
5. Remove the combined pending actions, release the pending lock, build
   `InitialTelemetryData`, construct the client, and insert it into `inner`.
6. On constructor error, put the combined actions back into `pending` with a
   refreshed `last_used`, log the failure, and return `Pending` without
   inserting a broken active entry.
7. Return `Ready { client, actions: combined, created: true }`.

The `inner` lock makes the active check, pending append, and active insertion one
state transition. Do not perform an asynchronous operation while it is held.

- [ ] **Step 8: Route server action batches through the new lifecycle**

In `enqueue_actions`, remove `initial_configurations`. Call
`get_or_create_for_actions` with the owned `actions` vector. Return immediately
for `Pending`. For `Ready`, replace the local `actions` and client with the
returned values.

When iterating a newly created lifecycle, set `telemetry.shared.config_sent`
when processing `AddConfig`, insert every `AddIntegration` value into
`telemetry.shared.integrations`, and set `buffered_info_changed` for either
change. Do not append an action to `actions_to_process` when
`InitialTelemetryData::contains_seeded_action(&action)` is true. Composer paths,
metric points, endpoints, logs, and Stop continue through their existing match
arms.

- [ ] **Step 9: Adapt direct constructor tests and preserve Stop behavior**

Update direct `get_or_create` test callers to pass:

```rust
|| Config::default(),
InitialTelemetryData {
    configurations: vec![initial_configuration("test_config")],
    ..Default::default()
},
```

Keep `initial_stop_follows_app_started` passing. A pending Stop promotes the
lifecycle with whatever startup data has arrived, then the existing handle chain
queues Stop after Start.

- [ ] **Step 10: Run the focused application lifecycle tests**

Run each command separately:

```bash
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::composer_before_config_waits_for_configured_app_started \
  -- --exact --nocapture
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::pending_startup_data_is_seeded_before_start \
  -- --exact --nocapture
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::initial_config_reaches_app_started_through_enqueue_actions \
  -- --exact --nocapture
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::initial_stop_follows_app_started \
  -- --exact --nocapture
```

Expected: all selected tests PASS, with no empty `app-started` request in the
Composer case and no follow-up dependency/integration payload in the direct
startup-data case.

- [ ] **Step 11: Commit the application lifecycle repair**

```bash
git add datadog-sidecar/src/service/telemetry.rs \
  datadog-sidecar/src/service/sidecar_server.rs
git commit -m "fix(sidecar): wait for initial telemetry config"
```

---

### Task 2: Share metric definitions within a session

**Files:**
- Modify: `datadog-sidecar/src/service/telemetry.rs:47-196`
- Modify: `datadog-sidecar/src/service/telemetry.rs:645-1001`
- Test: `datadog-sidecar/src/service/telemetry.rs:1405-1740`

**Interfaces:**
- Consumes: runtime workers identified by `TelemetryCachedClientOwner::Runtime(InstanceId)`.
- Produces: `TelemetryMetricRegistrationScope::new(&InstanceId, &str, &str)`.
- Produces: `MetricsLogsClientSet::register_metric(&InstanceId, &str, &str, MetricContext) -> bool`.
- Produces: `MetricsLogsClientSet::registered_metrics(&InstanceId, &str, &str) -> Vec<MetricContext>`.

- [ ] **Step 1: Add a failing sibling-runtime replay test**

Add:

```rust
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn metric_registration_is_shared_by_runtimes_in_one_session() {
    const SERVICE: &str = "shared-appsec-service";
    const ENV: &str = "prod";
    const METRIC: &str = "waf.requests";

    let server = MockServer::start_async().await;
    let clients = MetricsLogsClientSet::default();
    let runtime_meta = RuntimeMetadata::new("php", "8.3", "test");
    let runtime_a = InstanceId::new("session", "runtime-a");
    let runtime_b = InstanceId::new("session", "runtime-b");

    let client_a = clients.get_or_create_metrics_logs(
        SERVICE,
        ENV,
        &runtime_a,
        &runtime_meta,
        || test_config(&server),
        Vec::new(),
    );
    assert!(clients.register_metric(
        &runtime_a,
        SERVICE,
        ENV,
        MetricContext {
            name: METRIC.to_string(),
            tags: Vec::new(),
            metric_type: MetricType::Count,
            common: true,
            namespace: MetricNamespace::AppSec,
        },
    ));

    let client_b = clients.get_or_create_metrics_logs(
        SERVICE,
        ENV,
        &runtime_b,
        &runtime_meta,
        || test_config(&server),
        Vec::new(),
    );

    for client in [&client_a, &client_b] {
        assert!(client
            .lock_or_panic()
            .as_ref()
            .expect("runtime worker")
            .telemetry_metrics
            .contains_key(METRIC));
    }
}
```

- [ ] **Step 2: Run the sibling-runtime test and verify it fails**

Run:

```bash
cargo test -p datadog-sidecar \
  service::telemetry::tests::metric_registration_is_shared_by_runtimes_in_one_session \
  -- --exact --nocapture
```

Expected: FAIL because runtime B does not replay runtime A's registration.

- [ ] **Step 3: Add failing session-isolation and full-scope tests**

Add one test that registers `waf.requests` in `session-a`, creates a worker in
`session-b` with the same service/environment, and asserts the second worker
does not have that metric.

Add another test using a test-only small scope limit:

```rust
let clients = MetricsLogsClientSet::with_registration_limit(2);
assert!(clients.register_metric(&instance, SERVICE, ENV, metric("one")));
assert!(clients.register_metric(&instance, SERVICE, ENV, metric("two")));
assert!(!clients.register_metric(&instance, SERVICE, ENV, metric("three")));
let names = clients.registered_metric_names(&instance, SERVICE, ENV);
assert_eq!(names, HashSet::from(["one".to_string(), "two".to_string()]));
```

- [ ] **Step 4: Run both tests and verify they expose current behavior**

Run each command separately:

```bash
cargo test -p datadog-sidecar \
  service::telemetry::tests::metric_registrations_do_not_cross_sessions \
  -- --exact --nocapture
cargo test -p datadog-sidecar \
  service::telemetry::tests::full_metric_scope_preserves_existing_definitions \
  -- --exact --nocapture
```

Expected: the scope-limit test FAILS because current code evicts an old
registration; the isolation test documents the behavior that must remain.

- [ ] **Step 5: Replace runtime registration keys with scoped definition maps**

Replace the flat registration key with:

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TelemetryMetricRegistrationScope {
    session_id: String,
    service: ServiceString,
    env: EnvString,
}

impl TelemetryMetricRegistrationScope {
    fn new(instance_id: &InstanceId, service: &str, env: &str) -> Self {
        Self {
            session_id: instance_id.session_id.clone(),
            service: service.to_string(),
            env: env.to_string(),
        }
    }
}

type TelemetryMetricRegistrations =
    HashMap<TelemetryMetricRegistrationScope, HashMap<String, MetricContext>>;
```

Add `registration_limit: usize` to `MetricsLogsClientSet`. Production `Default`
sets it to `libdd_telemetry::worker::MAX_ITEMS`; a test-only constructor accepts
a smaller value.

- [ ] **Step 6: Replay session definitions into new runtime workers**

Implement:

```rust
fn registered_metrics(
    &self,
    instance_id: &InstanceId,
    service: &str,
    env: &str,
) -> Vec<MetricContext> {
    let scope = TelemetryMetricRegistrationScope::new(instance_id, service, env);
    self.registrations
        .lock_or_panic()
        .get(&scope)
        .into_iter()
        .flat_map(|metrics| metrics.values().cloned())
        .collect()
}
```

Call this before `get_or_create_with` and register every returned context inside
the new runtime client closure.

- [ ] **Step 7: Broadcast new definitions to active sibling workers**

Change `register_metric` to:

1. Insert or update the metric in its session scope.
2. Reject a new name if that scope has reached `registration_limit`.
3. Collect active runtime clients whose owner has the same session ID and whose
   service/environment match.
4. Release both map locks.
5. Register the context in each collected client.
6. Return `true` on insert/update and `false` on capacity rejection.

Remove the current `client` parameter. Update `deliver_batch` to call:

```rust
clients.register_metric(instance_id, service, env, metric);
```

- [ ] **Step 8: Remove point-path registration touching**

Delete `touch_metric`. Change `AddMetricPoint` delivery from:

```rust
let metric_name = name.clone();
clients.touch_metric(instance_id, service, env, &metric_name);
```

to:

```rust
let metric_name = name.clone();
```

The only client lock on this path is the runtime client's metric-context lookup.

- [ ] **Step 9: Adapt existing registration replay tests**

Update `metrics_logs_cache_replays_registrations_after_eviction` and
`metrics_logs_replay_is_scoped_by_service` to inspect nested scopes. Replace
global registration-count assertions with per-scope name assertions.

- [ ] **Step 10: Run all direct telemetry tests**

Run:

```bash
cargo test -p datadog-sidecar service::telemetry::tests -- --nocapture
```

Expected: all telemetry tests PASS. Runtime workers stay distinct, sibling
runtimes share definitions only within one session/service/environment, and a
full definition scope does not evict existing names.

- [ ] **Step 11: Commit the registration repair**

```bash
git add datadog-sidecar/src/service/telemetry.rs
git commit -m "fix(sidecar): share metric definitions per session"
```

---

### Task 3: Clean up runtime and session telemetry state

**Files:**
- Modify: `datadog-sidecar/src/service/telemetry.rs:706-1001`
- Modify: `datadog-sidecar/src/service/sidecar_server.rs:156-255`
- Modify: `datadog-sidecar/src/service/sidecar_server.rs:899-908`
- Test: `datadog-sidecar/src/service/telemetry.rs:1405-1740`
- Test: `datadog-sidecar/src/service/sidecar_server.rs:1811-1907`

**Interfaces:**
- Produces: `TelemetryCachedClientSet::remove_runtime(&InstanceId)`.
- Produces: `TelemetryCachedClientSet::remove_session(&str)`.
- Produces: `MetricsLogsClientSet::remove_runtime(&InstanceId)`.
- Produces: `MetricsLogsClientSet::remove_session(&str)`.
- Produces: `SidecarServer::stop_runtime(&InstanceId) -> impl Future<Output = ()>`.

- [ ] **Step 1: Add failing cache cleanup tests**

Create three runtime workers:

```rust
let session_a_runtime_a = InstanceId::new("session-a", "runtime-a");
let session_a_runtime_b = InstanceId::new("session-a", "runtime-b");
let session_b_runtime = InstanceId::new("session-b", "runtime");
```

Use the same service/environment, register one metric in each session, and then
assert:

```rust
clients.remove_runtime(&session_a_runtime_a);
assert!(clients.get_existing_metrics_logs(
    &session_a_runtime_a, SERVICE, ENV
).is_none());
assert!(clients.get_existing_metrics_logs(
    &session_a_runtime_b, SERVICE, ENV
).is_some());

clients.remove_session("session-a");
assert!(clients.get_existing_metrics_logs(
    &session_a_runtime_b, SERVICE, ENV
).is_none());
assert!(clients.get_existing_metrics_logs(
    &session_b_runtime, SERVICE, ENV
).is_some());
assert!(clients.registered_metrics(
    &session_a_runtime_b, SERVICE, ENV
).is_empty());
assert!(!clients.registered_metrics(
    &session_b_runtime, SERVICE, ENV
).is_empty());
```

- [ ] **Step 2: Run the cleanup test and verify it fails to compile**

Run:

```bash
cargo test -p datadog-sidecar \
  service::telemetry::tests::runtime_and_session_cleanup_remove_owned_state \
  -- --exact --nocapture
```

Expected: compilation FAIL because `remove_runtime` and `remove_session` do not
exist.

- [ ] **Step 3: Implement cache removal without dropping workers under locks**

Add a general helper to `TelemetryCachedClientSet`:

```rust
fn remove_clients_matching(
    &self,
    predicate: impl Fn(&TelemetryCachedClientOwner) -> bool,
) {
    let removed = {
        let mut clients = self.inner.lock_or_panic();
        let keys = clients
            .keys()
            .filter(|(owner, _, _)| predicate(owner))
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| clients.remove(&key))
            .collect::<Vec<_>>()
    };
    drop(removed);
}
```

Build `remove_runtime` and `remove_session` on this helper. Runtime matching
requires full `InstanceId`; session matching compares `instance_id.session_id`.

In `MetricsLogsClientSet::remove_session`, remove the matching definition
scopes under the registration lock, release it, then remove runtime clients.

- [ ] **Step 4: Add a failing SidecarServer shutdown test**

Set up two runtime workers in one session, call the server runtime shutdown path
for one instance, and assert only that client's cache entry is gone. Then call
`stop_session` and assert the remaining entry and registration scope are gone.

The test must use the same methods called by `ConnectionSidecarHandler`, rather
than invoking cache cleanup directly.

- [ ] **Step 5: Run the server shutdown test and verify it fails**

Run:

```bash
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::shutdown_removes_runtime_owned_telemetry \
  -- --exact --nocapture
```

Expected: FAIL because server shutdown currently removes only `SessionInfo`
runtime records.

- [ ] **Step 6: Centralize runtime shutdown in SidecarServer**

Add:

```rust
async fn stop_runtime(&self, instance_id: &InstanceId) {
    self.metrics_logs_clients.remove_runtime(instance_id);
    let maybe_session = self
        .sessions
        .lock_or_panic()
        .get(&instance_id.session_id)
        .cloned();
    if let Some(session) = maybe_session {
        session.shutdown_runtime(&instance_id.runtime_id).await;
    }
}
```

Change all three paths to use it:

- `ConnectionSidecarHandler::cleanup`
- `SidecarInterface::shutdown_runtime`
- any direct server test helper that performs runtime shutdown

In `stop_session`, call `metrics_logs_clients.remove_session(session_id)` before
returning when the `SessionInfo` entry is absent, and before awaiting session
shutdown when it exists.

- [ ] **Step 7: Run cleanup and shutdown tests**

Run each command separately:

```bash
cargo test -p datadog-sidecar \
  service::telemetry::tests::runtime_and_session_cleanup_remove_owned_state \
  -- --exact --nocapture
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::shutdown_removes_runtime_owned_telemetry \
  -- --exact --nocapture
```

Expected: both tests PASS. Removal is idempotent, and session B remains intact.

- [ ] **Step 8: Commit explicit lifecycle cleanup**

```bash
git add datadog-sidecar/src/service/telemetry.rs \
  datadog-sidecar/src/service/sidecar_server.rs
git commit -m "fix(sidecar): clean up runtime telemetry workers"
```

---

### Task 4: Retry application telemetry shared-memory creation

**Files:**
- Modify: `datadog-sidecar/src/service/telemetry.rs:359-516`
- Test: `datadog-sidecar/src/service/telemetry.rs:1076-1403`

**Interfaces:**
- Produces: `ApplicationShmState::{Ready, RetryAt}`.
- Produces: `TelemetryCachedClient::write_shm_file_at(Instant, factory)`.
- Preserves: `TelemetryCachedClient::write_shm_file()` as the production entry point.

- [ ] **Step 1: Add a failing shared-memory recovery test**

Add a test-only constructor path that accepts a writer factory, then write:

```rust
#[tokio::test]
async fn application_shm_writer_retries_and_publishes_current_state() {
    const SERVICE: &str = "shm-retry";
    const ENV: &str = "test";

    let retry_at = Instant::now();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_factory = attempts.clone();
    let path = path_for_telemetry(SERVICE, ENV);
    let mut client = TelemetryCachedClient::new_with_shm_factory(
        SERVICE,
        ENV,
        &InstanceId::new("session", "runtime"),
        &RuntimeMetadata::new("php", "8.3", "test"),
        Config::default,
        InitialTelemetryData::default(),
        Vec::new(),
        retry_at,
        move |_| {
            attempts_for_factory.fetch_add(1, Ordering::Relaxed);
            Err(std::io::Error::other("injected failure"))
        },
    )
    .unwrap();
    client.shared.config_sent = true;

    client.write_shm_file_at(retry_at + Duration::from_secs(59), |_| {
        panic!("retry happened before the deadline")
    });
    assert_eq!(attempts.load(Ordering::Relaxed), 1);

    client.write_shm_file_at(retry_at + Duration::from_secs(60), |path| {
        OneWayShmWriter::<NamedShmHandle>::new(path.clone())
    });
    assert_eq!(attempts.load(Ordering::Relaxed), 1);

    let mut reader = OneWayShmReader::new(open_named_shm(&path).unwrap(), ());
    let shared: TelemetryCachedClientShmData =
        bincode::deserialize(reader.read().1).unwrap();
    assert!(shared.config_sent);
}
```

The successful retry closure does not increment the injected-failure counter;
the final assertion proves the second creation published current state.

- [ ] **Step 2: Run the SHM recovery test and verify it fails to compile**

Run:

```bash
cargo test -p datadog-sidecar \
  service::telemetry::tests::application_shm_writer_retries_and_publishes_current_state \
  -- --exact --nocapture
```

Expected: compilation FAIL because the injectable constructor, retry state, and
`write_shm_file_at` do not exist.

- [ ] **Step 3: Model application SHM state explicitly**

Replace `shm_writer: Option<OneWayShmWriter<NamedShmHandle>>` with:

```rust
enum ApplicationShmState {
    NotRequired,
    Ready(OneWayShmWriter<NamedShmHandle>),
    RetryAt {
        path: CString,
        deadline: Instant,
    },
}
```

Use `NotRequired` for metrics/log clients. On initial application creation
failure, store:

```rust
ApplicationShmState::RetryAt {
    path,
    deadline: now + Duration::from_secs(60),
}
```

- [ ] **Step 4: Add production and injectable writer paths**

Implement:

```rust
fn write_shm_file(&mut self) {
    self.write_shm_file_at(Instant::now(), |path| {
        OneWayShmWriter::<NamedShmHandle>::new(path.clone())
    });
}

fn write_shm_file_at(
    &mut self,
    now: Instant,
    create: impl FnOnce(
        &CString,
    ) -> std::io::Result<OneWayShmWriter<NamedShmHandle>>,
) {
    let serialized = match bincode::serialize(&self.shared) {
        Ok(value) => value,
        Err(error) => {
            warn!("Failed to serialize telemetry data for shared memory: {error}");
            return;
        }
    };

    if matches!(
        &self.shm_state,
        ApplicationShmState::RetryAt { deadline, .. } if now >= *deadline
    ) {
        let ApplicationShmState::RetryAt { path, .. } =
            std::mem::replace(&mut self.shm_state, ApplicationShmState::NotRequired)
        else {
            unreachable!();
        };
        self.shm_state = match create(&path) {
            Ok(writer) => ApplicationShmState::Ready(writer),
            Err(error) => {
                warn!("Failed to create telemetry shared-memory writer: {error:?}");
                ApplicationShmState::RetryAt {
                    path,
                    deadline: now + Duration::from_secs(60),
                }
            }
        };
    }

    if let ApplicationShmState::Ready(writer) = &self.shm_state {
        writer.write(&serialized);
    }
}
```

Use a small private factory-backed constructor to exercise the initial failure.
The public production constructor passes `Instant::now()` and
`OneWayShmWriter::new`.

- [ ] **Step 5: Adapt Stop and Drop handling**

`mark_stopping` and `Drop` write an empty buffer only when the state is
`ApplicationShmState::Ready`. They replace the state with `NotRequired` before
the writer is dropped. Pending retry state requires no operating-system action.

Update tests that directly access `.shm_writer` to match
`ApplicationShmState::Ready`.

- [ ] **Step 6: Run SHM and replacement tests**

Run each command separately:

```bash
cargo test -p datadog-sidecar \
  service::telemetry::tests::application_shm_writer_retries_and_publishes_current_state \
  -- --exact --nocapture
cargo test -p datadog-sidecar \
  service::telemetry::tests::stopping_client_is_atomically_replaced \
  -- --exact --nocapture
```

Expected: both tests PASS. The injected failure does not retry at 59 seconds,
the 60-second attempt publishes `config_sent`, and replacement still owns the
named segment.

- [ ] **Step 7: Commit SHM recovery**

```bash
git add datadog-sidecar/src/service/telemetry.rs
git commit -m "fix(sidecar): retry telemetry shared memory setup"
```

---

### Task 5: Preserve stats semantics and verify the integrated repair

**Files:**
- Modify: `datadog-sidecar/src/service/sidecar_server.rs:324-403`
- Test: `datadog-sidecar/src/service/sidecar_server.rs:1811-1907`
- Verify: all files changed since `origin/main`

**Interfaces:**
- Consumes: `TelemetryCachedClientSet::clients()` for application workers.
- Consumes: `MetricsLogsClientSet::clients()` for auxiliary worker stats.
- Preserves: `SidecarStats.active_telemetry_clients` as application-worker count.

- [ ] **Step 1: Change the existing worker-source test to the required count**

Rename `compute_stats_includes_every_telemetry_worker_source` to
`compute_stats_preserves_application_client_count` and change:

```rust
assert_eq!(stats.active_telemetry_clients, 3);
```

to:

```rust
assert_eq!(stats.active_telemetry_clients, 1);
```

Keep:

```rust
assert_eq!(stats.telemetry_worker.metric_contexts, 3);
assert_eq!(stats.telemetry_worker_errors, 0);
```

This requires worker resource statistics to include all worker sources while
the client count remains application-only.

- [ ] **Step 2: Run the stats test and verify it fails**

Run:

```bash
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::compute_stats_preserves_application_client_count \
  -- --exact --nocapture
```

Expected: FAIL with `left: 3, right: 1`.

- [ ] **Step 3: Separate application count from worker collection**

In `compute_stats`:

```rust
let application_clients = self.telemetry_clients.clients();
let active_telemetry_clients = application_clients
    .iter()
    .filter(|client| client.lock_or_panic().as_ref().is_some())
    .count() as u32;

let cached_clients = application_clients
    .into_iter()
    .chain(self.metrics_logs_clients.clients())
    .collect::<Vec<_>>();
```

Continue adding stats-concentrator workers to `workers` and collecting their
worker stats. Do not add an auxiliary-client field.

- [ ] **Step 4: Run the focused stats test**

Run:

```bash
cargo test -p datadog-sidecar \
  service::sidecar_server::tests::compute_stats_preserves_application_client_count \
  -- --exact --nocapture
```

Expected: PASS with one active application client and three worker metric
contexts.

- [ ] **Step 5: Format and run the affected crate test suite**

Run:

```bash
cargo fmt --all -- --check
cargo test -p datadog-sidecar
cargo clippy -p datadog-sidecar --all-targets -- -D warnings
```

Expected: all commands exit 0. The sidecar test summary reports zero failed
tests, and clippy reports no warnings.

- [ ] **Step 6: Check the full branch diff**

Run:

```bash
git diff --check
git status --short
git diff --stat origin/main...HEAD
git diff origin/main...HEAD -- \
  datadog-sidecar/src/service/telemetry.rs \
  datadog-sidecar/src/service/sidecar_server.rs \
  datadog-sidecar/src/service/stats_flusher.rs
```

Expected: no whitespace errors, only intended worktree files, and no remaining
runtime-scoped metric-definition registry or point-path `touch_metric`.

- [ ] **Step 7: Commit stats semantics**

```bash
git add datadog-sidecar/src/service/sidecar_server.rs
git commit -m "fix(sidecar): preserve telemetry client stats"
```

- [ ] **Step 8: Run the mandatory pre-push review**

Invoke the `pre-push-review` skill against `origin/main`. Apply every actionable
finding, rerun affected tests, and commit new fixes because this branch is
already published.

- [ ] **Step 9: Run final committed-state verification**

Run:

```bash
git status --short
git diff --check
cargo fmt --all -- --check
cargo test -p datadog-sidecar
cargo clippy -p datadog-sidecar --all-targets -- -D warnings
```

Expected: clean status, no diff errors, and all commands exit 0.

- [ ] **Step 10: Update the existing pull request**

Push `brian.marks/debug-telemetry-start-race`, re-read the repository pull
request template, update the draft PR description to describe the current
application pending state, session-scoped definitions, lifecycle cleanup,
shared-memory retry, and test plan, then retain the `AI Generated` label.

- [ ] **Step 11: Monitor post-push CI**

Invoke `dd:pr-babysit`. Wait for every real correctness check to pass. Ignore
`devflow/mergegate` and any aggregator blocked only by that gate, as required by
the repository instructions.
