# Fork-safety contract for `libdd-http-client`

> **Status:** M4 deliverable (Task 6). Per-backend post-fork contract,
> validated by `tests/fork_safety_hyper.rs` and
> `tests/fork_safety_reqwest.rs`.

This document captures the post-fork contract that callers of
`libdd_http_client::HttpClient` (and, by composition,
`libdd_agent_client::AgentClient`) must follow when their host process
forks. It is normative for FFI / pyo3 callers (CPython multiprocessing,
PHP-FPM, Ruby's `Process.fork`, …) and for any in-process Rust code that
spawns child processes via `fork(2)`.

## TL;DR

| Backend  | Action required in child after fork |
| -------- | ----------------------------------- |
| `hyper`  | Reinitialise `SharedRuntime` via `after_fork_child`. The `HttpClient` itself can be reused — its connection pool is empty post-fork because hyper's idle sockets were owned by the parent's tokio runtime, which is gone. |
| `reqwest`| Reinitialise `SharedRuntime` via `after_fork_child` **and** rebuild the `HttpClient`. Reusing the inherited client is not supported; see [§ Reqwest backend](#reqwest-backend) for the failure shape. |

The `SharedRuntime` hooks live at
[`libdd-shared-runtime/src/shared_runtime/mod.rs`][sr-mod]:
[`before_fork`][sr-before] (line 246), [`after_fork_parent`][sr-parent]
(line 281), [`after_fork_child`][sr-child] (line 311).

## Why `HttpClient` is not a `Worker` / `PausableWorker`

`HttpClient` is **stateless aside from its connection pool**. It owns:

- A `reqwest::Client` (reqwest backend) or a hyper `GenericHttpClient<Connector>`
  (hyper backend). Both wrap a connection pool that only holds idle
  sockets — no application-owned background tasks.
- Configuration (timeout, retry, treat-http-errors-as-errors, …).

It does **not** own:

- A tokio runtime — that lives on `SharedRuntime`.
- Periodic background tasks (telemetry tick, batch flush, …) — those are
  the domain of `Worker` implementations.
- Any state that needs explicit `pause`/`resume` semantics. The
  connection pool's idle sockets simply die in the child, which is fine —
  reqwest / hyper-util both reconnect on the next request.

Because of this, registering `HttpClient` as a `PausableWorker` would be
busy-work: there's nothing meaningful to pause, no in-flight task to
park. The right model is "the runtime is a `PausableWorker` (it owns the
worker pool); the client is a passive consumer".

This is the same reason `libdd-data-pipeline`'s exporter does not register
its `HttpClient` as a worker either — only the trace buffer registers,
because *it* owns the timer / flush task.

## Hyper backend

A `GenericHttpClient<Connector>` owns:

- A hyper-util connection pool with idle entries.
- A `Connector` (HTTP, HTTPS-rustls, UDS, named pipes). The TLS variant
  holds rustls state but no background tasks.
- A tokio executor handle (`TokioExecutor`).
- For UDS / named pipes: only `AsyncFd`-style streams per request — no
  background tasks.

### Post-fork contract (hyper)

1. **Parent**: `runtime.before_fork()` before `fork(2)`.
2. **Parent (after fork)**: `runtime.after_fork_parent()`.
3. **Child (after fork)**: `runtime.after_fork_child()`. After this, the
   `HttpClient` built in the parent is safe to reuse from the child:
   - The connection pool is effectively empty (idle entries pointed at
     the parent's tokio runtime, which is dead).
   - The `Connector` is a plain value; the resolver, when used, is
     `HttpConnector` from hyper-util which uses the per-thread
     `getaddrinfo` blocking pool driven via the *current* runtime — so
     it picks up the child's reinitialised SharedRuntime correctly.

`tests/fork_safety_hyper.rs::test_hyper_backend_fork_then_send_in_child`
exercises this path end-to-end against an `httpmock` server.

### What can go wrong (hyper)

If the child issues `client.send_blocking(...)` *without* calling
`runtime.after_fork_child()`, `block_on` falls back to a temporary
single-threaded current-thread runtime (per `SharedRuntime::block_on`'s
documented behaviour at [`mod.rs:342`][sr-block-on]). The request still
works, but a fresh runtime is built per call — fine for one-off requests,
wasteful in a loop. Call `after_fork_child` before the first send.

## Reqwest backend

A `reqwest::Client` (with `hickory-dns` enabled — the default in our
backend feature set, which pulls `reqwest?/hickory-dns`) owns:

- An internal HTTP/1 + HTTP/2 connection pool (idle sockets per host).
- A hickory-dns resolver, lazily initialised via a `OnceCell<TokioResolver>`
  on the first DNS lookup. The resolver uses
  `hickory_proto::runtime::TokioRuntimeProvider`, which spawns
  per-lookup background tasks via `tokio::spawn`.
- A tokio runtime handle captured at first-use (the runtime that drove
  the call into reqwest).
- TLS state (`rustls`) — fork-safe at the data-structure level.

### Post-fork contract (reqwest)

1. **Parent**: `runtime.before_fork()` before `fork(2)`.
2. **Parent (after fork)**: `runtime.after_fork_parent()`.
3. **Child (after fork)**: `runtime.after_fork_child()` **and**
   `HttpClient::new(...)` (or the builder) — i.e. **rebuild the client**.

The rebuild is the supported / documented path. Anything else is
best-effort: see [§ Failure shape when the child reuses the parent's
client](#failure-shape-when-the-child-reuses-the-parents-client).

`tests/fork_safety_reqwest.rs::test_fork_with_client_rebuild` exercises
this path end-to-end against an `httpmock` server.

### Failure shape when the child reuses the parent's client

Tested by
`tests/fork_safety_reqwest.rs::test_fork_without_rebuild`. The test
warms reqwest's hickory `OnceCell` and stashes an idle pool socket in
the parent before forking, then in the child calls `send_blocking`
without rebuilding. The asserted contract is **bounded resolution**:

- The child returns within `CHILD_WAIT_TIMEOUT` (15 s — generous, so a
  truly hung child is unambiguous).
- The child's per-request timeout (`CHILD_REQUEST_TIMEOUT` = 3 s) is
  what bounds DNS / connect / read latency, so any error shows up well
  before the wall-clock ceiling.
- The child does **not** panic / segfault / receive a fatal signal.
- If the child returns `Err`, it is a mapped [`HttpClientError`] variant
  (e.g. `TimedOut`, `ConnectionFailed`, `IoError`).

What the test does **not** assert:

- That the inherited client **fails**. Empirically, on reqwest 0.13 +
  hickory 0.25 + Linux x86_64, the child's request often succeeds:
  hickory's `TokioRuntimeProvider` re-spawns per-lookup tasks via
  `Handle::current()`, which in the child is the fresh SharedRuntime,
  and the connection pool's idle socket is closed and reopened on demand.
  Pinning the test to a specific `HttpClientError` variant would be
  flaky.

In other words: today's reqwest is **more fork-tolerant in practice
than its contract guarantees**. The contract still says "rebuild the
client" because:

1. A future reqwest / hickory release could change this. The supported
   path needs to be the one that is documented, not the one that
   happens to work today.
2. PHP-FPM and similar fork-heavy SAPIs that fork pre-warmed clients
   under load have historically tripped over reqwest's resolver state;
   we don't want to relitigate that bug per language client.
3. The system-resolver fallback (`Endpoint::with_system_resolver(true)`
   in `libdd-common`) is **not fork-safe** — glibc's resolver holds
   locks across fork. The hickory default protects callers from this,
   but only if the client is built fresh in the child.

### Don't enable `with_system_resolver` if you fork

`libdd-common::Endpoint` exposes `use_system_resolver` as an escape hatch
for environments where hickory can't read `/etc/resolv.conf` correctly.
That path uses glibc's `getaddrinfo`, which is **not fork-safe**: glibc
holds internal locks across fork that a forking child can deadlock on.
If your application forks, leave the default (`hickory_dns(true)`) on.

## SharedRuntime integration

`HttpClient::send_blocking` (Task 5a) wraps `runtime.block_on(self.send(...))`.
The fork-safety story therefore reduces to "the SharedRuntime's fork
hooks are correct". They are; see the test pattern in
`libdd-data-pipeline/src/trace_buffer/mod.rs` (search for `before_fork` /
`after_fork_child`).

`SharedRuntime::block_on` has a documented fallback: if the runtime is
`None` (e.g. between `before_fork` and `after_fork_*`), it builds a
temporary current-thread runtime. That fallback is exercised by
`tests/blocking_test.rs::test_send_blocking_works_with_uninitialised_runtime_fallback`.
Do not rely on it as a substitute for `after_fork_child` — it allocates
a fresh runtime per call.

## Linux only

The fork tests are gated on `cfg(target_os = "linux")`:

- **macOS**: forbids `fork()` after Mach ports are touched (which a
  tokio runtime does indirectly), so the child is unstable. The
  practical guidance for macOS callers is "use `posix_spawn` / `execve`,
  not `fork`".
- **Windows**: no `fork(2)`. `CreateProcess` does not duplicate state.

The contract documented here is also Linux-specific in spirit: the
`SharedRuntime::before_fork` / `after_fork_child` hooks are
`#[cfg(not(target_arch = "wasm32"))]` but only meaningful on Unix.

## AgentClient

`libdd-agent-client::AgentClient` composes an `HttpClient` with header
injection and a typed send surface. It owns no fork-relevant state of
its own — the whole story is inherited from `HttpClient`. The same
contract applies: `runtime.after_fork_child()` plus, if the underlying
backend is reqwest, rebuild the `AgentClient`.

A dedicated `AgentClient` fork test was deliberately not added in M4 —
the test would duplicate the `HttpClient` test and add nothing
meaningful to the coverage. M5 will revisit this if any agent-specific
state grows.

## Future work

- **Auto-rebuild on fork.** The reqwest backend could detect a
  PID-change and rebuild its client transparently. That would mean
  stashing `getpid()` at construction and checking it on every send,
  which is per-call overhead for a problem most callers don't have.
  Deferred unless a real consumer asks for it.
- **Pyo3 / FFI ergonomics.** Once the FFI surface lands (M7), the
  `ddog_http_client_handle_t` builder will need to expose a
  "post-fork" entry point that the language clients can call from
  their own fork hooks (`os.register_at_fork(after_in_child=...)` in
  CPython). That entry point is just a thin wrapper over
  `runtime.after_fork_child()` plus a client rebuild on the reqwest
  path. To be designed in M7, not M4.

## Review

This document is the M4 close criterion per
[`PLAN.md`][plan]. Reviewed by the SharedRuntime owner (record the
sign-off in the PR description, not here).

[sr-mod]: ../../libdd-shared-runtime/src/shared_runtime/mod.rs
[sr-before]: ../../libdd-shared-runtime/src/shared_runtime/mod.rs#L246
[sr-parent]: ../../libdd-shared-runtime/src/shared_runtime/mod.rs#L281
[sr-child]: ../../libdd-shared-runtime/src/shared_runtime/mod.rs#L311
[sr-block-on]: ../../libdd-shared-runtime/src/shared_runtime/mod.rs#L342
[plan]: https://github.com/DataDog/libdatadog/blob/main/libdd-http-client/docs/m3-consolidation.md
