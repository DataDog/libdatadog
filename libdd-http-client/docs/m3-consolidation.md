# M3 Consolidation: Shared HTTP Foundation in `libdd-common`

**Status:** Discovery spike — Task 1 of the libdd-http-client M3-M7 plan.
**Owner of this doc:** the implementer of Task 2.
**Branch base:** `yannham/apmsp-2722-agent-client-layer` (M2).
**Source plan:** `/home/bits/.claude/plans/libdd-http-client-m3-m7/PLAN.md`.

---

## Purpose

This doc settles the architectural questions for M3 *before* any production
code is written:

1. Which crates currently consume `libdd-common`'s hyper-based HTTP stack, and
   what happens to each in M3.
2. Where the shared foundation lives, what it exposes, and how
   `libdd-common`'s public API stays compatible (no breaking changes).
3. How each backend (`reqwest_backend`, `hyper_backend`) reuses that foundation.
4. How binary-size impact is measured.
5. Per-backend fork-safety implications, to set up Task 6.

Two adjacent decisions are *already resolved* upstream and are not re-litigated
here (see `PLAN.md` "Open Questions"):

- **Backend scope:** consolidate **both** reqwest and hyper backends onto the
  shared foundation. Reqwest-specific bits (hickory-dns, reqwest's own
  multipart encoder) stay reqwest-specific.
- **`HttpClientTrait` integration:** Task 7 (M5) separately swaps
  `DefaultHttpClient`'s internals to libdd-http-client. **M3 does not touch
  `HttpClientTrait`.** It only builds the foundation that
  `libdd-http-client` (and therefore `DefaultHttpClient` via Task 7) sits on.

---

## 1. Authoritative consumer enumeration

### 1.1 The grep

The Task 1 spec's authoritative grep:

```
rg "libdd_common::(http_common|HttpClient|new_default_client|new_client_periodic)" --type rust
```

Run from repo root on this branch (`task-01-m3-discovery-spike`,
6df734864 — "feat: add Https transport variant to AgentTransport"):

```
libdd-data-pipeline/src/trace_exporter/error.rs:8         use libdd_common::http_common;
libdd-capabilities-impl/src/http.rs:10                    use libdd_common::http_common::{new_default_client, Body, GenericHttpClient};
datadog-live-debugger/src/sender.rs:10                    use libdd_common::http_common;
libdd-crashtracker/src/crash_info/errors_intake.rs:593    let client = libdd_common::http_common::new_client_periodic();
libdd-telemetry/src/worker/http_client.rs:136             inner: libdd_common::HttpClient,
libdd-http-client/src/backend/hyper_backend.rs:10         use libdd_common::http_common::{self, Body};
libdd-trace-utils/src/test_utils/datadog_test_agent.rs:9  use libdd_common::http_common;
libdd-trace-utils/src/stats_utils.rs:12                   use libdd_common::http_common;
libdd-trace-utils/src/stats_utils.rs:99                   use libdd_common::http_common;
```

Because some files `use libdd_common::http_common;` at the top and reference
items further down (e.g. `http_common::Body::from(...)` or
`http_common::new_default_client()`), the grep above misses a few
*call sites* even though it captures every consumer crate that reaches into
`http_common`. A broader scan
(`rg "http_common::|libdd_common::HttpClient" --files-with-matches`) was used
to find the actual usage patterns; results are folded into the table below.

### 1.2 Classification

Legend:

- **migrate-now** — site moves to the new shared module path in M3 (Task 4
  bulk migration).
- **migrate-now (Task 11)** — same, but isolated on its own branch because the
  test-agent ripples into many crates' test surfaces.
- **keep-via-shim** — site is kept untouched in M3; `libdd-common` re-exports
  the existing names so it continues to compile. Task 7 will carry it onto
  `libdd-http-client` later, transparently.
- **out-of-scope** — site does not depend on the HTTP-client surface (e.g.
  uses `http_common::Body` only, or only error types).
- **internal** — site is inside `libdd-common` or `libdd-http-client` itself
  and changes as part of Task 2 / Task 3, not Task 4.

| # | Crate / file | Symbols touched | Classification | Routed to |
|---|---|---|---|---|
| 1 | `libdd-common/src/http_common.rs` | the module itself | internal | Task 2 (extract) |
| 2 | `libdd-common/src/connector/mod.rs` | `http_common::new_default_client` (in tests) | internal | Task 2 |
| 3 | `libdd-common/src/lib.rs` | `pub type HttpClient = http_common::GenericHttpClient<connector::Connector>;` | internal — keep as a re-export shim | Task 2 |
| 4 | `libdd-http-client/src/backend/hyper_backend.rs` | `http_common::{self, Body}`, `http_common::GenericHttpClient`, `http_common::client_builder()`, `http_common::ClientError`, `http_common::ErrorKind` | internal — backend is migrated by Task 3 | Task 3 |
| 5 | `libdd-telemetry/src/worker/http_client.rs` | `libdd_common::HttpClient` (type alias), `http_common::new_client_periodic()`, `http_common::HttpRequest`, `http_common::HttpResponse`, `http_common::Error`, `http_common::Body`, `http_common::into_response`, `http_common::empty_response` | **migrate-now** | Task 4 |
| 6 | `libdd-telemetry/src/worker/mod.rs` | `http_common::HttpRequest`, `http_common::HttpResponse`, `http_common::Body`, `http_common::Error` | **migrate-now** (paired with #5; same crate) | Task 4 |
| 7 | `libdd-crashtracker/src/crash_info/errors_intake.rs` | `libdd_common::http_common::new_client_periodic()` | **migrate-now** | Task 4 |
| 8 | `datadog-live-debugger/src/sender.rs` | `http_common::Body::channel`, `http_common::Sender`, `http_common::ResponseFuture`, `http_common::HttpResponse`, `http_common::new_default_client()`, `http_common::into_response`, `http_common::Error` | **migrate-now** | Task 4 |
| 9 | `datadog-remote-config/src/fetch/fetcher.rs` | `http_common::Body::from`, `http_common::new_default_client()`, `http_common::empty_response` | **migrate-now** | Task 4 |
| 10 | `datadog-remote-config/src/fetch/test_server.rs` | `http_common::Body` (test scaffolding) | **migrate-now** (with #9) | Task 4 |
| 11 | `libdd-tracer-flare/src/zip.rs` | `http_common::Body::from_bytes`, `http_common::new_default_client()`, `http_common::into_response` | **migrate-now** | Task 4 |
| 12 | `libdd-trace-utils/src/stats_utils.rs` | `http_common::Body` (only — under `cfg(feature = "mini_agent")` and in tests) | **migrate-now** (cosmetic — just the Body alias) | Task 4 |
| 13 | `libdd-trace-utils/src/trace_utils.rs` | `http_common::Body` in `#[cfg(test)]` only | **migrate-now** (test-only, low risk) | Task 4 |
| 14 | `libdd-trace-utils/tests/test_send_data.rs` | `http_common::new_default_client()` (integration test) | **migrate-now** (test-only) | Task 4 |
| 15 | `libdd-trace-utils/src/test_utils/datadog_test_agent.rs` | `http_common::Body`, `http_common::new_default_client()` | **migrate-now (Task 11)** — owned by Task 11 on its own branch off Task 2; **not in Task 4's scope** | Task 11 |
| 16 | `libdd-data-pipeline/src/trace_exporter/error.rs` | `http_common::Error`, `http_common::ClientError`, `http_common::ErrorKind` (error-type plumbing only — no client construction; data-pipeline does HTTP via `HttpClientTrait`) | **keep-via-shim** | Task 7 (incidentally; not M3) |
| 17 | `libdd-capabilities-impl/src/http.rs` | `http_common::{new_default_client, Body, GenericHttpClient}`, `connector::Connector` | **keep-via-shim** | **Task 7** swaps the implementation; M3 does not touch it. Re-exports under the old paths must stay green. |

**Consumer count for Task 4's bulk migration: 9 files across 5 crates**
(`libdd-telemetry`: 2, `libdd-crashtracker`: 1, `datadog-live-debugger`: 1,
`datadog-remote-config`: 2, `libdd-tracer-flare`: 1, `libdd-trace-utils`:
2 non-test-agent — note that `datadog_test_agent.rs` and the integration test
`tests/test_send_data.rs` are routed to Task 11 since the latter consumes the
test agent).

> **Note on closure of the list.** PLAN.md's "Open Questions" item 4 already
> stated the six-crate starting set. The grep confirms the same six consumer
> crates (telemetry, live-debugger, crashtracker, remote-config, tracer-flare,
> trace-utils) plus the existing internal users. No new external consumer was
> discovered. `libdd-data-pipeline/src/trace_exporter/error.rs` is in the grep
> only because it imports `http_common` for its `Error`/`ClientError`/`ErrorKind`
> conversion — it does **not** construct an HTTP client itself.

### 1.3 What is *not* migrating in M3

- **`libdd-data-pipeline`** as a whole. Its HTTP path goes through
  `HttpClientTrait` (`HttpClient: HttpClientTrait`). Task 7 swaps
  `DefaultHttpClient`'s internals, which in turn carries data-pipeline onto
  `libdd-http-client` for free.
- **`libdd-capabilities-impl::DefaultHttpClient`**: same — owned by Task 7.
- **The `libdd-common::http_common::Error` / `ClientError` / `ErrorKind` types**.
  These are referenced by `libdd-data-pipeline/src/trace_exporter/error.rs` for
  error-type conversion. M3 keeps them addressable at their current paths via
  shim re-exports; whether they live in the new shared module or stay in
  `http_common` is a Task 2 implementation detail (recommendation: keep them
  in `http_common` and re-export them from the new module so the
  data-pipeline error conversion is unaffected).

---

## 2. Shared module shape

### 2.1 Where it lives

The shared foundation is a **new module inside `libdd-common`**, at:

```
libdd-common/src/http/
    mod.rs        // re-exports + public API of the shared foundation
    body.rs       // the Body enum + Sender (moved from http_common)
    client.rs     // GenericHttpClient<C> alias, client_builder(),
                  // new_default_client(), new_client_periodic(),
                  // into_response(), empty_response(), mock_response()
    error.rs      // Error, ClientError, ErrorKind, From<hyper::Error>,
                  // From<HttpRequestError> (moved from http_common)
    fips.rs       // (cfg(feature = "fips")) install_fips_provider()
                  // unifies the FIPS-init logic that lives in
                  // libdd-http-client::init_fips_crypto and
                  // connector/mod.rs's `ensure_crypto_provider_initialized`
```

The choice of `libdd-common/src/http/` (a module *directory*) over a flat
`libdd-common/src/http_native.rs` is to keep room for backend-specific
auxiliary modules (e.g. a future `http/multipart_bridge.rs`).

The existing **`libdd-common/src/multipart.rs`** stays where it is. It already
plays the role of the shared multipart encoder (`MultipartFormData`,
`MultipartPart`) that the hyper backend uses; the reqwest backend continues to
use `reqwest::multipart::Form`. No migration of `multipart.rs` itself in M3.

The existing **`libdd-common/src/connector/`** stays where it is.
The hyper backend already pulls `libdd_common::connector::Connector` directly
(`libdd-http-client/src/backend/hyper_backend.rs:9`), so M3 does not move it.
The `Connector` enum (`Http`, `Https-rustls`, `UDS`, `named-pipes`) is the
same one both backends will continue to use through the foundation.

### 2.2 What the new module exposes

```rust
// libdd-common::http::*

// --- Body and helpers (moved from http_common::native) ---
pub use body::{Body, Sender};

// --- Client construction (moved from http_common::native) ---
pub use client::{
    client_builder,                         // hyper-util Builder factory
    new_default_client,                     // GenericHttpClient<Connector>
    new_client_periodic,                    // pool_max_idle_per_host(0)
    into_response, empty_response, mock_response,
    collect_response_bytes,
    GenericHttpClient,                      // type alias
    HttpRequest, HttpResponse, HttpRequestError, ResponseFuture,
};

// --- Error types (kept compatible with libdd-data-pipeline) ---
pub use error::{ClientError, Error, ErrorKind};

// --- FIPS init (cfg(feature = "fips")) ---
#[cfg(feature = "fips")]
pub fn install_fips_provider() -> Result<(), FipsInstallError>;

// --- Convenience re-exports for backends ---
pub use crate::connector::Connector;        // both backends need it
pub use crate::multipart::{MultipartFormData, MultipartPart};
```

### 2.3 What `libdd-common` keeps as backward-compatibility shims

To honour the **"no breaking changes to libdd-common's public API in M3"**
constraint (PLAN.md "Constraints & Gotchas"), the existing public paths stay
addressable. After M3:

- `libdd-common::http_common` stays as a module that **re-exports** everything
  from the new `libdd-common::http`. The intention is to keep it as a deprecated
  alias (no `#[deprecated]` attribute yet — that lands in a follow-up once
  every consumer is migrated, see Task 4 / Task 11 / Task 7). Concretely:

    ```rust
    // libdd-common/src/lib.rs
    pub mod http;
    pub mod http_common {
        // Backwards-compatibility shim. Prefer `libdd_common::http`.
        pub use crate::http::*;
    }
    ```

- `pub type HttpClient = http_common::GenericHttpClient<connector::Connector>;`
  in `libdd-common/src/lib.rs` (line 117–118) **stays unchanged**.
  `libdd-telemetry/src/worker/http_client.rs:136` uses
  `inner: libdd_common::HttpClient,` and that compiles as-is.

- `pub type HttpResponse = http_common::HttpResponse;` (line 119–120) — same.

- `pub type HttpRequestBuilder = http::request::Builder;` — unchanged.

- `pub trait Connect: ...` — unchanged.

- `Endpoint`, `parse_uri`, `decode_uri_path_in_authority`, the `header` module,
  and all of `connector/`, `multipart`, `tag`, `entity_id`, `error`,
  `unix_utils`, `timeout`, `cstr`, `tag`, etc. — **untouched**. Endpoint and
  its methods (`to_request_builder`, `set_standard_headers`,
  `to_reqwest_client_builder`) stay where they are; the plan's
  out-of-scope rule applies.

### 2.4 Both backends' needs, covered

| Need | Reqwest backend | Hyper backend | Source |
|---|---|---|---|
| Build a TLS-capable client | `reqwest::Client::builder()` (uses its own internal connector) | `client_builder().build(Connector::default())` | `libdd-common::http::client_builder` + `Connector` |
| UDS / Windows named-pipe transport | `builder.unix_socket()` / `builder.windows_named_pipe()` | URL rewrite to `unix:` / `windows:` scheme; routed by `Connector` | reqwest stays internal; hyper uses `libdd_common::connector::{uds, named_pipe}` (already does) |
| Multipart encoding | `reqwest::multipart::Form` (kept reqwest-specific) | `libdd_common::multipart::MultipartFormData` | `libdd-common::multipart` (existing, M1 deliverable) |
| DNS resolver | `hickory-dns` (reqwest feature, kept reqwest-specific) | n/a — `Connector` builds its own resolver via the OS | reqwest stays internal |
| Connection-pool toggle | `builder.pool_max_idle_per_host(0)` (reqwest API) | `client_builder().pool_max_idle_per_host(0)` (hyper-util API) | each backend calls its own builder; foundation supplies `client_builder()` |
| FIPS init | install rustls aws-lc-rs default provider, once per process | same | `libdd-common::http::install_fips_provider` (or keep the existing surface — see §2.5) |
| Error mapping (timeout / closed / canceled / parse / parse-status / write-aborted / incomplete / other) | `reqwest::Error::is_timeout` etc. → `HttpClientError` | `hyper::Error::is_timeout` etc. → `ClientError`/`ErrorKind` → `HttpClientError` | hyper backend uses `libdd-common::http::ErrorKind`; reqwest backend keeps its own mapping (no shared error layer in M3 — see §3.3) |

### 2.5 Where FIPS init lives

Today:
- `libdd-common/src/connector/mod.rs::ensure_crypto_provider_initialized()`
  installs `rustls::crypto::ring` for the `https` feature. It is a *no-op*
  for the `fips` feature, where the caller is expected to install
  aws-lc-rs first.
- `libdd-http-client::init_fips_crypto()` explicitly installs aws-lc-rs.

**M3 decision:** add `libdd_common::http::install_fips_provider()` (cfg `fips`)
that does the aws-lc-rs install. `libdd-http-client::init_fips_crypto`
keeps the same signature but delegates to the libdd-common helper.
`libdd-agent-client` will gain a re-export. M7 FFI exposes a thin C wrapper
around `libdd_http_client::init_fips_crypto` (`ddog_http_client_init_fips`).

This keeps the fix-once-per-process semantics centralized and removes the
risk that two crates each try to install a different default provider.

---

## 3. Per-backend reuse strategy

### 3.1 Current duplication inventory

Lines that today live in `libdd-http-client/src/backend/hyper_backend.rs`
and could be shared (or already are) with `libdd-common/src/http_common.rs`:

| Concern | hyper_backend.rs | http_common.rs | Shared after M3? |
|---|---|---|---|
| `Body` construction | uses `Body::{empty, from_bytes}` directly | defines them | **already shared** — confirms direction |
| `client_builder()` | `http_common::client_builder()` | defines it | **already shared** |
| `Connector` | `Connector::default()` | defined in `connector/` | **already shared** |
| Error mapping (`hyper_util::Error` → categorised) | `map_hyper_error` (lines 137–144) re-uses `ClientError::from` and inspects `ErrorKind` | defines `ClientError`, `ErrorKind`, `From<hyper_util::Error>` impl | **already shared** at the `ErrorKind` level; the `HttpClientError` mapping stays in the backend |
| Multipart encoding | `MultipartFormData::encode(parts)` | n/a (lives in `multipart.rs`) | **already shared** (M1) |
| URL rewrite for UDS / named pipes | `rewrite_url` + `build_transport_uri` (lines 27–66) | `connector::uds::socket_path_to_uri`, `connector::named_pipe::named_pipe_path_to_uri` | **partially shared** — the URI helpers come from libdd-common; the `rewrite_url` glue stays in the backend |
| Header collection | `collect_response_headers` (lines 119–135) | n/a | **stays in the backend** — trivially small, backend-flavour-specific |

The hyper backend is **already** a thin shim over libdd-common's HTTP
foundation. Today (line 13): `client: http_common::GenericHttpClient<Connector>`.
The M3 work for the hyper backend is mostly cosmetic: switch the import path
from `libdd_common::http_common` to `libdd_common::http` (or via a re-export
shim, no source change needed).

The **reqwest backend** by contrast holds a `reqwest::Client` and reaches into
**none** of `http_common`. M3's reqwest-side consolidation is therefore
*not* about replacing `reqwest::Client` — it's about routing FIPS init,
endpoint helpers (`Endpoint::to_reqwest_client_builder` already exists in
libdd-common), and any future cross-cutting concern (request logging, retry
policy types) through the same foundation.

### 3.2 What each backend will share vs keep internal after M3

#### Hyper backend (`libdd-http-client/src/backend/hyper_backend.rs`)

**Shared from `libdd-common::http`:**
- `Body`, `Sender` (constructing request bodies, building channel-based bodies for streaming)
- `GenericHttpClient<C>` and `client_builder()` for client construction
- `into_response`, `empty_response`, `collect_response_bytes`
- `ClientError`, `ErrorKind` for the error-mapping helper
- `Connector` (via `libdd-common::connector::Connector`)
- `MultipartFormData`, `MultipartPart` (via `libdd-common::multipart`)
- `install_fips_provider()` (delegated by `libdd-http-client::init_fips_crypto`)

**Stays internal to the backend:**
- `HyperBackend` struct itself.
- `rewrite_url()` and `build_transport_uri()` — backend-specific URL prep.
- `convert_method()` / `convert_multipart_part()` — small adapters between
  `libdd-http-client::HttpRequest` and `hyper::Request`.
- `collect_response_headers()` — backend-flavour utility.
- `map_hyper_error()` — maps `ClientError`/`ErrorKind` (from libdd-common)
  to the public `HttpClientError`.
- `build_body()` — wires `HttpRequest` (libdd-http-client) into
  `libdd-common::http::Body`.

#### Reqwest backend (`libdd-http-client/src/backend/reqwest_backend.rs`)

**Shared from `libdd-common::http`:**
- `install_fips_provider()` (FIPS init).
- (Optional, future) `libdd-common::Endpoint::to_reqwest_client_builder()` —
  the existing helper that already centralises UDS/Windows-named-pipe/file
  transport setup. M3 may *defer* using this helper to keep the change
  surface tight; it's a good Task 3 follow-up if it doesn't bloat the diff.

**Stays internal to the backend:**
- `ReqwestBackend` struct, `build_multipart_form()`, `map_reqwest_error()`,
  the `reqwest::Method` / `HttpMethod` adapter, transport setup using
  `reqwest`'s `unix_socket` / `windows_named_pipe` builder methods.
- The `hickory-dns` / `multipart` / `rustls-no-provider` reqwest features —
  these cargo features stay on the libdd-http-client side.

#### What the consumers of libdd-common's old surface get

The crates in §1.2's "migrate-now" rows continue to construct HTTP clients
the way they do today, but call the **new module path**:

```rust
// before (still works through the shim):
let client = libdd_common::http_common::new_default_client();

// after Task 4:
let client = libdd_common::http::new_default_client();
```

No semantic change. No new types. No new traits.

### 3.3 What is *not* shared between backends

- **Error types.** `HttpClientError` (libdd-http-client's public error)
  stays the unifying surface for backend errors. `ClientError`/`ErrorKind`
  from libdd-common is hyper-only and is intentionally not exposed through
  `HttpClientError` (it is *consumed* by `map_hyper_error`).
- **Connection-pool tuning.** Each backend uses its own builder API
  (`reqwest::ClientBuilder::pool_max_idle_per_host` vs
  `hyper_util::client::legacy::Builder::pool_max_idle_per_host`).
  `allow_connection_pooling` lives in `HttpClientConfig` and each backend
  threads it through itself.
- **Timeout enforcement.** Reqwest enforces timeout natively
  (`builder.timeout()`); hyper backend wraps `client.request(..)` in
  `tokio::time::timeout`. This split stays.
- **Multipart encoding.** Reqwest uses `reqwest::multipart::Form`
  (which understands `mime_str` and reqwest's own boundary generation).
  Hyper uses `libdd_common::multipart::MultipartFormData`. They produce
  semantically equivalent payloads but the wire-format boundary differs,
  which is fine (both ends are content-agnostic).

---

## 4. Binary-size methodology

### 4.1 Pinned control and target

- **Control (does NOT import `libdd-http-client`):** `libdd-profiling-ffi`
  cdylib (`libdatadog_profiling_ffi.so` on Linux). Its default features
  do not pull `libdd-http-client`; it does pull `libdd-telemetry-ffi`
  optionally as a feature. We measure with **default features only** so
  the comparison stays on the path that does not import the new HTTP stack.
  Rationale: it is the largest non-HTTP-client cdylib in the repo and
  isolates "noise" coming from cargo's link/LTO behaviour.
- **Target (post-Task 4 will import `libdd-http-client` indirectly via
  Task 7's `DefaultHttpClient` swap):** `libdd-telemetry-ffi` cdylib
  (`libdatadog_telemetry_ffi.so`). It is the most direct dependent on
  `libdd-telemetry`, which is the heaviest of the migrate-now consumers.
  *In M3 (Task 4)*, the telemetry crate's HTTP path is rewired through the
  shared `libdd-common::http` module but still uses hyper directly (not
  through libdd-http-client). The telemetry-ffi number therefore captures
  the M3 dedup gains alone.
- **Target post-M7:** the new `libdd-http-client-ffi` and
  `libdd-agent-client-ffi` cdylibs ship in the release artifact. M7's PR
  re-runs the same methodology with these as additional targets.

### 4.2 Measurement command (Linux x86_64)

Reference target triple: `x86_64-unknown-linux-gnu`. Run from repo root.

```bash
# 1. Build the release cdylib (uses workspace [profile.release] = lto/opt=s/cu=1).
cargo build --release \
    --target x86_64-unknown-linux-gnu \
    -p libdd-profiling-ffi    # or libdd-telemetry-ffi for the target

# 2. Locate and strip the cdylib.
artifact=target/x86_64-unknown-linux-gnu/release/libdatadog_profiling_ffi.so
cp "$artifact" /tmp/probe.so
strip --strip-unneeded /tmp/probe.so

# 3. Record size in bytes.
stat --format='%s' /tmp/probe.so
```

The release profile in `Cargo.toml` already pins:

```toml
[profile.release]
codegen-units = 1
debug = "line-tables-only"
lto = true
opt-level = "s"            # optimize for size
```

so no profile overrides are passed.

### 4.3 Feature set

Always build the **default features** of the cdylib crate (no
`--all-features`, no `--no-default-features` overrides). Specifically:

- `libdd-profiling-ffi` default = `["ddcommon-ffi"]` (no http-client path).
- `libdd-telemetry-ffi` default = `["cbindgen", "expanded_builder_macros"]`
  (no extra HTTP toggles).
- `libdd-http-client-ffi` (post-M7): the default it ships with — pinned in
  Task 9's PR.

If a measurement matrix is needed for the FIPS path (`fips` feature),
record it as a separate row; do not blend it with the `https` measurement.

### 4.4 Diff procedure and acceptance threshold

For each PR that lands a chunk of M3:

1. Build + strip the cdylib for the M3 base commit (the parent of the PR).
2. Build + strip the cdylib for the PR HEAD.
3. Diff: `delta_bytes = head_size - base_size`,
   `delta_pct = delta_bytes / base_size`.
4. Record both numbers in the PR description for **control** and **target**.
5. **Threshold (per PLAN.md "Verification — M3 close"):** if the
   target's `delta_pct > +2%`, sign-off from the libdd-common owner is
   required before merging.

### 4.5 Sources of noise to be aware of

- Cargo's incremental cache: always run `cargo clean -p <crate>` before each
  build, or build in a fresh `target/` dir.
- `rustc` toolchain drift: pin to the workspace `rust-version = 1.84.1`
  (the workspace Cargo.toml already enforces this).
- Dynamic-linker layout differences: always strip with the same `strip`
  binary, same flags.
- Per-platform variance: only Linux x86_64 numbers are normative for M3;
  macOS / Windows values are recorded for awareness only.

---

## 5. Fork-safety implications per backend

This section primes Task 6 (M4 fork-safety validation). It does **not**
prescribe code; it captures the contract each backend brings.

### 5.1 Reqwest backend

A `reqwest::Client` (with `hickory-dns` enabled — cf.
`libdd-http-client/Cargo.toml`: `reqwest?/hickory-dns`) brings into the
process:

- An internal **HTTP/1 + HTTP/2 connection pool** (idle connections per
  host).
- An internal **hickory-dns resolver task** (in-process, fork-safe by
  itself in the sense that no global libc resolver state is held).
- A **tokio runtime handle** that the client was constructed against (the
  `Tokio` global executor or the runtime captured at `build()` time).
- TLS state held by `rustls` — itself fork-safe at the data-structure level
  but the `aws-lc-rs` / `ring` provider has internal RNG state.

**Post-fork contract:** the **child process must reconstruct the
`reqwest::Client`**. There is no zombie file descriptor problem (no
classical FD leakage to worry about), but pool sockets and the dns task
that lived in the parent process are *not* alive in the child. A child
that tries to send through the inherited client will hang on a pool
connection that the kernel side believes is still half-open from the
parent.

The `Endpoint::to_reqwest_client_builder` already chooses **hickory-dns**
by default (`hickory_dns(!self.use_system_resolver)` —
`libdd-common/src/lib.rs:384`). That decision was made specifically
*because* the system resolver holds locks and global state across
fork. Reqwest's default-resolver-uses-glibc would break PHP-FPM and
similar fork-heavy SAPIs. M3 keeps this default.

### 5.2 Hyper backend

A `GenericHttpClient<Connector>` brings into the process:

- A **hyper-util connection pool** with idle entries.
- A **`Connector`** which is `Http(HttpConnector)` or
  `Https(HttpsConnector)`. The TLS variant holds rustls state but no
  background tasks.
- A **tokio runtime executor handle** (`TokioExecutor`).
- For UDS / named-pipes: only an `AsyncFd`-style stream per request — no
  background tasks of its own.

**Post-fork contract:** simpler than reqwest's. Reconstruct the client in
the child *only* if you intend to send. No DNS task, no internal worker
pool beyond the tokio executor. The only fork-time concern is the tokio
runtime itself — owned by `libdd-shared-runtime::SharedRuntime`, which has
its own `before_fork` / `after_fork_parent` / `after_fork_child` hooks
(`libdd-shared-runtime/src/shared_runtime/mod.rs`).

### 5.3 What this means for Task 6

Task 6 will publish `libdd-http-client/docs/fork-safety.md` documenting:

1. The post-fork contract: **rebuild the `HttpClient` in the child process
   before sending**. This is identical for both backends, but the failure
   modes differ:
   - Reqwest: child hangs on stale pool socket / dead DNS task.
   - Hyper: child immediately gets `ClientError(Closed)` or `Canceled`
     when the inherited tokio task hits a cancelled handle.
2. The `SharedRuntime` integration: `HttpClient::send` is called inside
   `shared_runtime.block_on(...)`. The child must call
   `shared_runtime.after_fork_child()` *before* attempting any send.
3. A reqwest-specific note: `hickory_dns(true)` is the default and must
   stay the default. `Endpoint::with_system_resolver(true)` exists as an
   escape hatch but is **not fork-safe**; document this clearly.
4. CI-level fork-safety smoke tests: spawn a child, call `HttpClient::send`
   to a localhost mock server post-fork on both backends.

M3 does not add fork-safety code. It only ensures the foundation
(`Connector`, `Body`, `client_builder`, FIPS init) is in one place so
Task 6 has a single surface to instrument.

---

## 6. Hand-off to Task 2

Task 2 ("Extract shared foundation in libdd-common, with crashtracker as
proof") starts with these decisions baked in. Concretely, Task 2's PR
should:

1. Add `libdd-common/src/http/{mod,body,client,error,fips}.rs` (or a flatter
   `libdd-common/src/http.rs` if Task 2's implementer prefers — both work,
   the directory is recommended for room).
2. Move the contents of `libdd-common/src/http_common.rs::native::*` into
   the new module (the portable wasm-only types stay in `http_common.rs`
   for now; revisit in Task 4 cleanup).
3. Add the shim:
   ```rust
   // libdd-common/src/lib.rs
   pub mod http;
   pub mod http_common {
       pub use crate::http::*;
       // plus the wasm-only types still defined in this file
   }
   ```
4. Add `install_fips_provider` and have `libdd-http-client::init_fips_crypto`
   delegate to it.
5. **Crashtracker proof:** migrate
   `libdd-crashtracker/src/crash_info/errors_intake.rs:593` from
   `libdd_common::http_common::new_client_periodic()` to
   `libdd_common::http::new_client_periodic()` as the proof-of-life that
   the new path works end-to-end. (Crashtracker is the smallest
   migrate-now consumer; one call site, one PR — perfect proof.)
6. Confirm the existing `libdd_common::HttpClient` and `libdd_common::HttpResponse`
   type aliases in `lib.rs` still resolve (via the shim).

Task 3 ("Migrate libdd-http-client backends onto shared foundation") then
flips `libdd-http-client/src/backend/hyper_backend.rs:10` from `http_common`
to `http`, and stitches FIPS init through `libdd-common::http::install_fips_provider`.
The reqwest backend gets the FIPS-init delegation only — no other source
change in Task 3.

Task 4 ("Migrate remaining internal consumers") covers rows 5–14 of the
table in §1.2 (9 files). Task 11 covers row 15
(`libdd-trace-utils::test_utils::datadog_test_agent`) in isolation.

---

## 7. Open follow-ups (not blockers)

- **Whether to `#[deprecated]` the `libdd_common::http_common` shim** after
  Task 4 / Task 11 / Task 7 are all merged. Recommended: yes, with a one-cycle
  deprecation note pointing at `libdd_common::http`. Tracked as a follow-up
  to Task 4, not a blocker for M3 close.
- **Whether `libdd-common::Endpoint::to_reqwest_client_builder` should also
  set up FIPS / system-resolver-vs-hickory based on a feature flag rather
  than `Endpoint.use_system_resolver`.** Out of scope for M3.
- **`libdd-data-pipeline-ffi`'s control role.** It currently imports
  `libdd-capabilities-impl` (which carries `DefaultHttpClient` over hyper).
  Once Task 7 lands, data-pipeline-ffi will indirectly link
  `libdd-http-client`. If we want a stable cdylib control through M3 → M7,
  `libdd-profiling-ffi` is the right pick — it does not pull
  capabilities-impl by default.
