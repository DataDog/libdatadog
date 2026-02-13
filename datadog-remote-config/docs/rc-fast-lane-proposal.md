# Remote Config Fast Lane: Priority Fetching for Latency-Sensitive Products

## Problem Statement

When a large fleet of tracers boots simultaneously (e.g. a deployment rollout, auto-scaling
event, or cluster restart), the Remote Configuration (RC) subsystem experiences a thundering
herd problem that causes unacceptable delays — **p50=76s, p90=80s** with default Agent config
— for products like `FFE_FLAGS` (Feature Flag Evaluation) that are critical to application
startup.

> **Note:** Earlier measurements showed a 30-second cliff (p50=29s), but this was an artifact
> of `SetProviderAndWait`'s hard-coded 30-second timeout in dd-trace-go (`defaultInitTimeout`
> in `openfeature/provider.go:36`). When the provider's `Init()` times out, it returns
> `context.DeadlineExceeded` — but the provider is still registered and RC continues in the
> background. The flag becomes available on the next poll cycle, artificially inflating boot
> time by the timeout duration. Switching to non-blocking `SetProvider` with event handlers
> eliminated this cliff entirely and revealed the true RC delivery latency.

### Request Flow (End-to-End)

```
Tracer (libdatadog)                    DD Agent                         Backend
─────────────────                    ────────                         ───────
POST /v0.7/config ──────────────►  gRPC ClientGetConfigs()
  products: [FFE_FLAGS,               │
   APM_TRACING, ASM...]               ├─ Is client active? (30s TTL)
                                       │   NO → trigger cache bypass
                                       │         (rate-limited: 5/interval)
                                       │         (blocks up to 2s)
                                       │             │
                                       │             ├──► POST /configurations ──► Backend
                                       │             │    (s.mu released during wait)
                                       │             ◄────────────────────────────
                                       │   YES → serve from local TUF state
                                       │
                                       ├─ Filter by tracer predicates
                                       ├─ Diff against client's cached files
                                       ◄─ Return matched configs
◄──────────────────────────────────
  sleep(5s, fixed, no jitter)          Background: poll backend every 1min
  repeat                                 (with backoff on errors)
```

### Where the Bottleneck Is

There are **three layers** that compound during a thundering herd, each in a different repo:

#### Layer 1: libdatadog (this repo) — Tracer RC Client

| Issue | Source | Impact |
|-------|--------|--------|
| All products bundled into one request | [`fetcher.rs:302-335`][lb1] — single `ClientGetConfigsRequest` with all `product_capabilities.products` | FFE_FLAGS blocked by slow ASM_DATA payloads |
| Fixed 5s polling, no jitter | [`shared.rs:262`][lb2] (`AtomicU64::new(5_000_000_000)`), [`shared.rs:354`][lb3] (bare `sleep`, no jitter) | All tracers retry in lockstep |
| `agent_refresh_interval` parsed but **never applied** | [`targets.rs:47`][lb4] (field definition in `TargetsCustom`), only other ref is [`test_server.rs:122`][lb5] (test stub) — never read by polling loop | Server cannot tell clients to slow down |
| No backoff on errors | [`shared.rs:346-348`][lb6] (error branch logs + cleans up, no interval change), [`shared.rs:354`][lb3] (same fixed sleep on error or success) | Failed clients hammer the Agent immediately |
| No HTTP 429 handling | [`fetcher.rs:358-369`][lb7] — only `StatusCode::OK` and `StatusCode::NOT_FOUND` special-cased; 429 hits generic `bail!()` | Rate limit responses treated as generic errors |
| 100-permit FIFO semaphore, no prioritization | [`multitarget.rs:186`][lb8] (`DEFAULT_CLIENTS_LIMIT = 100`), [`multitarget.rs:195`][lb9] (init), [`multitarget.rs:551-555`][lb10] (comment: "no prioritization or anything") | Fast lane products queue behind everything |
| 3s HTTP timeout | [`libdd-common/src/lib.rs:241`][lb11] (`DEFAULT_TIMEOUT: u64 = 3_000` ms) | Requests timeout under load, retry with no escalation |

[lb1]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/fetcher.rs#L302-L335
[lb2]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/shared.rs#L262
[lb3]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/shared.rs#L354
[lb4]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/targets.rs#L47
[lb5]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/test_server.rs#L122
[lb6]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/shared.rs#L346-L348
[lb7]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/fetcher.rs#L358-L369
[lb8]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/multitarget.rs#L186
[lb9]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/multitarget.rs#L195
[lb10]: https://github.com/DataDog/libdatadog/blob/main/datadog-remote-config/src/fetch/multitarget.rs#L551-L555
[lb11]: https://github.com/DataDog/libdatadog/blob/main/libdd-common/src/lib.rs#L241

#### Layer 2: DD Agent (`datadog-agent` repo) — RC Service

Source: `pkg/config/remote/service/service.go` (all line refs below are in this file unless noted).

| Issue | Source | Impact |
|-------|--------|--------|
| `s.mu.Lock()` held across `ClientGetConfigs` for filtering/matching; released during bypass wait | [L975-976][ag1] (lock + defer unlock), [L990][ag2] (explicit unlock before bypass wait), [L1016][ag3] (relock after bypass) | Active-client requests serialize on the mutex for predicate matching; new-client bypass wait does NOT hold the mutex |
| Cache bypass rate-limited to 5/interval (fixed-window) | [L52][ag4] (`defaultCacheBypassLimit = 5`), [L737][ag5] (`refreshBypassLimiter.Limit()`), impl in [`clients.go:22-50`][ag6] (fixed-window: truncate time, reset allowance) | 6th+ new tracer in a window gets stale/empty data |
| Bypass blocks up to 2s (`newClientBlockTTL`) | [L51][ag7] (`newClientBlockTTL = 2 * time.Second`), [L999-1004][ag8] (1st select: send on channel or timeout), [L1009-1014][ag9] (2nd select: wait for response or timeout with remaining budget) | New tracers at boot wait up to 2s for backend round-trip |
| `refreshBypassCh` is unbuffered channel | [L702][ag10] (`make(chan chan<- struct{})` — no buffer), [L1000][ag11] (send blocks until poll loop reads) | Only one bypass can be in-flight; others timeout at 2s |
| Client "active" TTL = 30s | [L49][ag12] (`defaultClientsTTL = 30 * time.Second`), [`clients.go:52-54`][ag13] (`expired()`: `clock.Now().After(lastSeen + TTL)`), [`clients.go:85-92`][ag14] (`active()`) | After 30s without a request, client triggers bypass again (but the 5/interval limit still applies) |
| Backend poll default 1 minute | [L47][ag15] (`defaultRefreshInterval = 1 * time.Minute`) | Stale data between polls; bypass is only escape hatch |
| No product-level prioritization | [L974-1159][ag16] (single code path), [`tracer_predicates.go:27-71`][ag17] (filters by product + tracer attributes, no priority ordering) | FFE_FLAGS and ASM_DATA use identical code path |

[ag1]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L975-L976
[ag2]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L990
[ag3]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L1016
[ag4]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L52
[ag5]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L737
[ag6]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/clients.go#L22-L50
[ag7]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L51
[ag8]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L999-L1004
[ag9]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L1009-L1014
[ag10]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L702
[ag11]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L1000
[ag12]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L49
[ag13]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/clients.go#L52-L54
[ag14]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/clients.go#L85-L92
[ag15]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L47
[ag16]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/service.go#L974-L1159
[ag17]: https://github.com/DataDog/datadog-agent/blob/main/pkg/config/remote/service/tracer_predicates.go#L27-L71

#### Layer 3: Backend

| Issue | Impact |
|-------|--------|
| Rate limits Agent via `opaque_backend_state` | Agent slows down, tracers don't (libdatadog ignores `agent_refresh_interval`) |
| Single `/configurations` endpoint for all products | No way to prioritize latency-sensitive products |

### Compounding Scenario: 100 Tracers Boot Simultaneously

1. **T=0s**: 100 tracers call `/v0.7/config`. All are new clients (never seen before).
2. **Agent**: Each tracer acquires `s.mu.Lock()` ([L975][ag1]), checks `clients.active()` → false
   (never seen), calls `clients.seen()`, then **releases the mutex** ([L990][ag2]) before sending
   on the unbuffered `refreshBypassCh` ([L1000][ag11]). However, the bypass channel is unbuffered
   and the poll loop can only process one bypass at a time. The `refreshBypassLimiter`
   ([L737][ag5]) allows only 5 bypasses per refresh window ([L52][ag4]). So: at most 5 tracers
   get a backend round-trip; the rest either timeout at 2s waiting to send on the channel, or
   are rate-limited and get stale/empty data.
3. **T=2s**: Bypass timeout fires (`newClientBlockTTL`, [L51][ag7]). The <=5 lucky tracers got
   fresh data (if backend responded in time). The other 95+ got whatever was cached (likely
   nothing on first boot).
4. **T=5s**: All 100 tracers retry simultaneously (no jitter — [`shared.rs:354`][lb3]). But now
   they're "active" (`defaultClientsTTL = 30s`, [L49][ag12]), so no bypass — they get whatever
   the Agent has cached. If the Agent's 1-minute poll ([L47][ag15]) hasn't run yet, this is
   still stale.
5. **T=10-60s**: Cycle repeats every 5s, in lockstep. Eventually the Agent's background poll
   completes and fresh data becomes available.

**Result**: FFE_FLAGS, which could be a 200-byte payload, is delayed 30+ seconds because it's
trapped in the same pipeline as everything else.

---

## Design Decision: Agent-Baked vs. User-Configurable

| Aspect | Agent-Baked (hardcoded) | User-Configurable |
|--------|------------------------|-------------------|
| **Which products are fast lane** | Compiled into libdatadog (e.g. `FFE_FLAGS` is always fast lane) | User/tracer specifies which products are fast lane at registration time |
| **Interval tuning** | Hardcoded fast interval (e.g. 1s initial, 5s steady-state) | Configurable via `DD_RC_FAST_LANE_INTERVAL` or API parameter |
| **Rollout risk** | Low — Datadog controls the list; no misconfiguration risk | Medium — users could mark too many products as fast lane, defeating the purpose |
| **Flexibility** | Cannot adapt to new products without a library release | Customers with custom RC products can opt-in |
| **Complexity** | Lower — enum-level annotation | Higher — needs validation, documentation, runtime config |

**Recommendation: Hybrid approach.**

- The **set of fast-lane-eligible products** is baked into the library — only products we know
  are latency-sensitive (currently: `FFE_FLAGS`) are eligible.
- The **activation** of fast lane behavior is user-configurable — tracers opt-in by setting an
  environment variable or passing a configuration flag. This lets us ship the capability without
  forcing it on everyone, and gives us a kill switch.
- The **fast lane interval** has a sensible default (1s for first fetch, 5s steady-state) but can
  be overridden via configuration.

---

## Proposed Solution: Fast Lane Fetching

### Architecture Overview

```
              ┌─────────────────────────────────────────────────┐
              │           MultiTargetFetcher                    │
              │                                                 │
              │  ┌───────────────────┐  ┌────────────────────┐  │
              │  │  Fast Lane Pool   │  │  Standard Pool     │  │
              │  │  Semaphore: 20    │  │  Semaphore: 100    │  │
              │  └────────┬──────────┘  └─────────┬──────────┘  │
              │           │                       │             │
              │  ┌────────▼──────────┐  ┌─────────▼──────────┐  │
              │  │  SharedFetcher    │  │  SharedFetcher      │  │
              │  │  products:        │  │  products:          │  │
              │  │    [FFE_FLAGS]    │  │    [APM_TRACING,    │  │
              │  │  interval: 1s    │  │     ASM, ASM_DATA]  │  │
              │  │  first fetch: 0s │  │  interval: 5s       │  │
              │  └────────┬──────────┘  └─────────┬──────────┘  │
              │           │                       │             │
              └───────────┼───────────────────────┼─────────────┘
                          │                       │
              ┌───────────▼───────────────────────▼─────────────┐
              │              DD Agent /v0.7/config               │
              │                                                 │
              │  ClientGetConfigs():                            │
              │    ┌──────────────────────────────────────┐     │
              │    │ Fast lane request? (single product,  │     │
              │    │ small payload) → serve from cache    │     │
              │    │ with higher bypass allowance         │     │
              │    └──────────────────────────────────────┘     │
              │                                                 │
              │  Background poll: /configurations (1min)        │
              └─────────────────────────────────────────────────┘
```

### Changes by Repo

---

### Part A: libdatadog Changes (this repo)

#### A1. Mark products as fast-lane-eligible (`path.rs`)

Add an intrinsic property to `RemoteConfigProduct`:

```rust
impl RemoteConfigProduct {
    /// Products that are eligible for fast lane fetching.
    /// These are latency-sensitive products where startup time matters.
    pub fn is_fast_lane_eligible(&self) -> bool {
        matches!(self, RemoteConfigProduct::FfeFlags)
    }
}
```

This is the baked-in list. Adding a new fast-lane product requires a library change — this is
intentional to prevent abuse.

#### A2. Split product sets at registration time (`multitarget.rs`)

When `add_runtime()` or `add_target()` is called, partition the requested products into fast lane
and standard sets:

```rust
// In add_target() or add_runtime():
let (fast_lane_products, standard_products): (Vec<_>, Vec<_>) = product_capabilities
    .products
    .iter()
    .partition(|p| p.is_fast_lane_eligible() && self.fast_lane_enabled);
```

If `fast_lane_products` is non-empty, create/reuse a **separate** `SharedFetcher` for them.
The standard products go through the existing path unchanged.

#### A3. Dedicated fast lane fetcher pool (`multitarget.rs`)

Add a second semaphore and fetcher tracking structure:

```rust
pub struct MultiTargetFetcher<N: NotifyTarget, S: FileStorage + Clone + Sync + Send> {
    // ... existing fields ...

    /// Whether fast lane fetching is enabled
    fast_lane_enabled: bool,
    /// Separate semaphore for fast lane fetchers — smaller pool, guaranteed availability
    fast_lane_semaphore: Semaphore,
    /// Fast lane polling interval (nanoseconds). Default: 1s initial, then 5s steady-state.
    fast_lane_interval: AtomicU64,
    /// Fast lane fetchers by target
    fast_lane_services: Mutex<HashMap<Arc<Target>, KnownTarget>>,
}
```

The fast lane semaphore is intentionally small (e.g. 20 permits) since fast lane fetchers are
lightweight (small payloads, single product).

#### A4. Honor `agent_refresh_interval` from server (`fetcher.rs`, `shared.rs`)

This is a prerequisite fix that benefits both lanes. After parsing the targets response in
`fetch_once()`:

```rust
// In fetch_once(), after parsing targets_list (fetcher.rs ~line 404):
if let Some(server_interval) = targets_list.signed.custom.agent_refresh_interval {
    opaque_state.server_interval = Some(server_interval);
}
```

In the polling loop (`shared.rs`), apply it:

```rust
// In SharedFetcher::run(), after successful fetch:
if let Some(interval_ms) = opaque_state.server_interval.take() {
    let interval_ns = interval_ms * 1_000_000;
    // Only allow server to slow us down, not speed us up beyond our configured minimum
    let current = self.interval.load(Ordering::Relaxed);
    if interval_ns > current {
        self.interval.store(interval_ns, Ordering::Relaxed);
    }
}
```

#### A5. Add jitter + backoff on errors (`shared.rs`)

Currently the polling loop just logs errors and retries at the same interval. Add exponential
backoff with jitter:

```rust
Err(e) => {
    clean_inactive();
    error!("{:?}", e);
    consecutive_errors += 1;
    // Exponential backoff with jitter, capped at 30s
    let backoff_ns = std::cmp::min(
        self.interval.load(Ordering::Relaxed) * (1 << consecutive_errors.min(3)),
        30_000_000_000,
    );
    let jitter_ns = rand::random::<u64>() % (backoff_ns / 4);
    extra_sleep = Duration::from_nanos(backoff_ns + jitter_ns);
}
// Reset consecutive_errors on success
```

#### A6. Configuration surface

**Environment variables:**

| Variable | Default | Description |
|----------|---------|-------------|
| `DD_RC_FAST_LANE_ENABLED` | `false` | Enable fast lane fetching for eligible products |
| `DD_RC_FAST_LANE_INTERVAL_MS` | `1000` | Fast lane polling interval in milliseconds |
| `DD_RC_FAST_LANE_INITIAL_INTERVAL_MS` | `0` | Delay before first fast lane fetch (0 = immediate) |

**Programmatic API (for library consumers):**

```rust
impl MultiTargetFetcher {
    pub fn set_fast_lane_enabled(&self, enabled: bool);
    pub fn set_fast_lane_interval(&self, interval_ms: u64);
}
```

---

### Part B: DD Agent Changes (`datadog-agent` repo)

The Agent side is equally important — without these changes, splitting the request in libdatadog
just sends two requests into the same bottleneck.

#### B1. Increase cache bypass budget for fast lane requests (`service.go`)

The current bypass rate limiter allows only 5 bypasses per refresh interval. During a thundering
herd, this is exhausted immediately. For requests that only ask for fast-lane products, use a
**separate, higher-budget rate limiter**:

```go
// In CoreAgentService:
refreshBypassLimiter         rateLimiter // existing: 5/interval for standard
fastLaneBypassLimiter        rateLimiter // new: 50/interval for fast lane products

// In ClientGetConfigs():
if isFastLaneRequest(request) {
    limiter = s.fastLaneBypassLimiter
} else {
    limiter = s.refreshBypassLimiter
}
```

A request is "fast lane" if it only contains fast-lane-eligible products (e.g. `products ==
["FFE_FLAGS"]`). This can be determined by the Agent from the product list in the request.

#### B2. Reduce mutex contention for fast lane requests (`service.go`)

Currently `ClientGetConfigs` acquires `s.mu.Lock()` at entry ([L975][ag1]) and uses
`defer s.mu.Unlock()` ([L976][ag1]). For new clients, the mutex is explicitly released
([L990][ag2]) before the bypass wait and reacquired after ([L1016][ag3]). This means the mutex
is held for predicate matching and response building, but **not** for the up-to-2s bypass wait.
Active-client requests (no bypass) hold it for the full function body. All tracer requests still
serialize on this single mutex for the filtering/matching work.

The key improvement is ensuring fast lane requests don't contend with standard requests during
the bypass:

```go
// Separate bypass channels:
refreshBypassCh         chan<- chan<- struct{} // standard products
fastLaneBypassCh        chan<- chan<- struct{} // fast lane products

// In poll loop, prioritize fast lane bypasses:
select {
case response := <-fastLaneBypassCh:
    // Always process fast lane bypasses first
    ...
case response := <-refreshBypassCh:
    // Standard bypass
    ...
case <-s.clock.After(refreshInterval):
    // Scheduled poll
    ...
}
```

#### B3. Shorter bypass timeout for fast lane (`service.go`)

The current `newClientBlockTTL = 2 * time.Second` is too long for a fast lane request. FFE_FLAGS
payloads are small — if the bypass doesn't complete quickly, fall through and serve whatever is
cached:

```go
const (
    newClientBlockTTL         = 2 * time.Second   // standard (unchanged)
    fastLaneClientBlockTTL    = 500 * time.Millisecond // fast lane
)
```

#### B4. Agent-side product awareness (`service.go`)

Add a helper to classify requests:

```go
var fastLaneProducts = map[string]struct{}{
    "FFE_FLAGS": {},
}

func isFastLaneRequest(req *pbgo.ClientGetConfigsRequest) bool {
    if req.Client == nil || len(req.Client.Products) == 0 {
        return false
    }
    for _, p := range req.Client.Products {
        if _, ok := fastLaneProducts[p]; !ok {
            return false
        }
    }
    return true
}
```

#### B5. Configuration (`remote_configuration.fast_lane.*`)

| Parameter | Default | Description |
|-----------|---------|-------------|
| `remote_configuration.fast_lane.cache_bypass_limit` | `50` | Bypass budget per interval for fast lane requests |
| `remote_configuration.fast_lane.block_ttl_ms` | `500` | Max time to block on bypass for fast lane requests |

---

### What This Does NOT Change

- The `/v0.7/config` endpoint and protobuf request format are unchanged.
- The TUF/Uptane verification pipeline is unchanged.
- The backend `/configurations` endpoint is unchanged — no backend work required.
- File storage, reference counting, and notification mechanisms are shared across both lanes.
- Standard (non-fast-lane) product behavior is completely unchanged.

---

## Tradeoffs

| Pro | Con |
|-----|-----|
| FFE_FLAGS fetch decoupled from ASM/APM congestion | Two requests per target instead of one (marginal overhead) |
| Sub-second first fetch for feature flags at boot | Additional fetcher state and semaphore in libdatadog |
| Server backoff finally honored (benefits everyone) | Agent needs coordinated changes (two repos) |
| Kill switch via env var for safe rollout | Fast lane bypass budget must be tuned to avoid overwhelming backend |
| Backoff + jitter fixes thundering herd for all products | Need to verify concurrent fetcher access to shared file storage |

---

## Phased Rollout Plan

### Phase 1: Fix the fundamentals (libdatadog only, no feature flag)

- Honor `agent_refresh_interval` from server responses (A4)
- Add exponential backoff with jitter on errors (A5)
- **Impact**: Reduces thundering herd severity for all products. No Agent changes needed.
  Safe to ship immediately.

### Phase 2: Fast lane in libdatadog (behind `DD_RC_FAST_LANE_ENABLED=false`)

- Product-level fast lane eligibility (A1)
- Product set splitting and dedicated fetcher pool (A2, A3)
- Configuration surface (A6)
- **Impact**: libdatadog sends separate requests for FFE_FLAGS, but Agent treats them the same
  as any other request. Still an improvement because FFE_FLAGS requests are tiny and fast.

### Phase 3: Fast lane in DD Agent

- Fast lane bypass budget and classification (B1, B4)
- Separate bypass channels and shorter timeout (B2, B3)
- Agent-side configuration (B5)
- **Impact**: Agent actively prioritizes fast lane requests. Full benefit realized.

### Phase 4: Enable by default

- Flip `DD_RC_FAST_LANE_ENABLED` default to `true`
- Monitor request volume, bypass rates, and FFE_FLAGS latency at boot

---

## Tracer SDK Coverage: Where Do Changes Need to Land?

Each tracer SDK has its own independent RC client implementation. **libdatadog's RC client is only
used by dd-trace-php** (via the sidecar). All other tracers implement RC natively in their own
language:

| Tracer | RC Implementation | Polling interval | Jitter/backoff? | Bundles all products? |
|--------|------------------|-----------------|-----------------|----------------------|
| **dd-trace-php** | libdatadog sidecar ([`ext/remote_config.c`][tr1] → `ddog_process_remote_configs()`) | 5s (via libdatadog [`shared.rs:262`][lb2]) | No | Yes |
| **dd-trace-go** | Native Go, `net/http` ([`internal/remoteconfig/`][tr2]) | 5s ([`config.go:59`][tr3], env `DD_REMOTE_CONFIG_POLL_INTERVAL_SECONDS`) | No (fixed [`time.NewTicker`][tr4]) | Yes ([`remoteconfig.go` `c.allProducts()`][tr5]) |
| **dd-trace-java** | Native Java, OkHttp3 ([`remote-config/`][tr6]) | 5s (`DEFAULT_POLL_PERIOD = 5000`) | No | Yes |
| **dd-trace-py** | Native Python ([`ddtrace/internal/remoteconfig/`][tr7]) | 5s ([`settings/_config.py`][tr8], env `DD_REMOTE_CONFIG_POLL_INTERVAL_SECONDS`) | No | Yes |
| **dd-trace-rb** | Native Ruby, `Net::HTTP` ([`lib/datadog/core/remote/`][tr9]) | 5s ([`configuration/settings.rb`][tr10]) | No | Yes |
| **dd-trace-dotnet** | Native C#, `HttpClient` ([`RemoteConfigurationManagement/`][tr11]) | 5s ([`RemoteConfigurationSettings.cs:17`][tr12]) | No ([`Task.Delay(_pollInterval)`][tr13]) | Yes |
| **dd-trace-js** | Native JS, Node `http`/`https` ([`packages/dd-trace/src/remote_config/`][tr14]) | 5s ([`config/defaults.js`][tr15]) | No | Yes |

[tr1]: https://github.com/DataDog/dd-trace-php/blob/master/ext/remote_config.c
[tr2]: https://github.com/DataDog/dd-trace-go/tree/main/internal/remoteconfig
[tr3]: https://github.com/DataDog/dd-trace-go/blob/main/internal/remoteconfig/config.go#L59
[tr4]: https://github.com/DataDog/dd-trace-go/blob/main/internal/remoteconfig/remoteconfig.go#L246
[tr5]: https://github.com/DataDog/dd-trace-go/blob/main/internal/remoteconfig/remoteconfig.go#L798
[tr6]: https://github.com/DataDog/dd-trace-java/tree/master/remote-config
[tr7]: https://github.com/DataDog/dd-trace-py/tree/main/ddtrace/internal/remoteconfig
[tr8]: https://github.com/DataDog/dd-trace-py/blob/main/ddtrace/internal/settings/_config.py
[tr9]: https://github.com/DataDog/dd-trace-rb/tree/master/lib/datadog/core/remote
[tr10]: https://github.com/DataDog/dd-trace-rb/blob/master/lib/datadog/core/configuration/settings.rb
[tr11]: https://github.com/DataDog/dd-trace-dotnet/tree/master/tracer/src/Datadog.Trace/RemoteConfigurationManagement
[tr12]: https://github.com/DataDog/dd-trace-dotnet/blob/master/tracer/src/Datadog.Trace/RemoteConfigurationManagement/RemoteConfigurationSettings.cs#L17
[tr13]: https://github.com/DataDog/dd-trace-dotnet/blob/master/tracer/src/Datadog.Trace/RemoteConfigurationManagement/RemoteConfigurationSettings.cs#L107
[tr14]: https://github.com/DataDog/dd-trace-js/tree/master/packages/dd-trace/src/remote_config
[tr15]: https://github.com/DataDog/dd-trace-js/blob/master/packages/dd-trace/src/config/defaults.js

Ruby depends on the libdatadog gem, but only for profiling/crashtracking/telemetry — its RC
client is pure Ruby (`Net::HTTP`).

**Key finding: every tracer uses a fixed 5s poll interval with no jitter and bundles all products
into a single request.** The thundering herd behavior described in this document applies uniformly
across all language tracers, not just PHP/libdatadog.

### Implications for This Proposal

- **Part A (libdatadog)** changes only auto-benefit **PHP**. For all other tracers, equivalent
  changes (product splitting, dedicated fetcher, backoff/jitter) would need to be ported to each
  tracer's own RC client.
- **Part B (DD Agent)** changes benefit **all tracers automatically**, regardless of language.
  The Agent treats every tracer's `/v0.7/config` request the same way — if the Agent prioritizes
  fast lane requests, every tracer benefits without any client-side changes.
- **This makes the Agent-side changes (Part B) the highest-leverage investment.** They should be
  prioritized over client-side changes if resources are constrained.
- Client-side changes (product splitting, jitter) are still valuable per-tracer but can be rolled
  out incrementally, starting with the highest-traffic tracers.

---

## Load Test Results

Load test repo: `DataDog/ffe-dogfooding` branch `leo.romanovsky/multitracer-loadtest`

### Setup

- 100 Go tracer instances (`dd-trace-go` v2.5.0 with OpenFeature provider)
- 1 Datadog Agent (default config, `refresh_interval` at default 1 minute)
- Non-blocking `SetProvider` with OpenFeature event handlers (no 30s timeout artifact)
- Each instance measures: tracer start, provider ready (`ProviderReady` event), first flag
  evaluation, total boot time
- Docker Compose on a single host (Apple Silicon, Docker Desktop)

### Results: Default Agent Config (1-minute refresh interval)

```
=== PROVIDER READY TIME (ProviderReady event = actual RC delivery) ===
100 instances, 0 crashes, 0 restarts, 0 ProviderError events

min:    5,492ms   (5.5s)
p50:   75,726ms  (75.7s)
p75:   79,997ms  (80.0s)
p90:   80,005ms  (80.0s)
p95:   80,006ms  (80.0s)
p99:  136,699ms (136.7s)
max:  136,699ms (136.7s)

Distribution:
  <=  10s:  12/100 (12%)    — cache bypass winners
  <=  30s:  25/100 (25%)
  <=  60s:  50/100 (50%)    — waited for first Agent poll cycle
  <= 120s:  99/100 (99%)    — waited for second poll cycle
  > 120s:    1/100 (1%)

Success: 100/100  |  Timeout: 0  |  Error: 0
```

### Results: Agent with 5s refresh interval (NOT production-realistic)

For comparison, with `DD_REMOTE_CONFIGURATION_REFRESH_INTERVAL=5s` (the override in the
main docker-compose.yml, 12x more aggressive than the default):

```
=== PROVIDER READY TIME ===
min:    5,059ms  (5.1s)
p50:   11,268ms  (11.3s)
p90:   17,122ms  (17.1s)
max:   22,396ms  (22.4s)
```

### Results: SetProviderAndWait (DO NOT USE for latency measurement)

The Go tracer's `SetProviderAndWait` has a hard-coded 30-second timeout
([`defaultInitTimeout` in `openfeature/provider.go:36`][spaw]). This creates an artificial cliff
that masks the real RC delivery latency:

[spaw]: https://github.com/DataDog/dd-trace-go/blob/main/openfeature/provider.go#L36

```
=== TOTAL BOOT TIME ===
min:    6,093ms  (6.1s)
p50:   29,527ms  (29.5s)     <-- artificial 30s cliff from Init timeout
p90:   34,654ms  (34.7s)
p99:   49,032ms  (49.0s)

28-32s timeout cliff: 54/100 instances
```

### Conclusions

1. **The RC backend API itself is fast.** 0 `ProviderError` events across all 100 instances.
   Every instance eventually receives its configuration without errors. The delay is entirely
   in the **Agent-side throttling policies** — not the backend, not the network, not RC itself.

2. **With default Agent config, p50 is 76 seconds for 100 tracers.** Only 12% of tracers
   receive flags within 10 seconds (those lucky enough to trigger the Agent's cache bypass).
   The remaining 88% must wait for the Agent's 1-minute background poll cycle. This is
   driven by:
   - Agent cache bypass rate limit: only 5 bypasses per refresh window ([L52][ag4], [L737][ag5])
   - Agent background poll: 1 minute between polls to backend ([L47][ag15]) — the dominant factor
   - Unbuffered bypass channel ([L702][ag10]) serializes bypass attempts; only one in-flight
   - Agent mutex ([L975][ag1]) serializes predicate matching for all concurrent requests
     (though released during bypass wait — [L990][ag2])
   - Tracer fixed 5-second polling with no jitter ([`shared.rs:262,354`][lb2])

3. **The 30s timeout in `SetProviderAndWait` compounds the problem.** With a 1-minute Agent
   poll interval, `SetProviderAndWait` always times out for non-bypass tracers (they'd need
   60+ seconds, but the timeout is 30s). After timeout, `context.DeadlineExceeded` is
   returned — but the provider continues initializing in the background. The flag becomes
   available on the next tracer poll cycle after the Agent's poll delivers fresh data.

4. **A fast lane is critical.** The p50=76s boot time is completely unacceptable for feature
   flags. The RC infrastructure can deliver configs instantly (the backend responds promptly,
   0 errors), but the Agent's conservative policies — designed for products like ASM/APM that
   tolerate minute-scale delays — are the bottleneck. A dedicated fast path for
   latency-sensitive products like `FFE_FLAGS` would collapse this from 76s to sub-second by:
   - Higher cache bypass budgets for fast lane requests
   - Shorter or dedicated poll intervals for FFE_FLAGS
   - Separate concurrency controls that don't compete with ASM/APM traffic

---

## Open Questions

1. **Should the fast lane bypass the Agent entirely?** If the Agent is the primary bottleneck,
   libdatadog could fetch FFE_FLAGS directly from the backend (the `api_key` path currently
   short-circuited at `fetcher.rs:270-273`). This avoids the Agent's mutex and bypass limits
   entirely, but requires backend auth support and has security implications.

2. **Should the fast lane use a different Agent endpoint?** A dedicated `/v0.7/config/priority`
   endpoint could let the Agent handle these requests on a completely separate code path,
   avoiding mutex contention with standard requests. More invasive but cleaner separation.

3. **File storage sharing.** In libdatadog, both the fast lane and standard fetchers for the
   same target will write to the same `RefcountingStorage`. The current locking in
   `ConfigFetcherState` should handle this, but needs verification under concurrent access.

4. **Capability deduplication.** If `FFE_FLAGS` is split out, the standard fetcher must not
   advertise `FfeFlagConfigurationRules` capability. The capability partitioning needs to
   mirror the product partitioning.

5. **Agent bypass channel design.** The current `refreshBypassCh` is an unbuffered channel,
   meaning only one bypass can be in-flight at a time. Should fast lane use a buffered channel
   or a different signaling mechanism to allow concurrent bypass requests?

6. **Backend awareness.** Should the backend know about the fast lane concept? If so, it could
   prioritize responses for fast lane products or provide a dedicated lightweight endpoint.
   This would be a Phase 5 consideration.
