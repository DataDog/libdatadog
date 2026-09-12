# External Integrations

**Analysis Date:** 2026-06-15

## APIs & External Services

**Datadog Agent:**
- Local agent endpoint (default: `http://localhost:8126`)
- Environment variables: `DD_TRACE_AGENT_URL`, `DD_AGENT_HOST`, `DD_TRACE_AGENT_PORT`
- Supports Unix domain socket: `/var/run/datadog/apm.socket` (Unix only)
- Windows named pipe support: via `DD_TRACE_PIPE_NAME` environment variable
- Used by: `libdd-telemetry`, `libdd-profiling-ffi`, trace exporters
- SDK/Client: Built-in via `libdd-http-client` and `libdd-agent-client`

**Datadog Intake (Agentless):**
- Direct submission to Datadog infrastructure
- Controlled by: `_DD_DIRECT_SUBMISSION_ENABLED` environment variable
- Endpoints: `https://{SUBDOMAIN}-intake.datadoghq.com/` (customizable via `DD_SITE`)
- Subdomain: `instrumentation-telemetry-intake` for telemetry
- Requires: `DD_API_KEY` environment variable for authentication
- Used by: `libdd-telemetry`, `libdd-profiling-ffi`

**DogStatsD:**
- Metrics client for sending metrics to DogStatsD agent
- SDK/Client: `cadence` 1.3.0 via `libdd-dogstatsd-client`
- Purpose: In-process metrics collection and agent submission
- Configuration: Via `libdd-common` Endpoint management

**Remote Configuration Service:**
- Feature-gated client in `libdd-remote-config`
- Fetches runtime configuration from Datadog
- Implementation: `libdd-remote-config/src/` with protobuf message support
- Used by: Sidecar and instrumentation for feature flags, configuration updates

## Data Storage

**Databases:**
- Not applicable - libdatadog is a library, not a service
- Applications using libdatadog manage their own persistence

**File Storage:**
- Local filesystem only (no cloud storage integration)
- Temporary files: via tempfile crate
- FFI examples use CMake for artifact generation

**Caching:**
- In-memory caching via hashbrown (hash maps)
- HTTP response buffering via hyper and reqwest
- No external caching service integration

## Authentication & Identity

**Auth Provider:**
- Custom Datadog API key-based authentication
- Implementation: `libdd-common` handles authentication headers
- Location: `libdd-common/src/connector/` for endpoint initialization
- API Key: `DD_API_KEY` environment variable (required for direct submission)
- No OAuth, no third-party identity providers

**FFI Credential Management:**
- C FFI layer handles credential passing from caller
- Credentials not persisted by libdatadog
- Caller responsibility to manage secret handling

## Monitoring & Observability

**Error Tracking:**
- Structured error reporting via `thiserror` enums
- Error context via `anyhow` Result type
- Datadog crash tracking via `libdd-crashtracker`
- FFI crash collector with in-process and receiver modes
- Symbol demangling for stack traces via `symbolic-demangle`

**Logs:**
- Structured logging via `tracing` crate
- JSON-formatted output via `tracing-subscriber`
- Output: stderr (default) or file (via `tracing-appender`)
- Log levels controlled by environment or code configuration
- Sidecar includes dedicated logging configuration

**Metrics & Telemetry:**
- Built-in telemetry via `libdd-telemetry` crate
- Heartbeat interval: configurable via `DD_TELEMETRY_HEARTBEAT_INTERVAL`
- Extended heartbeat interval: `DD_TELEMETRY_EXTENDED_HEARTBEAT_INTERVAL`
- Self-telemetry in sidecar: via `_DD_SIDECAR_SELF_TELEMETRY`
- Watchdog monitoring: memory usage via `memory-stats` crate

## CI/CD & Deployment

**Hosting:**
- Multi-platform: Linux (x86_64, ARM), macOS (Intel, Apple Silicon), Windows
- Deployed as: shared libraries (`.so`, `.dylib`, `.dll`), static archives (`.a`, `.lib`), or CMake packages
- Builder: `cargo run --bin release` generates release artifacts (see `builder/Cargo.toml`)

**CI Pipeline:**
- GitHub Actions (inferred from .github directory)
- cargo-nextest for parallel test execution
- cargo clippy for linting
- cargo fmt nightly for formatting
- cargo deny for dependency audits
- Optional: Docker for integration tests (tracing_integration_tests)

**Build Features:**
- Default features: crashtracker, profiling, telemetry, data-pipeline, symbolizer, library-config, log, ddsketch, ffe, shared-runtime
- Feature flags for selective compilation:
  - `https` - TLS support
  - `fips` - FIPS-compliant cryptography
  - `cbindgen` - C header generation
  - `fuzzing` - Fuzz testing harness
  - Per-crate: `regex-lite` for binary size reduction

## Environment Configuration

**Required env vars:**
- `DD_API_KEY` - Datadog API key (required for direct submission only)

**Optional env vars:**
- `DD_TRACE_AGENT_URL` - Override agent endpoint (e.g., `http://custom-agent:8126`)
- `DD_AGENT_HOST` - Agent hostname (default: localhost)
- `DD_TRACE_AGENT_PORT` - Agent port (default: 8126)
- `DD_TRACE_PIPE_NAME` - Named pipe endpoint (Windows)
- `DD_SITE` - Datadog site (default: datadoghq.com) — used to construct intake URLs
- `_DD_DIRECT_SUBMISSION_ENABLED` - Enable direct submission to intake
- `DD_TELEMETRY_HEARTBEAT_INTERVAL` - Telemetry heartbeat frequency
- `DD_APM_TELEMETRY_DD_URL` - Custom telemetry endpoint URL
- `_DD_SHARED_LIB_DEBUG` - Enable debug logging

**Secrets location:**
- Managed by caller (not stored by libdatadog)
- API key passed via environment or direct parameter
- No credential files or vaults used by libdatadog

## Webhooks & Callbacks

**Incoming:**
- Remote Configuration service receives config updates from Datadog control plane
- Implementation: `libdd-remote-config` with client polling
- No webhook endpoints exposed; polling-based instead

**Outgoing:**
- Telemetry data sent to Datadog intake or agent
- Endpoint: `/api/v2/apmtelemetry` (direct submission) or `/telemetry/proxy/api/v2/apmtelemetry` (via agent)
- Data format: JSON-serialized telemetry events
- Tracing data: Protobuf format via `/v0.4/traces` or agent equivalents
- Profiling data: Protobuf format via custom profiling endpoints
- DogStatsD metrics: UDP protocol (via cadence client)

## Cross-Platform Considerations

**Unix/Linux:**
- Unix domain socket support: `/var/run/datadog/apm.socket`
- Fork-safe DNS resolver via hickory-dns
- POSIX system calls via `nix` crate
- Native certificate store access

**Windows:**
- Named pipe support for agent communication
- Windows API bindings via `windows` and `windows-sys` crates
- Native certificate store integration
- FIPS environment variable: `AWS_LC_FIPS_SYS_NO_ASM=1` required for FIPS mode

**Web Assembly (wasm32):**
- Limited support (some dependencies are conditional)
- No system networking for wasm target

---

*Integration audit: 2026-06-15*
