# Sidecar telemetry lifecycle isolation

Status: approved for implementation

Date: 2026-07-24

## Context

The sidecar currently shares full application telemetry workers by service and
environment. The pull request separates logs, metrics, and stats from that
application lifecycle, but its first version creates new correctness and scaling
problems in dd-trace-php.

First, an application worker starts as soon as any action reaches the sidecar.
In PHP, the Composer hook can send a file action while `vendor/autoload.php` is
executing. The tracer does not send its configuration batch until request
shutdown. Starting the worker from the Composer action still produces an
`app-started` payload without configuration.

Second, direct AppSec telemetry workers are scoped to the full session and
runtime ID. The C++ helper registers metric definitions on a service shared by
many PHP clients, so one registration must be usable by all runtimes in the same
session, service, and environment. Runtime-scoped definitions cause sibling PHP
workers to drop their metric points as unregistered.

The runtime-scoped worker also needs explicit cleanup. PHP creates a new runtime
ID after a fork, and FPM workers may be recycled often. Waiting 30 minutes for
cache expiry retains workers that can no longer submit data.

## Goals

- Include the initial PHP configuration in `app-started`, even when Composer or
  another application action arrives first.
- Preserve per-runtime attribution for direct logs and metrics.
- Allow metric definitions registered by one runtime to be used by sibling
  runtimes in the same session, service, and environment.
- Remove runtime-owned workers when the runtime ends and session-owned state
  when the session ends.
- Remove registration-map work from the metric point path.
- Recover from a temporary failure to create the application telemetry shared
  memory writer.
- Preserve the meaning of existing sidecar stats fields.

## Non-goals

- Changing the telemetry protocol or backend payload schemas.
- Combining all FPM logs and metrics under one runtime ID.
- Changing dd-trace-php helper registration behavior in this pull request.
- Reworking the telemetry worker mailbox or HTTP client implementation.
- Changing how Composer files are parsed and cached.

## Application lifecycle

The application telemetry cache will represent two states:

- `Pending`: action batches received before the initial configuration.
- `Active`: a full telemetry worker and its existing shared-memory state.

An absent application entry is inserted as `Pending`. Non-configuration actions
are appended without starting a worker. When a batch containing at least one
`AddConfig` action arrives, the cache atomically takes all pending actions and
creates the worker.

Before `Start` is queued, the worker builder receives every direct
configuration, dependency, and integration from the pending and current
batches. Those seeded actions are still reflected in the sidecar's deduplication
state, but they are not queued to the worker a second time. Composer file actions
are processed after startup because resolving them requires asynchronous file
I/O. Their dependencies may use the normal dependency-change payload.

If a pending lifecycle receives `Stop` before any configuration, no later batch
can complete that lifecycle. The cache creates a worker from the accumulated
startup data, queues `Start`, then queues `Stop`. This preserves the telemetry
protocol's lifecycle ordering without waiting for data that will never arrive.
The normal cache expiry task removes abandoned pending entries that receive
neither configuration nor `Stop`.

Once a worker is active, later configuration actions retain their current
configuration-change behavior. A stopped worker cannot be reused; the next
lifecycle starts in `Pending` again.

## Direct logs and metrics

Direct log and metric workers remain scoped by:

```
(InstanceId, service, environment)
```

This keeps the runtime ID in the telemetry envelope accurate and keeps session
endpoint configuration isolated.

Metric definitions use a different scope:

```
(session ID, service, environment)
```

Each scope owns a bounded map from metric name to `MetricContext`. A registration
from any runtime updates that scope. When a runtime worker is created, it copies
the definitions from its session scope before processing the triggering batch.
An already active runtime worker also receives the new definition.

This matches both helper implementations:

- The C++ helper registers on a service shared by multiple PHP clients.
- The Rust helper may register per client, which remains valid because repeated
  definitions update the same session scope.

Definitions are capped at the telemetry worker's context limit for each scope.
When the scope is full, a new definition is rejected with a warning. Existing
definitions are not evicted, because eviction could make a later replacement
worker silently lose a metric that an active helper still considers registered.

Metric points no longer update registration recency. The point path looks up the
context in its runtime worker and queues the point. It does not clone an
`InstanceId`, service, environment, or metric name for a second global lookup.

## Cleanup

`MetricsLogsClientSet` will expose two cleanup operations:

- `remove_runtime(&InstanceId)` removes all workers owned by that runtime.
- `remove_session(&str)` removes all workers and metric-definition scopes owned
  by that session.

The sidecar calls `remove_runtime` before or alongside `SessionInfo` runtime
shutdown. Connection cleanup uses the same path. Session shutdown calls
`remove_session` even when the runtime map is already empty.

Dropping the final worker handle performs the worker's existing final flush and
shutdown behavior. The 30-minute cache expiry remains a fallback for clients
that disappear without a shutdown message.

The cleanup methods collect matching cache entries while holding the map lock,
remove them, release the lock, and then drop the workers. Worker shutdown must
not run while a cache or session mutex is held.

## Shared-memory recovery

Application clients distinguish "shared memory not required" from "shared
memory creation failed." Metrics/log workers never retry because they do not use
the application cache.

When application writer creation fails, the client records a retry deadline.
The next application action after that deadline retries creation. Retries are
limited to once per minute. A successful retry immediately publishes the
current deduplication state, including `config_sent`, integrations, Composer
paths, and endpoint state.

This allows PHP readers to stop resending full configuration batches after a
temporary operating-system resource failure. A persistent failure produces at
most one warning and creation attempt per retry interval.

## Sidecar stats

`active_telemetry_clients` keeps its previous meaning: active application
telemetry workers. It does not begin counting runtime metrics/log workers or
stats-exporter workers.

This pull request does not add an auxiliary-client count. Aggregate worker error
and queue statistics may still inspect all worker types because those fields
describe sidecar resource use rather than application count.

## Concurrency

Application state transitions happen under the application cache mutex. Only
one caller can change an entry from `Pending` to `Active`. The worker is fully
initialized and its initial data is seeded before the active handle becomes
visible.

Direct telemetry actions are consumed by one receiver task. Registration scopes
and runtime client caches still use mutexes because stats collection, cleanup,
and explicit flushes can access them from other tasks. No method holds the
definition mutex while locking a runtime client.

The lock order is:

1. cache or definition map;
2. release the map lock;
3. individual client lock;
4. asynchronous worker operation after all standard mutexes are released.

## Error handling

- Failure to seed or queue `Start` removes the new active entry and leaves a
  warning. A later configuration batch may create another lifecycle.
- A metric point without a definition remains a warning and is dropped.
- A full definition scope rejects only the new definition.
- Runtime and session cleanup are idempotent.
- Shared-memory retry failures leave the in-process deduplication state intact.

## Tests

The implementation will add these regression tests:

1. Send a Composer file action, then send configuration in a second batch for
   the same application. Assert that exactly one `app-started` payload contains
   the configuration.
2. Send direct dependencies and integrations before configuration. Assert that
   they are included in the initial payload and are not sent again as changes.
3. Register a metric through runtime A, then submit points through runtimes A and
   B in the same session, service, and environment. Assert that both points are
   delivered by separate runtime workers.
4. Use the same service and environment in two sessions with different
   endpoints. Assert that definitions and payloads do not cross sessions.
5. Shut down one runtime and assert that only its workers are removed. Shut down
   the session and assert that its remaining workers and definitions are gone.
6. Fill one definition scope to its limit and assert that a new definition is
   rejected without removing an existing definition.
7. Inject an application shared-memory creation failure, advance the retry
   clock, and assert that the writer is recreated and publishes current state.
8. Assert that `active_telemetry_clients` remains application-only.

Focused tests will be followed by formatting, clippy for the affected crates,
the complete sidecar test suite, and repository-prescribed checks.

## Alternatives

One worker per session, service, and environment would use fewer tasks and HTTP
clients. It would also put data from many PHP processes under whichever runtime
created the worker, so this design rejects that option.

Changing only dd-trace-php to register once per runtime would fix the known C++
helper path. It would require a coordinated tracer release, would not help other
FFI callers, and would leave runtime cleanup and point-path costs unchanged.

Changing the telemetry protocol to carry a runtime ID per log or metric action
would permit shared workers with accurate attribution. That is a larger
cross-library change and is outside this repair.
