# libdd-remote-config agentless client vs `pkg/config/remote` (Core Agent) — comparison

## Context

The goal is to use `libdd-remote-config`'s `AgentlessFetcher`
(`libdd-remote-config/src/agentless_client/mod.rs`) as a drop-in replacement
for the trace-agent's RC client, so a tracer/SDK can talk directly to the RC
backend (`config.<site>/api/v0.1/configurations`) instead of going through the
local Datadog agent.

This document is the result of comparing the agentless implementation in
libdatadog with the Core Agent reference implementation in datadog-agent
(`pkg/config/remote/{service,api,uptane}`), and lists missing features,
discrepancies, missing configuration knobs, and potential bugs.

## Files compared

libdatadog (agentless / Rust):
- `libdd-remote-config/src/agentless_client/mod.rs` — TUF client + HTTP calls to `/api/v0.1/{configurations,status,org}`
- `libdd-remote-config/src/fetch/fetcher.rs` — `ConfigFetcher` that switches between agent (`/v0.7/config`) and agentless (`AgentlessFetcher`) modes
- `libdd-remote-config/roots/prod/{config,director}_root.json` — embedded TUF trust roots (prod site only)

datadog-agent (reference / Go):
- `pkg/config/remote/service/service.go` — `CoreAgentService`, polling loop, backoff, org status, subscriptions
- `pkg/config/remote/service/util.go` — `buildLatestConfigsRequest`, RC key parsing, `targetsCustom`
- `pkg/config/remote/api/http.go` — HTTPClient with TLS guards, header rotation, retries
- `pkg/config/remote/uptane/client.go` — `CoreAgentClient`: Uptane verification, BoltDB cache, org-id check
- `pkg/config/remote/meta/{prod.,staging.,gov.}{config,director}.json` — TUF roots per site

---

## High-level summary

The libdd agentless client implements the **minimum** to talk to the RC backend
directly: it embeds prod TUF roots, posts `LatestConfigsRequest`, runs Uptane
verification in-memory, and returns parsed targets. The agent implementation is
significantly more featureful: persistent cache, backoff, retries, multi-site
roots, org-id / org-uuid verification, RC-key & PAR-JWT auth, dynamic
credentials rotation, telemetry, subscriptions, and cache-bypass rate limiting.

The two are roughly equivalent for the **happy path** (fetch + verify a TUF
update), but the agentless client has several gaps that will bite production
use:

- **Site/trust-root coverage is prod-only** (no staging, no gov, no override).
- **No persistent cache** — every process restart re-downloads full metadata.
- **No org UUID / org ID verification.**
- **No backoff / retry policy.**
- **`agent_version` is hardcoded to `"7.78.4"`.**
- **`dbg!` calls are left in code paths reachable in release builds.**
- **Single-client model only** (TODO in code).
- Several `client_tracer` / `LatestConfigsRequest` fields are silently empty.

---

## Missing features

### 1. Multi-site TUF trust roots
Agent ships `prod.{config,director}.json`, `staging.{config,director}.json`,
`gov.{config,director}.json` (see `pkg/config/remote/meta/`) and exposes
`WithConfigRootOverride(site, override)` /
`WithDirectorRootOverride(site, override)` so customers on EU/US3/US5/gov/staging
get the correct trust anchors.

libdd embeds only `roots/prod/{config,director}_root.json`. There is no
`AgentlessConfig::site` field, no override option, and no staging/gov asset.
A staging or gov tenant cannot use the agentless client today — signature
verification would fail on first update.

### 2. Persistent cache (BoltDB)
Agent uses `uptane/transactional_store.go` (BoltDB) to persist TUF metadata,
target files, and the org UUID across restarts. Restarts therefore continue
from the last `targets`/`snapshot`/`timestamp` version.

libdd uses `tuf::repository::EphemeralRepository` for both `config` and
`director` clients. After any restart the request starts again at
`current_config_root_version = CONFIG_ROOT_VERSION` (16), forcing the backend
to re-emit the full root chain, snapshot, and targets. This costs bandwidth
and increases the cold-start window where no config is yet applied.

### 3. Org UUID handshake & verification
Agent:
- Calls `/api/v0.1/org` via `newRCBackendOrgUUIDProvider` and stores the UUID
  in BoltDB, keyed by `configLocalStore.GetMetaVersion(metaRoot)` so root
  rotation can recover from a bad/locked-out UUID.
- Sends `LatestConfigsRequest.OrgUuid` in every request.
- In `verifyOrg`, parses the snapshot custom field and asserts
  `snapshot.custom.OrgUUID == stored OrgUUID`.

libdd:
- Implements `get_org_data()` returning `OrgDataResponse` but **never calls
  it.**
- Always sends `org_uuid: String::new()`.
- Never reads `snapshot.custom.OrgUUID`. A maliciously or accidentally
  cross-org-routed update would not be detected.

### 4. Org ID verification (`WithOrgIDCheck`)
Agent supports asserting all `director` target paths belong to the configured
`orgID` (parsed from the legacy RC-key format), skipping `SourceEmployee`
paths. libdd has no equivalent — there is no `org_id` in `AgentlessConfig`.

### 5. Org status polling (`/api/v0.1/status`)
Agent runs a separate `orgStatusPoller` every `defaultRefreshInterval`
(1 min) calling `/api/v0.1/status` and logs whether RC is enabled and
authorized. It also uses the result to decide whether to log refresh errors
at `Warn`/`Error` or just `Debug`.

libdd: `get_org_status()` is implemented but never called anywhere in the
crate. There is no equivalent polling, and no degradation of log level when
RC is simply disabled for the org.

### 6. RC-key (`DDRCM_*`) auth
Agent supports the legacy RC-key in `getRemoteConfigAuthKeys`:
- Base32-decodes `DDRCM_<key>`, msgpack-decodes into `{AppKey, Datacenter, OrgID}`.
- Sends `DD-Application-Key` header in addition to `DD-Api-Key`.
- Uses the `OrgID` for `WithOrgIDCheck`.

libdd only supports `DD-Api-Key` via `Endpoint::set_standard_headers`. No
`DDRCM_` support.

### 7. PAR-JWT (Private Action Runner) auth
Agent supports `WithPARJWT(jwt)` and exposes `UpdatePARJWT(jwt)` to rotate the
token at runtime, adding the `DD-PAR-JWT` header. libdd has no equivalent.

### 8. Dynamic credentials rotation
Agent: `UpdateAPIKey(string)` and `UpdatePARJWT(string)`. The `apiKeyUpdateCallback`
re-fetches `OrgData` and verifies the stored org UUID hasn't changed (catches
accidental key swaps to a different organization).

libdd: API key is captured at construction inside `Endpoint`. There's no
public API to rotate it, and certainly no org-uuid-stability check.

### 9. Backoff / retry / error reporting on the wire
Agent:
- `backoff.NewExpBackoffPolicy(2.0, 30.0, maxBackoff.Seconds(), 2, false)` —
  exponential backoff with `[minimalMaxBackoffTime=2m, maximalMaxBackoffTime=5m]`
  clamps; configurable via `WithMaxBackoffInterval`.
- `calculateRefreshInterval = defaultRefreshInterval + backoffTime`.
- Sets `LatestConfigsRequest.HasError = true` and `Error = err.Error()` on the
  next poll after a failure, so the backend can observe client-side issues.
- Counts 503/504 separately to raise log level after threshold.
- Counts auth (401) errors and after `initialFetchErrorLog=5` downgrades them
  to Debug.

libdd:
- No backoff. The caller polls at the server-recommended `refresh_interval`.
- Always sends `has_error: false, error: ""`. The backend cannot tell the
  client failed last poll.
- No HTTP status classification. Any non-2xx returns the raw body in
  `parse_rc_response`.

### 10. `flush` / `CONFIG_STATUS_EXPIRED` semantics
Agent: in `ClientGetConfigs`, when `directorLocalStore`'s `timestamp.json` has
expired (`TimestampExpires().Before(now)`), it returns a
`ClientGetConfigsResponse{ConfigStatus: CONFIG_STATUS_EXPIRED}` to force
downstream clients to drop their state.

libdd: only checks per-target `custom.expires` in
`BorrowedTufTarget::try_create`. There's no global "timestamp expired, drop
everything" gate, and no signaling channel back to a caller above
`ConfigFetcher` to indicate that state is stale.

### 11. Delegated targets
`apply()` has an explicit TODO:
```
// TODO: We do not store the delegated targets metadata
// This will need to be revisited in order to support proper Uptane
// verification of the full configuration data.
```
Agent stores `delegated_targets` via `directorRemoteStore.update(response)`
and the go-tuf client walks delegations during verification. libdd's
verification is therefore not a complete Uptane verification today.

### 12. Multi-client support
`fetch_config` has a TODO: only one `Client` is sent per request. The agent
sends `ActiveClients` (all currently-known tracer clients) and runs
`executeTracerPredicates` to filter director targets per client, so a single
agent process serves many tracers. PHP (multi-process) is the canonical case
the TODO calls out.

### 13. Subscriptions / streaming
Agent has `CreateConfigSubscription` (gRPC stream) so internal agent
components (e.g. live-debugging, symbol DB) receive complete-view pushes.
N/A for the agentless client by design but worth noting.

### 14. Telemetry
Agent has `RcTelemetryReporter` with `IncRateLimit`, `IncTimeout`,
`SetConfigSubscriptionsActive`, etc., plus `expvar`-exported state
(`orgEnabled`, `apiKeyScoped`, `lastError`). libdd has zero telemetry.

### 15. TLS guards
Agent enforces:
- `baseURL.Scheme == "https"` unless `remote_configuration.no_tls=true`.
- Rejects `InsecureSkipVerify` unless `remote_configuration.no_tls_validation=true`.
- Forces `transport.IdleConnTimeout = 30s` (backend cuts idle at ~45s).

libdd: `make_agentless_configs_endpoint` requires `https`. There's no escape
hatch for local-proxy / staging testing, no `IdleConnTimeout` tuning that we
can see (depends on `libdd_capabilities_impl::NativeHttpClient`).

### 16. Cache bypass rate limiter
Agent: `refreshBypassLimiter` (token-bucket per-window) and
`refreshBypassCh` allow a new tracer to trigger an immediate refresh, bounded
by `WithClientCacheBypassLimit(limit, ...)` (default 5/window, [1,10]).
N/A in libdd because there is one consumer per fetcher.

### 17. Refresh-interval validation & override semantics
Agent:
- `WithRefreshInterval` clamps to `>= minimalRefreshInterval (5s)`.
- `getRefreshIntervalLocked` only accepts server-recommended intervals in
  `[1s, 1m]`, otherwise ignores.
- Server override only honored when caller didn't explicitly set it
  (`refreshIntervalOverrideAllowed`).

libdd:
- Always uses server-supplied `agent_refresh_interval` via
  `Duration::from_secs` with **no bounds check** — backend can set it to any
  u64.
- Default is hardcoded `Duration::from_secs(60)` — fine.
- No caller-facing override option.

---

## Probable bugs

### B1. Stray `dbg!` macros
Left in three places, will emit to stderr in release builds:
- `make_agentless_configs_endpoint` (line ~50): `dbg!(&e);`
- `ConfigFetcher::new` in fetcher.rs: `dbg!(state.invariants.agentless_enabled)`
- `get_latest_config`: `dbg!(&req);` and `dbg!(debug_latest_configs_response(&res));`

These leak the full request (containing the API key indirectly via
endpoint info, and tracer telemetry) and the full TUF response to stderr.
Should be removed or behind `tracing::debug!`.

### B2. Hardcoded `FAKE_AGENT_VERSION = "7.78.4"`
The backend uses `agent_version` for feature gating and telemetry. Pinning a
fake value means:
- Bug reports look like agent 7.78.4.
- The version will eventually fall behind any minimum-version gating.

Should be `concat!("libdatadog/", env!("CARGO_PKG_VERSION"))` or accept a
caller-provided value.

### B3. `trim_hash_target_path` uses `std::path::Path`
TUF paths are always `/`-separated. On Windows, `std::path::Path::components`
treats `\` as a separator too, which could mis-parse adversarial paths. Use
plain `str::rsplit_once('/')`.

### B4. Target files are stored twice in the director "remote" repo
`apply()`:
```rust
repo.store_target(&trimmed_target_path, ...).await?;
// (duplicated, commented-out section above shows the intent)
repo.store_target(&TargetPath::new(&target_file.path)?, ...).await?;
```
Each file is stored at both the original `<hash>.<name>` and the trimmed
`<name>` path. Then `fetch_target` reads back from `director_client.remote_repo()`
using the original target path. The trimmed copy is therefore unused. This
doubles memory for every target file per refresh.

### B5. `fetch_target` reads from the **unverified** remote repo
The comment acknowledges this: "Fetch from the content from the remote
__Unverified__ repo. This is fine as we are comparing the (hash + len) with a
validated target." It is correct given the post-fetch hash check, but the
agent goes through `directorTUFClient.DownloadBatch`, which performs the
verification as part of the download (proper TUF). Both end up safe; the
libdd path is just unconventional.

### B6. `opaque_backend_state` is only updated when present
```rust
if let Some(opaque_backend_state) = opaque_backend_state {
    self.opaque_backend_state = opaque_backend_state;
}
```
If the backend ever stops sending the field, libdd will keep echoing the
stale value forever. Agent overwrites unconditionally with whatever's in
`targetsCustom.OpaqueBackendState` (including empty).

### B7. `active_clients[].last_seen` is overwritten with `now`
```rust
active_clients: vec![remoteconfig::Client {
    last_seen: now,
    ..c
}],
```
Caller-provided `last_seen` is discarded. Not necessarily wrong (we only have
one client), but combined with the multi-client TODO it will need to be
fixed.

### B8. `store(... tm.version as u32)` truncates silently
`remoteconfig::TopMeta.version` is `u64` on the wire; cast to `u32` in
`MetadataVersion::Number`. Realistic versions are far below `u32::MAX` but a
malformed backend response would wrap silently.

### B9. `target_cache` doubles the cached file content
The comment notes:
```
// TODO: Not sure this is needed if the wrapped client already caches files?
target_cache: HashMap<tuf::metadata::TargetPath, CachedFile>,
```
Each verified file is held both inside the TUF repo's in-memory store **and**
in `target_cache`. For PHP-style multi-process or large config blobs this is
wasted memory. Worth deciding whether the upstream cache is authoritative.

### B10. `make_agentless_configs_endpoint` rejects `api_key.is_none()` even if a PAR-JWT or app-key would suffice
The check is `e.api_key.is_some()`. There is no alternative auth scheme today
in libdd, but this hardcodes the assumption and would have to change for B7
(RC key) and #7 (PAR JWT).

### B11. `agentless_enabled` is silently downgraded to agent
In `ConfigFetcherState::new`:
```rust
warn!("agentless enabled but the hostname is empty. Downgrading to agent endpoint");
warn!("agentless enabled but the endpoint is invalid. Downgrading to agent endpoint");
```
The caller asks for agentless, gets agent. Tracers that have no Datadog agent
to fall back to will silently fail with connection errors against
`/v0.7/config`. Should be a hard error or surface a status the caller can
react to.

### B12. `#[allow(dead_code)]` on `CONFIG_ROOT` is misleading
The constant **is** used inside `AgentlessFetcher::new` to build the
`config_client`. The `dead_code` allow + stale comment ("reserved for TUF
config-repo init") is a leftover.

### B13. `BorrowedTufTarget::try_create` interprets `custom.expires` as seconds, multiplied to ms
```rust
if expiry_ts * 1000 <= now_unix_milli_ts()
```
- `expiry_ts` is read via `as_u64()`, so a JSON value already in ms would be
  off by a factor of 1000.
- `expiry_ts * 1000` can wrap a `u64` for very large values (DoS via
  malformed metadata; trivially unlikely in practice but worth checked-mul).

The unit convention should be confirmed against what the backend / TUF spec
emits for that field. Agent uses go-tuf's standard expiry handling, not a
custom `expires` integer.

### B14. `parse_rc_response` rejects only the body of non-2xx, no `Retry-After` handling
- 401 isn't distinguished — agent has `ErrUnauthorized` mapped to debug-level
  logging.
- 503/504 aren't distinguished from 5xx generally.
- `Retry-After` header is ignored.

### B15. `refresh_interval` not clamped
Agent caps `agent_refresh_interval` to `[1s, 1m]`. libdd accepts whatever the
server says, including 0 (would cause a tight loop in the consumer).

---

## Missing configuration parameters (Option-for-Option)

Agent `Option` → libdd equivalent today:

| Agent `Option` | libdd equivalent | Notes |
|---|---|---|
| `WithTraceAgentEnv` | ❌ (sends empty `trace_agent_env`) | |
| `WithDatabaseFileName` | n/a (in-memory) | |
| `WithDatabasePath` | n/a | |
| `WithConfigRootOverride(site, override)` | ❌ — only prod root baked in | **blocker for non-prod sites** |
| `WithDirectorRootOverride(site, override)` | ❌ | **blocker for non-prod sites** |
| `WithRefreshInterval` | ❌ — only server-driven | |
| `WithOrgStatusRefreshInterval` | ❌ — org status never polled | |
| `WithMaxBackoffInterval` | ❌ — no backoff at all | |
| `WithRcKey(DDRCM_*)` | ❌ | |
| `WithAPIKey` | via `Endpoint.api_key` (immutable) | |
| `WithPARJWT` | ❌ | |
| `WithClientCacheBypassLimit` | n/a | |
| `WithClientTTL` | n/a (single client) | |
| `WithAgentPollLoopDisabled` | n/a | |
| `tagsGetter` | ❌ — `tags: vec![]` always | |
| `hostname` | ✅ `AgentlessConfig.hostname` | |
| `agentVersion` | ❌ — hardcoded `"7.78.4"` | |
| `cfg("api_key")` runtime updates | ❌ | |

Additionally, agent exposes hostname/agent-uuid via `LatestConfigsRequest`:
- `agent_uuid` — always empty in libdd.
- `tags` — always empty.
- `trace_agent_env` — always empty.

These are used by the backend for routing/diagnostics; missing them is not a
correctness issue but reduces observability and may break some product
features that target by env/host-tags.

---

## Recommended next steps

The list below is opinionated about what would be required to actually
replace the agent client. Ordering = highest impact first.

- [ ] **Embed non-prod TUF roots** (staging, gov) and auto-select based on
      the `site` in the configured endpoint (the `{site}` in
      `https://config.{site}`). Additionally, expose an optional override
      **file path** on `ConfigInvariants` so a caller can supply custom
      roots from disk (`config_root_override_path`,
      `director_root_override_path`).
- [ ] **Propagate `has_error` / `error`** from caller into the next request
      (the `ConfigFetcher` already has `client_state.last_error` — wire it
      into `agentless_fetcher.fetch_config`).
- [ ] **Add exponential backoff** on consecutive failures with the following
      schedule (not the agent's `[2m, 5m]`):
      - 1st error → no backoff
      - 2nd error → random in `[30s, 60s]`
      - 3rd error → random in `[60s, 120s]`
      - 4th+ error → `120s` max
- [ ] **Clamp `agent_refresh_interval`** to `[1s, 1m]`, mirroring agent.
- [ ] **Replace `std::path::Path` in `trim_hash_target_path`** with explicit
      `/` splitting.

## Verification

This is a comparison document, not a code change. Sign-off criteria:
- Agree with the assessment of which gaps are blockers for replacing the
  agent client today (sites, org UUID, debug-print removal, agent version).
- Decide which of the "future" items must land before declaring agentless
  GA, and which are acceptable carry-over.
