# OTLP HTTP/protobuf trace export

- **Date:** 2026-06-12
- **Status:** Approved design, pending implementation plan
- **Repos:** `libdatadog` (feature), `dd-trace-py` (SDK wiring + E2E)
- **Branch (libdatadog):** `brian.marks/otlp-http-protobuf-export`

## Background

libdatadog can export traces over OTLP, but only as **HTTP/JSON**. The trace exporter
decodes incoming (msgpack) DD spans, maps them to an OTLP `ExportTraceServiceRequest`, serializes
that to JSON, and POSTs it with `Content-Type: application/json`.

The groundwork for more encodings already exists:

- `OtlpProtocol::{HttpJson, HttpProtobuf, Grpc}` is stubbed in `libdd-data-pipeline/src/otlp/config.rs`
  (`HttpProtobuf` and `Grpc` carry `#[allow(dead_code)]` and "not supported yet").
- The transport (`send_otlp_traces_http`) is format-agnostic: it POSTs a `Vec<u8>` body with a
  content-type header and retries. The sidecar already POSTs `application/x-protobuf` for FFE metrics.
- `libdd-common::header::APPLICATION_PROTOBUF` (`application/x-protobuf`) already exists.
- `libdd-trace-protobuf` already vendors the OTLP `common/v1` and `resource/v1` protos and generates
  Rust from them via `prost-build` + `protoc-bin-vendored` behind its `generate-protobuf` feature.
- The hand-rolled serde JSON types (`libdd-trace-utils/src/otlp_encoder/json_types.rs`) deliberately
  duplicate the OTLP schema; the file comment anticipates a separate protobuf path.

dd-trace-py is already pre-wired: it reads `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` /
`OTEL_EXPORTER_OTLP_PROTOCOL` into a `TRACES_PROTOCOL` setting (validated to `http/json` /
`http/protobuf`), exposes `TraceExporterBuilder` to Python via PyO3 with `set_otlp_endpoint` /
`set_otlp_headers`, and has a comment noting `TRACES_PROTOCOL` is "collected for telemetry but not
yet used to switch transport" because libdatadog only supports JSON.

## Goal & scope

Add OTLP **HTTP/protobuf** as a second trace-export encoding alongside HTTP/JSON, selectable per the
OTel-standard protocol values, and wire it through dd-trace-py so it is reachable from the SDK.

**In scope**

- Traces only.
- Encodings: `http/json` (existing) and `http/protobuf` (new).
- Protocol selection via the Rust builder, the C FFI, and dd-trace-py's Python builder + writer.
- Validation: Rust unit/integration tests, a dd-trace-py local E2E with a protobuf-decoding receiver,
  system-tests against locally-built artifacts, and sdk-backend-verify against the Datadog backend.

**Out of scope (non-goals)** — see "Non-goals / future" for where the design leaves room:

- gRPC transport.
- gzip / `Content-Encoding`.
- OTLP `partial_success` response parsing.
- logs / metrics signals.

## Decisions

1. **Type source: vendor `.proto` + generate prost types** (not the `opentelemetry-proto` crate).
   Rationale: `opentelemetry-proto 0.31` aligns with the workspace's prost 0.14, but its manifest makes
   `opentelemetry` and `opentelemetry_sdk` non-optional and requires `tonic` + `tonic-prost` for the
   message types — it drags the OTel Rust SDK and tonic into the widely-used `libdd-trace-utils`. For a
   footprint-sensitive FFI library, vendoring the protos and generating prost types via the existing
   `libdd-trace-protobuf` pipeline adds **zero new runtime dependencies** and follows an established
   in-repo pattern.

2. **Keep the hand-rolled serde JSON path; do not unify onto shared types.**
   Rationale: OTLP/JSON deviates from canonical protobuf-JSON (trace/span IDs are hex, not base64;
   int64 is a string). The hand-rolled serde types already implement this correctly and are tested.
   Generating JSON from prost types (e.g. `pbjson`) would emit base64 IDs — wrong per the OTLP/JSON
   spec. So the JSON path stays exactly as-is.

3. **Share the mapping logic via one mapper + a mechanical converter.**
   Rationale: the semantic DD-span→OTLP mapping (128-bit trace-id reconstruction, span-kind inference,
   attribute limits, status, flags) runs once in `map_traces_to_otlp` and produces the serde types. The
   protobuf path adds only a dumb, fully-tested structural converter from the serde types to the
   generated prost types. No mapping logic is duplicated.

## Architecture & data flow

```
DD spans (msgpack-decoded)
        │
        ▼
map_traces_to_otlp(...)  ──►  ExportTraceServiceRequest   (hand-rolled serde types — UNCHANGED)
        │
        ├─ HttpJson     ─► serde_json::to_vec(&req)              ─► Content-Type: application/json
        └─ HttpProtobuf ─► (&req).into() : proto::Export…Request ─► prost encode_to_vec ─► application/x-protobuf
                              (mechanical serde→prost converter; no mapping logic duplicated)
```

The endpoint path (`/v1/traces`), retry strategy, sampling enforcement (unsampled chunks dropped
before export), and resource attributes are unchanged.

## Component changes — libdatadog

### A. `libdd-trace-protobuf` — vendor + generate the prost types

- Add vendored protos under `src/pb/opentelemetry/proto/`:
  - `trace/v1/trace.proto`
  - `collector/trace/v1/trace_service.proto` (defines `ExportTraceServiceRequest`)
- Add both to the `compile_protos([...])` list in `build.rs` (alongside the existing common/resource
  entries).
- Regenerate under `--features generate-protobuf` and commit the new `opentelemetry.proto.trace.v1.rs`
  and `opentelemetry.proto.collector.trace.v1.rs` (matching the checked-in-generated convention).
- Net new external runtime deps: **zero** (`prost`, `prost-build`, `protoc-bin-vendored` already present).

### B. `libdd-trace-utils::otlp_encoder` — converter + two encoders, feature-gated

- `json_types.rs` and `mapper.rs`: **unchanged.**
- New `proto_convert.rs`: `impl From<&ExportTraceServiceRequest> for proto::ExportTraceServiceRequest`,
  converting hex-string→16/8-byte IDs, int-string→i64, base64-string→bytes, the `AnyValue` enum→prost
  `any_value::Value`, dropped counts, flags, status, links, events. Behind a new `otlp-protobuf` cargo
  feature that pulls the generated types from `libdd-trace-protobuf`.
- `mod.rs` exposes:
  - `encode_otlp_json(&req) -> serde_json::Result<Vec<u8>>` (always available),
  - `encode_otlp_protobuf(&req) -> Vec<u8>` (feature-gated).
- The feature gate keeps non-OTLP and JSON-only consumers of `libdd-trace-utils` from paying for the
  protobuf types.

### C. `libdd-data-pipeline` — protocol dispatch + config plumbing

- `otlp/config.rs`: make `OtlpProtocol` `pub`; add `impl FromStr` (`"http/json"→HttpJson`,
  `"http/protobuf"→HttpProtobuf`, `"grpc"→Grpc`); drop `#[allow(dead_code)]` on `HttpProtobuf`.
- `otlp/exporter.rs` (`send_otlp_traces_http`): set content-type from `config.protocol`
  (`APPLICATION_JSON` vs `APPLICATION_PROTOBUF`) instead of hardcoding JSON; rename `json_body`→`body`.
- `trace_exporter/mod.rs` (`send_otlp_traces_inner`): replace the hardcoded `serde_json::to_vec` with a
  `match config.protocol` selecting `encode_otlp_json` / `encode_otlp_protobuf`. `Grpc` returns a clear
  "not yet supported" `TraceExporterError`.
- `trace_exporter/builder.rs`: add `set_otlp_protocol(OtlpProtocol)`; use it where `OtlpProtocol::HttpJson`
  is currently hardcoded. Enable the `otlp-protobuf` feature on the `libdd-trace-utils` dep.

### D. `libdd-data-pipeline-ffi` — protocol setter

- Add `otlp_protocol` to `TraceExporterConfig` and
  `ddog_trace_exporter_config_set_otlp_protocol(config, CharSlice)` that parses the OTel string via
  `FromStr`, rejecting `"grpc"` with `InvalidArgument` + a clear message.
- Apply it in the create fn next to `set_otlp_endpoint`. Regenerate the C header.

## Protocol config surface

Mirror the OTel SDK / dd-trace-java naming: callers pass `http/json` or `http/protobuf` (the values they
read from `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` / `OTEL_EXPORTER_OTLP_PROTOCOL`). libdatadog does not read
env vars itself — the host tracer resolves the value and calls the setter, consistent with
`set_otlp_endpoint`.

**Default = `HttpJson`**, to preserve current behavior for existing integrations. (The OTel SDK and
dd-trace-java default to `http/protobuf`; keeping JSON the default here avoids changing behavior for
callers who don't set the protocol. Easy to flip later.)

## Component changes — dd-trace-py (companion PR)

1. **PyO3 binding** — `src/native/data_pipeline/mod.rs`: add `set_otlp_protocol(&str)` forwarding to the
   new builder method.
2. **Writer wiring** — `ddtrace/internal/writer/writer.py` `_create_exporter()`: call
   `builder.set_otlp_protocol(otel_config.exporter.TRACES_PROTOCOL)` when OTLP is enabled.
3. **Un-stub the comments** — drop the "not yet used to switch transport" note at
   `ddtrace/internal/settings/_opentelemetry.py` and the "libdatadog currently only supports http/json"
   default note.
4. **Cargo dependency** — once libdatadog ships a release containing this feature, bump the
   `rev = "v35.0.0"` git pins in `src/native/Cargo.toml`. Until then, the local cargo patch (below) is
   used for E2E.

The dd-trace-py PR is only mergeable after a libdatadog release contains the feature; it is sequenced
after the libdatadog PR.

## Testing strategy — libdatadog

- Existing JSON snapshot test (`otlp_export_sends_correct_payload`) and all `mapper.rs` unit tests stay
  green, unchanged (JSON path untouched).
- New `proto_convert` unit tests: serde→prost equivalence (trace/span/parent IDs as bytes, kind, status,
  all `AnyValue` variants incl. bytes/array, dropped counts, flags, links, events).
- New protobuf export integration test (mirrors the JSON one): mock server asserts
  `Content-Type: application/x-protobuf` + path `/v1/traces`, then prost-decodes the body and asserts
  `resource_spans` / `service.name` / span names.
- New parity test: `map → encode_json` vs `map → encode_protobuf → prost-decode` carry identical data —
  guards the two encoders against drift.
- `FromStr` + FFI-setter tests (including `grpc` rejection).
- `cargo ffi-test` (C/C++ examples) since FFI signatures change.

## E2E validation

Layered, from fastest/most-deterministic to fullest-chain.

### Tier 1 — dd-trace-py local receiver (deterministic, repeatable)

- Point dd-trace-py at the local libdatadog build via a git-keyed cargo patch in `src/native/`
  (the deps are git deps, so this is **not** `[patch.crates-io]`):

  ```toml
  [patch."https://github.com/DataDog/libdatadog"]
  libdd-data-pipeline  = { path = "/path/to/local/libdatadog/libdd-data-pipeline" }
  libdd-trace-utils    = { path = "/path/to/local/libdatadog/libdd-trace-utils" }
  libdd-trace-protobuf = { path = "/path/to/local/libdatadog/libdd-trace-protobuf" }
  # + any other crate in the modified set
  ```

  dd-trace-py builds use libdatadog's committed generated prost code, so no `protoc` is needed there.

- Build dd-trace-py in a fresh venv (`pip install -e .`).
- Run a small local OTLP/HTTP receiver on `:4318` handling `POST /v1/traces`: assert
  `Content-Type: application/x-protobuf`, `ExportTraceServiceRequest().ParseFromString(body)` with the
  `opentelemetry-proto` Python package, and assert resource `service.name`, span names, and the
  32-hex-char `trace_id` survive the round trip.
- Run a tiny instrumented app twice — `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf` and `http/json`
  — confirming the new path works and the existing JSON path is unaffected. Ensure
  `DD_TRACE_AGENT_PROTOCOL_VERSION` is unset (it disables OTLP).

### Tier 2 — system-tests against local builds (via `apm-ecosystems:system-tests-local`)

- Build dd-trace-py against the local libdatadog (Tier 1 patch), then run the relevant system-tests
  OTLP scenario(s) with the locally-built tracer. The exact scenario / parametric test name is to be
  identified during planning. Goal: exercise the protobuf path through the supported system-tests
  harness rather than only a bespoke receiver.

### Tier 3 — sdk-backend-verify (full chain to the Datadog backend, via `apm-ecosystems:sdk-backend-verify`)

- Run the instrumented app with `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf` against an OTLP
  receiver that forwards to the Datadog backend (DD Agent OTLP intake on `:4318`, or the OTel Collector
  with a Datadog exporter), then verify the spans land in the backend with correct service/resource/
  trace-id via the backend APIs. Confirms the protobuf bytes are accepted end-to-end and ingested.

## Validation gauntlet (per AGENTS.md)

For each touched crate: `cargo check -p <crate>` →
`cargo +nightly-2026-02-08 fmt --all -- --check` →
`cargo +stable clippy --workspace --all-targets --all-features -- -D warnings` →
`cargo nextest run` (workspace + all-features) → `cargo test --doc` → `cargo ffi-test`.
If `Cargo.lock` changes: `./scripts/update_license_3rdparty.sh` + `cargo deny check`.
Apache headers on new files via `./scripts/reformat_copyright.sh`.

## Risks & mitigations

- **Footprint spike (Phase 0 gate):** before real work, add the vendored protos, regenerate, and confirm
  `cargo tree -p libdd-trace-utils --features otlp-protobuf` shows no new heavy crates. This is the whole
  premise of decision 1 — go/no-go.
- **Converter correctness** (hex/base64/int-string round-trips): covered by the parity and converter
  unit tests.
- **proto3 field presence:** prost uses `0`/empty for absent scalars; the converter must map
  `Option`/empty consistently. Covered by unit tests; semantically harmless for OTLP receivers.
- **Cross-repo sequencing:** the dd-trace-py PR depends on a libdatadog release. E2E uses the local
  cargo patch until then; the PR documents the required version bump.

## Non-goals / future hooks

- **gRPC:** `OtlpProtocol::Grpc` stays; rejected at the setter/exporter. A future addition is isolated to
  the exporter plus a transport that doesn't fit today's HTTP/1 client.
- **gzip:** add later as a `Content-Encoding` on the existing body (`flate2` is already available).
- **`partial_success`:** neither dd-trace-go nor dd-trace-java parse it; keep status-only handling.

## Sequencing / PR plan

1. **libdatadog PR** (this branch): feature + unit/integration tests + regenerated protos + C header.
2. **Local E2E** (Tier 1) against the libdatadog branch via cargo patch in a dd-trace-py worktree.
3. **dd-trace-py PR**: PyO3 binding + writer wiring + comment cleanup; depends on a libdatadog release
   bump. Validated with system-tests (Tier 2) and sdk-backend-verify (Tier 3) against local builds.
