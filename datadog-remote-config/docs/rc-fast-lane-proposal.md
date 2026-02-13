# Remote Config Fast Lane: Priority Fetching for Latency-Sensitive Products

## Problem Statement

When a large fleet of tracers boots simultaneously (e.g. a deployment rollout, auto-scaling
event, or cluster restart), the Remote Configuration (RC) subsystem experiences a thundering
herd problem that causes unacceptable delays — **p50=11s, p90=17s, max=22s** — for products
like `FFE_FLAGS` (Feature Flag Evaluation) that are critical to application startup.

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
                                       │             │    (holds s.mu.Lock!)
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

| Issue | Location | Impact |
|-------|----------|--------|
| All products bundled into one request | `fetcher.rs:302-335` | FFE_FLAGS blocked by slow ASM_DATA payloads |
| Fixed 5s polling, no jitter | `shared.rs:262,354` | All tracers retry in lockstep |
| `agent_refresh_interval` parsed but **never applied** | `targets.rs:47`, unused in `shared.rs` | Server cannot tell clients to slow down |
| No backoff on errors | `shared.rs:346-354` | Failed clients hammer the Agent immediately |
| No HTTP 429 handling | `fetcher.rs:360-368` | Rate limit responses treated as generic errors |
| 100-permit FIFO semaphore, no prioritization | `multitarget.rs:54,551-553` | Fast lane products queue behind everything |
| 3s HTTP timeout | `libdd-common/src/lib.rs:241` | Requests timeout under load, retry with no escalation |

#### Layer 2: DD Agent (`datadog-agent` repo) — RC Service

| Issue | Location | Impact |
|-------|----------|--------|
| `s.mu.Lock()` held across entire `ClientGetConfigs` | `service.go:975-976` | All concurrent tracer requests serialize on a single mutex |
| Cache bypass rate-limited to 5/interval | `service.go:737,52` | 6th+ new tracer in a window gets stale/empty data |
| Bypass blocks up to 2s (`newClientBlockTTL`) | `service.go:1001-1014` | New tracers at boot wait up to 2s for backend round-trip |
| `refreshBypassCh` is unbuffered channel | `service.go:702,1000` | Only one bypass can be in-flight; others timeout |
| Backend poll default 1 minute | `service.go:47` | Stale data between polls; bypass is only escape hatch |
| No product-level prioritization | `service.go:974-1159` | FFE_FLAGS and ASM_DATA use identical code path |

#### Layer 3: Backend

| Issue | Impact |
|-------|--------|
| Rate limits Agent via `opaque_backend_state` | Agent slows down, tracers don't (libdatadog ignores `agent_refresh_interval`) |
| Single `/configurations` endpoint for all products | No way to prioritize latency-sensitive products |

### Compounding Scenario: 100 Tracers Boot Simultaneously

1. **T=0s**: 100 tracers call `/v0.7/config`. All are new clients (never seen before).
2. **Agent**: First 5 trigger cache bypass (`refreshBypassLimiter`). Remaining 95 are rate-limited
   and get stale/empty data. The mutex (`s.mu.Lock()`) serializes all 100 requests.
3. **T=2s**: Bypass timeout fires. The 5 lucky tracers got fresh data (if backend responded in
   time). The other 95 got nothing.
4. **T=5s**: All 100 tracers retry simultaneously (no jitter). But now they're "active" (TTL not
   expired), so no bypass — they get whatever the Agent has cached. If the Agent's 1-minute poll
   hasn't run yet, this is still stale.
5. **T=10-30s**: Cycle repeats. Tracers keep hitting the Agent every 5s, in lockstep. Eventually
   the Agent's background poll completes and fresh data becomes available.

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

Currently `ClientGetConfigs` holds `s.mu.Lock()` for the entire duration (`service.go:975`),
including the cache bypass wait (up to 2s). This serializes all tracer requests.

For fast lane requests that can be served from cached TUF state (no bypass needed), the mutex
hold time is already short. But when bypass IS needed, the mutex is released during the wait
(`service.go:990`). The key improvement is ensuring fast lane requests don't contend with
standard requests during the bypass:

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

| Tracer | RC Implementation | Uses libdatadog RC? | Changes needed in |
|--------|------------------|---------------------|-------------------|
| **dd-trace-php** | libdatadog sidecar | **Yes** | This repo (libdatadog) |
| **dd-trace-go** | Native Go (`internal/remoteconfig/`) | No | `dd-trace-go` |
| **dd-trace-java** | Native Java/OkHttp (`remote-config/`) | No | `dd-trace-java` |
| **dd-trace-py** | Native Python (`ddtrace/internal/remoteconfig/`) | No | `dd-trace-py` |
| **dd-trace-rb** | Native Ruby/Net::HTTP (`lib/datadog/core/remote/`) | No | `dd-trace-rb` |
| **dd-trace-dotnet** | Native C# (`RemoteConfigurationManagement/`) | No | `dd-trace-dotnet` |
| **dd-trace-js** | Native JS (`packages/dd-trace/src/remote_config/`) | No | `dd-trace-js` |

Ruby depends on the libdatadog gem, but only for profiling/crashtracking/telemetry — its RC
client is pure Ruby.

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
- 1 Datadog Agent (default config, `refresh_interval=5s`)
- All instances boot against the same Agent and request `FFE_FLAGS` via RC
- Each instance measures: tracer start, provider ready (ProviderReady event), first flag
  evaluation, total boot time
- Docker Compose on a single host (Apple Silicon, Docker Desktop)

### Results: SetProvider (non-blocking) with event handlers

```
=== CONTAINER HEALTH ===
Running: 100  |  Exited: 0  |  Restarting: 0  |  Crashes: 0

=== PROVIDER READY TIME (ProviderReady event, i.e. actual RC delivery) ===
min:    5,059ms  (5.1s)
p50:   11,268ms  (11.3s)
p90:   17,122ms  (17.1s)
max:   22,396ms  (22.4s)

=== TOTAL BOOT TIME (tracer start + provider ready + first eval) ===
min:    5,075ms  (5.1s)
p50:   11,782ms  (11.8s)
p75:   14,355ms  (14.4s)
p90:   17,184ms  (17.2s)
p95:   18,705ms  (18.7s)
p99:   24,298ms  (24.3s)
max:   24,298ms  (24.3s)

Success: 100/100  |  Timeout: 0  |  Error: 0
ProviderError events: 0
```

### Results: SetProviderAndWait (for comparison — DO NOT USE for latency measurement)

```
=== TOTAL BOOT TIME ===
min:    6,093ms  (6.1s)
p50:   29,527ms  (29.5s)     <-- artificial 30s cliff from Init timeout
p90:   34,654ms  (34.7s)
p99:   49,032ms  (49.0s)
max:   49,032ms  (49.0s)

Under 10s: 20  |  28-32s (timeout cliff): 54  |  Over 32s: 15
```

### Conclusions

1. **The RC backend API itself is fast.** There is no evidence of backend-side rate limiting
   or slow responses. The `ProviderReady` event fires without errors for all 100 instances —
   0 `ProviderError` events. The delay is entirely in the **client-side and Agent-side policies**
   that throttle how quickly new tracers can receive their first configuration.

2. **The thundering herd latency is real: p50=11s, p90=17s for 100 tracers.** Even without
   the 30s timeout artifact, the RC delivery takes 5-22 seconds. This is driven by:
   - Agent cache bypass rate limit (5 bypasses per refresh interval)
   - Agent mutex serialization (`s.mu.Lock()` across `ClientGetConfigs`)
   - Agent background poll interval (default 1 minute between polls to backend)
   - Tracer fixed 5-second polling with no jitter

3. **The 30s timeout in `dd-trace-go` `SetProviderAndWait` masks the real problem.** It makes
   latency look worse than it is (p50=29s vs p50=11s), but it also hides the fact that every
   instance *eventually* gets its config — just not fast enough. Applications using
   `SetProviderAndWait` see either "fast" (under 30s) or "timeout + delayed", creating a
   bimodal distribution that makes debugging harder.

4. **A fast lane is needed.** Even the true p50=11s is unacceptable for feature flags at
   application startup. The RC infrastructure can deliver configs quickly (the backend responds
   promptly), but the Agent's conservative throttling policies (designed for ASM/APM products
   that can tolerate delays) are the bottleneck. A dedicated fast path for latency-sensitive
   products like `FFE_FLAGS` — with higher bypass budgets, separate concurrency controls, and
   shorter polling intervals — would bring this down to sub-second.

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
