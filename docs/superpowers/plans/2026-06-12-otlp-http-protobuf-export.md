# OTLP HTTP/protobuf trace export — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add OTLP HTTP/protobuf as a second trace-export encoding alongside HTTP/JSON in libdatadog, selectable via the OTel-standard protocol values, and wire it through dd-trace-py with end-to-end validation.

**Architecture:** Vendor the OTLP `trace` + `collector/trace` protos into `libdd-trace-protobuf` and generate prost types (zero new runtime deps). Keep the existing hand-rolled serde JSON path untouched. The semantic DD-span→OTLP mapping runs once and produces the serde types; a mechanical `From<&serde_types>` converter produces the prost types for protobuf. The exporter selects encoder + content-type from `OtlpProtocol`. dd-trace-py gains a `set_otlp_protocol` binding and passes its already-parsed `TRACES_PROTOCOL` through.

**Tech Stack:** Rust (prost 0.14, prost-build, protoc-bin-vendored, serde_json, httpmock), C FFI (cbindgen), Python/PyO3 (setuptools-rust), system-tests, sdk-backend-verify.

**Spec:** `docs/superpowers/specs/2026-06-12-otlp-http-protobuf-export-design.md`

**Refinement vs spec:** The spec proposed gating the protobuf encoder behind an `otlp-protobuf` cargo feature. During planning we confirmed the generated OTLP types live in `libdd-trace-protobuf` (already a non-optional dep of `libdd-trace-utils`) and, matching the existing OTLP common/resource pattern, are compiled unconditionally. A feature gate would only guard the small converter module for negligible benefit, so this plan drops the gate (YAGNI). No new runtime dependency is introduced either way.

**Worktrees:**
- libdatadog: `/Users/brian.marks/go/src/github.com/DataDog/libdatadog-otlp-http-protobuf-export` (branch `brian.marks/otlp-http-protobuf-export`) — already created.
- dd-trace-py: create at execution time (Phase 6).

---

## Phase 0 — Footprint spike (go/no-go gate)

### Task 0: Confirm vendored prost types add no heavy dependencies

**Files:** none (investigation).

- [ ] **Step 1: Record the OTel proto version already vendored**

Run:
```bash
cd /Users/brian.marks/go/src/github.com/DataDog/libdatadog-otlp-http-protobuf-export
head -20 libdd-trace-protobuf/src/pb/opentelemetry/proto/common/v1/common.proto
```
Expected: a header/comment indicating the upstream opentelemetry-proto version (e.g. a release tag or proto package version). Note this version — Phase 1 vendors `trace.proto` + `trace_service.proto` from the **same** release for import compatibility.

- [ ] **Step 2: Confirm prost is already the protobuf toolchain (no new runtime crate needed)**

Run:
```bash
grep -n 'prost' libdd-trace-protobuf/Cargo.toml libdd-trace-utils/Cargo.toml
```
Expected: `prost = "0.14.x"` present in both; `prost-build` + `protoc-bin-vendored` present in `libdd-trace-protobuf` under `[build-dependencies]` behind `generate-protobuf`. Conclusion: vendoring adds only generated structs, no new external runtime crate.

- [ ] **Step 3: Gate decision**

If Steps 1–2 hold (they should, per the spec's prior investigation), proceed. If a new heavy crate would be required, STOP and revisit the spec's decision 1.

---

## Phase 1 — Generate OTLP trace + collector prost types (`libdd-trace-protobuf`)

### Task 1: Vendor the OTLP trace + collector protos and generate prost types

**Files:**
- Create: `libdd-trace-protobuf/src/pb/opentelemetry/proto/trace/v1/trace.proto`
- Create: `libdd-trace-protobuf/src/pb/opentelemetry/proto/collector/trace/v1/trace_service.proto`
- Modify: `libdd-trace-protobuf/build.rs` (compile list + license prepend)
- Create (generated, committed): `libdd-trace-protobuf/src/opentelemetry.proto.trace.v1.rs`, `libdd-trace-protobuf/src/opentelemetry.proto.collector.trace.v1.rs`

- [ ] **Step 1: Vendor the two proto files from the matching opentelemetry-proto release**

Use the same release tag noted in Task 0. From the opentelemetry-proto repo, copy verbatim:
- `opentelemetry/proto/trace/v1/trace.proto` → `libdd-trace-protobuf/src/pb/opentelemetry/proto/trace/v1/trace.proto`
- `opentelemetry/proto/collector/trace/v1/trace_service.proto` → `libdd-trace-protobuf/src/pb/opentelemetry/proto/collector/trace/v1/trace_service.proto`

```bash
mkdir -p libdd-trace-protobuf/src/pb/opentelemetry/proto/trace/v1
mkdir -p libdd-trace-protobuf/src/pb/opentelemetry/proto/collector/trace/v1
TAG=<tag-from-task-0>   # e.g. v1.5.0
BASE="https://raw.githubusercontent.com/open-telemetry/opentelemetry-proto/$TAG/opentelemetry/proto"
curl -fsSL "$BASE/trace/v1/trace.proto" -o libdd-trace-protobuf/src/pb/opentelemetry/proto/trace/v1/trace.proto
curl -fsSL "$BASE/collector/trace/v1/trace_service.proto" -o libdd-trace-protobuf/src/pb/opentelemetry/proto/collector/trace/v1/trace_service.proto
```
Expected: both files saved. `trace.proto` imports `opentelemetry/proto/common/v1/common.proto` and `.../resource/v1/resource.proto` (already vendored). `trace_service.proto` imports `.../trace/v1/trace.proto` and defines `ExportTraceServiceRequest`/`ExportTraceServiceResponse`.

- [ ] **Step 2: Add both protos to the compile list in `build.rs`**

In `libdd-trace-protobuf/build.rs`, extend the `compile_protos(&[ ... ], &["src/pb/"])` array (currently ending at `"src/pb/idx/span.proto"`):

```rust
            &[
                "src/pb/agent_payload.proto",
                "src/pb/tracer_payload.proto",
                "src/pb/span.proto",
                "src/pb/stats.proto",
                "src/pb/remoteconfig.proto",
                "src/pb/opentelemetry/proto/common/v1/process_context.proto",
                "src/pb/opentelemetry/proto/trace/v1/trace.proto",
                "src/pb/opentelemetry/proto/collector/trace/v1/trace_service.proto",
                "src/pb/idx/tracer_payload.proto",
                "src/pb/idx/span.proto",
            ],
```

- [ ] **Step 3: Prepend the OTel license header to the new generated files**

In `build.rs`, next to the existing `prepend_to_file(otel_license, ...resource.v1.rs)` / `...common.v1.rs` calls, add:

```rust
    prepend_to_file(
        otel_license,
        &output_path.join("opentelemetry.proto.trace.v1.rs"),
    );
    prepend_to_file(
        otel_license,
        &output_path.join("opentelemetry.proto.collector.trace.v1.rs"),
    );
```

- [ ] **Step 4: Regenerate the committed Rust types**

Run:
```bash
cargo build -p libdd-trace-protobuf --features generate-protobuf
```
Expected: build succeeds; new files `libdd-trace-protobuf/src/opentelemetry.proto.trace.v1.rs` and `libdd-trace-protobuf/src/opentelemetry.proto.collector.trace.v1.rs` appear, and `libdd-trace-protobuf/src/_includes.rs` now references the `opentelemetry::proto::trace::v1` and `opentelemetry::proto::collector::trace::v1` modules.

- [ ] **Step 5: Verify the generated type path compiles and is reachable**

Run:
```bash
cargo build -p libdd-trace-protobuf
```
Then confirm the symbol path with a throwaway check:
```bash
grep -rn "ExportTraceServiceRequest" libdd-trace-protobuf/src/opentelemetry.proto.collector.trace.v1.rs | head
```
Expected: `pub struct ExportTraceServiceRequest` present. Its module path is `libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest`, with span/resource types under `opentelemetry::proto::trace::v1` and `...::common::v1` / `...::resource::v1`.

- [ ] **Step 6: Commit**

```bash
git add libdd-trace-protobuf/src/pb/opentelemetry libdd-trace-protobuf/build.rs \
        libdd-trace-protobuf/src/opentelemetry.proto.trace.v1.rs \
        libdd-trace-protobuf/src/opentelemetry.proto.collector.trace.v1.rs \
        libdd-trace-protobuf/src/_includes.rs
git commit -m "feat(trace-protobuf): vendor + generate OTLP trace/collector prost types"
```

---

## Phase 2 — Converter + protobuf encoder (`libdd-trace-utils::otlp_encoder`)

Module paths below assume the generated types are re-exported as
`libdd_trace_protobuf::opentelemetry::proto::{trace::v1 as otlp_trace, common::v1 as otlp_common, resource::v1 as otlp_resource, collector::trace::v1 as otlp_collector}`. Confirm exact paths from Task 1 Step 5 and adjust the `use` lines if the generated module nesting differs.

### Task 2: serde→prost converter for the OTLP request

**Files:**
- Create: `libdd-trace-utils/src/otlp_encoder/proto_convert.rs`
- Modify: `libdd-trace-utils/src/otlp_encoder/mod.rs` (declare module + re-export encoders)
- Test: inline `#[cfg(test)]` in `proto_convert.rs`

- [ ] **Step 1: Write the failing test for the converter**

Create `libdd-trace-utils/src/otlp_encoder/proto_convert.rs` with only the test module first:

```rust
// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tests {
    use crate::otlp_encoder::{map_traces_to_otlp, OtlpResourceInfo};
    use crate::span::BytesData;
    use crate::span::v04::Span;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;

    #[test]
    fn converts_ids_and_attributes_to_proto() {
        let resource_info = OtlpResourceInfo {
            service: "svc".to_string(),
            ..Default::default()
        };
        let mut span: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128,
            span_id: 0xEEE19B7EC3C1B174,
            parent_id: 0xEEE19B7EC3C1B173,
            name: libdd_tinybytes::BytesString::from_static("op"),
            resource: libdd_tinybytes::BytesString::from_static("res"),
            r#type: libdd_tinybytes::BytesString::from_static("web"),
            start: 1544712660000000000,
            duration: 1000000000,
            error: 0,
            ..Default::default()
        };
        span.metrics
            .insert(libdd_tinybytes::BytesString::from_static("count"), 42.0);

        let serde_req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let proto: ProtoReq = (&serde_req).into();

        let rs = &proto.resource_spans[0];
        let sp = &rs.scope_spans[0].spans[0];
        // trace_id: 16 bytes, big-endian, high 64 bits zero (no _dd.p.tid)
        assert_eq!(
            sp.trace_id,
            vec![0, 0, 0, 0, 0, 0, 0, 0, 0xD2, 0x69, 0xB6, 0x33, 0x81, 0x3F, 0xC6, 0x0C]
        );
        assert_eq!(sp.span_id, vec![0xEE, 0xE1, 0x9B, 0x7E, 0xC3, 0xC1, 0xB1, 0x74]);
        assert_eq!(sp.parent_span_id, vec![0xEE, 0xE1, 0x9B, 0x7E, 0xC3, 0xC1, 0xB1, 0x73]);
        assert_eq!(sp.name, "res");
        assert_eq!(sp.start_time_unix_nano, 1544712660000000000);
        assert_eq!(sp.end_time_unix_nano, 1544712661000000000);
        // count metric -> int attribute
        let count = sp
            .attributes
            .iter()
            .find(|kv| kv.key == "count")
            .expect("count attr");
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value;
        assert!(matches!(count.value.as_ref().unwrap().value, Some(Value::IntValue(42))));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails to compile (no `From` impl yet)**

Run:
```bash
cargo test -p libdd-trace-utils otlp_encoder::proto_convert -- --nocapture
```
Expected: compile error — `From<&ExportTraceServiceRequest>` not implemented / trait bound not satisfied.

- [ ] **Step 3: Implement the converter**

Prepend the implementation above the test module in `proto_convert.rs`. Use the generated module paths confirmed in Task 1 Step 5:

```rust
//! Converts the hand-rolled serde OTLP request (the JSON wire model) into the generated
//! prost types for binary (HTTP/protobuf) export. The semantic DD-span -> OTLP mapping already
//! happened in `mapper.rs`; this is a purely structural translation.

use crate::otlp_encoder::json_types as j;
use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value::Value as ProtoValue, AnyValue as ProtoAnyValue, ArrayValue as ProtoArrayValue,
    InstrumentationScope as ProtoScope, KeyValue as ProtoKeyValue,
};
use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource as ProtoResource;
use libdd_trace_protobuf::opentelemetry::proto::trace::v1::{
    span::{Event as ProtoEvent, Link as ProtoLink},
    status::StatusCode as ProtoStatusCode,
    ResourceSpans as ProtoResourceSpans, ScopeSpans as ProtoScopeSpans, Span as ProtoSpan,
    Status as ProtoStatus,
};

/// Decode a fixed-width lowercase hex string into a byte vector. The mapper always produces
/// well-formed hex of the expected width; on the unexpected event of a malformed value we fall
/// back to an all-zero buffer of `len` bytes rather than panicking (FFI reliability).
fn hex_to_bytes(s: &str, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let bytes = s.as_bytes();
    if bytes.len() == len * 2 {
        let mut i = 0;
        while i < bytes.len() {
            match (hex_nibble(bytes[i]), hex_nibble(bytes[i + 1])) {
                (Some(hi), Some(lo)) => out.push((hi << 4) | lo),
                _ => return vec![0u8; len],
            }
            i += 2;
        }
        out
    } else {
        vec![0u8; len]
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_u64(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

impl From<&j::AnyValue> for ProtoAnyValue {
    fn from(v: &j::AnyValue) -> Self {
        let value = match v {
            j::AnyValue::StringValue(s) => ProtoValue::StringValue(s.clone()),
            j::AnyValue::BoolValue(b) => ProtoValue::BoolValue(*b),
            j::AnyValue::IntValue(i) => ProtoValue::IntValue(*i),
            j::AnyValue::DoubleValue(d) => ProtoValue::DoubleValue(*d),
            j::AnyValue::BytesValue(b) => ProtoValue::BytesValue(b.clone()),
            j::AnyValue::ArrayValue(a) => ProtoValue::ArrayValue(ProtoArrayValue {
                values: a.values.iter().map(ProtoAnyValue::from).collect(),
            }),
        };
        ProtoAnyValue { value: Some(value) }
    }
}

fn kv(k: &j::KeyValue) -> ProtoKeyValue {
    ProtoKeyValue {
        key: k.key.clone(),
        value: Some(ProtoAnyValue::from(&k.value)),
    }
}

impl From<&j::ExportTraceServiceRequest> for ProtoReq {
    fn from(req: &j::ExportTraceServiceRequest) -> Self {
        ProtoReq {
            resource_spans: req.resource_spans.iter().map(resource_spans).collect(),
        }
    }
}

fn resource_spans(rs: &j::ResourceSpans) -> ProtoResourceSpans {
    ProtoResourceSpans {
        resource: rs.resource.as_ref().map(|r| ProtoResource {
            attributes: r.attributes.iter().map(kv).collect(),
            dropped_attributes_count: 0,
        }),
        scope_spans: rs.scope_spans.iter().map(scope_spans).collect(),
        schema_url: String::new(),
    }
}

fn scope_spans(ss: &j::ScopeSpans) -> ProtoScopeSpans {
    ProtoScopeSpans {
        scope: ss.scope.as_ref().map(|s| ProtoScope {
            name: s.name.clone().unwrap_or_default(),
            version: s.version.clone().unwrap_or_default(),
            attributes: Vec::new(),
            dropped_attributes_count: 0,
        }),
        spans: ss.spans.iter().map(span).collect(),
        schema_url: ss.schema_url.clone().unwrap_or_default(),
    }
}

fn span(s: &j::OtlpSpan) -> ProtoSpan {
    ProtoSpan {
        trace_id: hex_to_bytes(&s.trace_id, 16),
        span_id: hex_to_bytes(&s.span_id, 8),
        trace_state: s.trace_state.clone().unwrap_or_default(),
        parent_span_id: s
            .parent_span_id
            .as_ref()
            .map(|p| hex_to_bytes(p, 8))
            .unwrap_or_default(),
        flags: s.flags.unwrap_or(0),
        name: s.name.clone(),
        kind: s.kind,
        start_time_unix_nano: parse_u64(&s.start_time_unix_nano),
        end_time_unix_nano: parse_u64(&s.end_time_unix_nano),
        attributes: s.attributes.iter().map(kv).collect(),
        dropped_attributes_count: s.dropped_attributes_count.unwrap_or(0),
        events: s.events.iter().map(event).collect(),
        dropped_events_count: s.dropped_events_count.unwrap_or(0),
        links: s.links.iter().map(link).collect(),
        dropped_links_count: 0,
        status: Some(ProtoStatus {
            message: s.status.message.clone().unwrap_or_default(),
            code: status_code(s.status.code),
        }),
    }
}

fn status_code(code: i32) -> i32 {
    // Mirror j::status_code constants onto the generated enum's i32 values.
    match code {
        c if c == j::status_code::OK => ProtoStatusCode::Ok as i32,
        c if c == j::status_code::ERROR => ProtoStatusCode::Error as i32,
        _ => ProtoStatusCode::Unset as i32,
    }
}

fn link(l: &j::OtlpSpanLink) -> ProtoLink {
    ProtoLink {
        trace_id: hex_to_bytes(&l.trace_id, 16),
        span_id: hex_to_bytes(&l.span_id, 8),
        trace_state: l.trace_state.clone().unwrap_or_default(),
        attributes: l.attributes.iter().map(kv).collect(),
        dropped_attributes_count: l.dropped_attributes_count.unwrap_or(0),
        flags: 0,
    }
}

fn event(e: &j::OtlpSpanEvent) -> ProtoEvent {
    ProtoEvent {
        time_unix_nano: parse_u64(&e.time_unix_nano),
        name: e.name.clone(),
        attributes: e.attributes.iter().map(kv).collect(),
        dropped_attributes_count: e.dropped_attributes_count.unwrap_or(0),
    }
}
```

> Note: field names/struct shapes above are the standard prost OTLP output, but prost can name nested types differently across versions. After generation (Task 1), open `opentelemetry.proto.trace.v1.rs` and reconcile any field names (`dropped_links_count`, `flags`, the `span::{Event, Link}` / `status::StatusCode` nesting) with the generated source before finishing this task.

- [ ] **Step 4: Declare the module in `mod.rs`**

In `libdd-trace-utils/src/otlp_encoder/mod.rs`, add under the existing `pub mod mapper;`:
```rust
pub mod proto_convert;
```

- [ ] **Step 5: Run the converter test to verify it passes**

Run:
```bash
cargo test -p libdd-trace-utils otlp_encoder::proto_convert -- --nocapture
```
Expected: PASS. If field-name mismatches appear, fix per the note in Step 3, then re-run.

- [ ] **Step 6: Commit**

```bash
git add libdd-trace-utils/src/otlp_encoder/proto_convert.rs libdd-trace-utils/src/otlp_encoder/mod.rs
git commit -m "feat(trace-utils): add serde->prost OTLP converter"
```

### Task 3: Public encoders (`encode_otlp_json`, `encode_otlp_protobuf`) + parity test

**Files:**
- Modify: `libdd-trace-utils/src/otlp_encoder/mod.rs`
- Test: inline `#[cfg(test)]` in `mod.rs`

- [ ] **Step 1: Write the failing parity test**

Add to `libdd-trace-utils/src/otlp_encoder/mod.rs` a test module:

```rust
#[cfg(test)]
mod encode_tests {
    use super::*;
    use crate::span::BytesData;
    use crate::span::v04::Span;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;
    use prost::Message;

    fn sample() -> ExportTraceServiceRequest {
        let resource_info = OtlpResourceInfo { service: "svc".to_string(), ..Default::default() };
        let span: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128,
            span_id: 0xEEE19B7EC3C1B174,
            name: libdd_tinybytes::BytesString::from_static("op"),
            resource: libdd_tinybytes::BytesString::from_static("res"),
            start: 1, duration: 2, ..Default::default()
        };
        map_traces_to_otlp(vec![vec![span]], &resource_info)
    }

    #[test]
    fn json_and_protobuf_carry_same_span() {
        let req = sample();
        let json = encode_otlp_json(&req).unwrap();
        let pb = encode_otlp_protobuf(&req);

        let json_v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let json_name = json_v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"]
            .as_str().unwrap().to_string();

        let proto = ProtoReq::decode(pb.as_slice()).unwrap();
        let proto_name = proto.resource_spans[0].scope_spans[0].spans[0].name.clone();

        assert_eq!(json_name, "res");
        assert_eq!(proto_name, "res");
        // Span id round-trips identically: JSON hex vs proto bytes.
        let json_sid = json_v["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["spanId"]
            .as_str().unwrap().to_string();
        let proto_sid = &proto.resource_spans[0].scope_spans[0].spans[0].span_id;
        assert_eq!(json_sid, hex::encode(proto_sid));
    }
}
```

Add `hex = "0.4"` to `libdd-trace-utils` `[dev-dependencies]` if not already present (used only in tests).

- [ ] **Step 2: Run to verify it fails (encoders not defined)**

Run:
```bash
cargo test -p libdd-trace-utils otlp_encoder::encode_tests -- --nocapture
```
Expected: compile error — `encode_otlp_json` / `encode_otlp_protobuf` not found.

- [ ] **Step 3: Implement the encoders**

Add to `libdd-trace-utils/src/otlp_encoder/mod.rs` (after the `pub use mapper::map_traces_to_otlp;` line):

```rust
use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoExportTraceServiceRequest;
use prost::Message;

pub use json_types::ExportTraceServiceRequest;

/// Serialize an OTLP request to the HTTP/JSON wire format.
pub fn encode_otlp_json(
    req: &ExportTraceServiceRequest,
) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(req)
}

/// Serialize an OTLP request to the HTTP/protobuf wire format.
pub fn encode_otlp_protobuf(req: &ExportTraceServiceRequest) -> Vec<u8> {
    let proto: ProtoExportTraceServiceRequest = req.into();
    proto.encode_to_vec()
}
```

- [ ] **Step 4: Run to verify it passes**

Run:
```bash
cargo test -p libdd-trace-utils otlp_encoder:: -- --nocapture
```
Expected: PASS (parity test + converter test + existing mapper tests all green).

- [ ] **Step 5: Commit**

```bash
git add libdd-trace-utils/src/otlp_encoder/mod.rs libdd-trace-utils/Cargo.toml
git commit -m "feat(trace-utils): add encode_otlp_json/encode_otlp_protobuf"
```

---

## Phase 3 — Protocol selection + dispatch (`libdd-data-pipeline`)

### Task 4: Make `OtlpProtocol` public + `FromStr`

**Files:**
- Modify: `libdd-data-pipeline/src/otlp/config.rs`
- Test: inline `#[cfg(test)]` in `config.rs`

- [ ] **Step 1: Write the failing test**

Add to `libdd-data-pipeline/src/otlp/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn protocol_from_str() {
        assert_eq!(OtlpProtocol::from_str("http/json").unwrap(), OtlpProtocol::HttpJson);
        assert_eq!(OtlpProtocol::from_str("http/protobuf").unwrap(), OtlpProtocol::HttpProtobuf);
        assert_eq!(OtlpProtocol::from_str("grpc").unwrap(), OtlpProtocol::Grpc);
        assert!(OtlpProtocol::from_str("nonsense").is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p libdd-data-pipeline otlp::config -- --nocapture`
Expected: compile error — `from_str` not implemented; `OtlpProtocol` not public.

- [ ] **Step 3: Implement**

In `libdd-data-pipeline/src/otlp/config.rs` change the enum visibility and remove the dead-code allow on `HttpProtobuf`:

```rust
/// OTLP trace export protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OtlpProtocol {
    /// HTTP with JSON body (Content-Type: application/json). Default for HTTP.
    #[default]
    HttpJson,
    /// HTTP with protobuf body (Content-Type: application/x-protobuf).
    HttpProtobuf,
    /// gRPC. (Not supported yet)
    #[allow(dead_code)]
    Grpc,
}

impl std::str::FromStr for OtlpProtocol {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "http/json" => Ok(OtlpProtocol::HttpJson),
            "http/protobuf" => Ok(OtlpProtocol::HttpProtobuf),
            "grpc" => Ok(OtlpProtocol::Grpc),
            other => Err(format!("unknown OTLP protocol: {other}")),
        }
    }
}
```
Also change `protocol: OtlpProtocol` field on `OtlpTraceConfig` from `pub(crate)` to `pub` and drop its `#[allow(dead_code)]`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p libdd-data-pipeline otlp::config -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add libdd-data-pipeline/src/otlp/config.rs
git commit -m "feat(data-pipeline): make OtlpProtocol public with FromStr"
```

### Task 5: Content-type by protocol in the transport

**Files:**
- Modify: `libdd-data-pipeline/src/otlp/exporter.rs`

- [ ] **Step 1: Update `send_otlp_traces_http` to choose content-type from protocol**

In `libdd-data-pipeline/src/otlp/exporter.rs`, rename the `json_body: Vec<u8>` parameter to `body: Vec<u8>`, pass it to `send_with_retry` instead of `json_body`, and replace the hardcoded content-type insert:

```rust
    let content_type = match config.protocol {
        crate::otlp::config::OtlpProtocol::HttpProtobuf => libdd_common::header::APPLICATION_PROTOBUF,
        _ => libdd_common::header::APPLICATION_JSON,
    };
    let mut headers = config.headers.clone();
    headers.insert(http::header::CONTENT_TYPE, content_type);
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p libdd-data-pipeline`
Expected: success (the caller still passes JSON bytes; updated in Task 6).

- [ ] **Step 3: Commit**

```bash
git add libdd-data-pipeline/src/otlp/exporter.rs
git commit -m "feat(data-pipeline): set OTLP content-type from protocol"
```

### Task 6: Encoder dispatch in the send path

**Files:**
- Modify: `libdd-data-pipeline/src/trace_exporter/mod.rs` (`send_otlp_traces_inner`, ~line 548)
- Modify: `libdd-data-pipeline/src/trace_exporter/mod.rs` imports (~line 18)

- [ ] **Step 1: Replace the hardcoded JSON serialization with protocol dispatch**

In `send_otlp_traces_inner`, replace the `serde_json::to_vec(&request)` block with:

```rust
        let request = map_traces_to_otlp(traces, &resource_info);
        let body = match config.protocol {
            OtlpProtocol::HttpJson => {
                libdd_trace_utils::otlp_encoder::encode_otlp_json(&request).map_err(|e| {
                    error!("OTLP JSON serialization error: {e}");
                    TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(e.to_string()))
                })?
            }
            OtlpProtocol::HttpProtobuf => {
                libdd_trace_utils::otlp_encoder::encode_otlp_protobuf(&request)
            }
            OtlpProtocol::Grpc => {
                return Err(TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(
                    "OTLP gRPC export is not supported".to_string(),
                )));
            }
        };
        send_otlp_traces_http(
            &self.capabilities,
            config,
            self.endpoint.test_token.as_deref(),
            body,
        )
        .await?;
```

Add `OtlpProtocol` to the `use crate::otlp::{...}` import line at the top of the file.

- [ ] **Step 2: Verify the workspace builds**

Run: `cargo check -p libdd-data-pipeline`
Expected: success.

- [ ] **Step 3: Add a protobuf export integration test**

Create `libdd-data-pipeline/tests/test_trace_exporter_otlp_protobuf_export.rs`:

```rust
// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod otlp_protobuf_tests {
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_data_pipeline::trace_exporter::TraceExporterBuilder;
    use libdd_trace_utils::test_utils::create_test_json_span;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest;
    use prost::Message;
    use serde_json::json;
    use tokio::task;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn otlp_protobuf_export_sends_decodable_payload() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        let mut mock = server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/v1/traces")
                    .header("content-type", "application/x-protobuf");
                then.status(200).body("");
            })
            .await;

        let endpoint = format!("http://localhost:{}/v1/traces", server.port());
        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporterBuilder::default();
            builder
                .set_otlp_endpoint(&endpoint)
                .set_otlp_protocol(libdd_data_pipeline::otlp::config::OtlpProtocol::HttpProtobuf)
                .set_language("test-lang")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test");
            let exporter = builder.build::<NativeCapabilities>().expect("build");
            let mut span = create_test_json_span(1234, 12342, 12341, 1, false);
            span["name"] = json!("pb_span");
            let data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
            exporter.send(data.as_ref()).expect("send ok");
        })
        .await;
        assert!(task_result.is_ok());
        assert_eq!(mock.calls_async().await, 1);

        // Decode the most recent request body as protobuf to prove wire correctness.
        let received = mock.received_requests_async().await.unwrap();
        let body = &received[0].body;
        let req = ExportTraceServiceRequest::decode(body.as_slice()).expect("valid protobuf");
        let svc = req.resource_spans[0]
            .resource
            .as_ref()
            .unwrap()
            .attributes
            .iter()
            .find(|kv| kv.key == "service.name")
            .unwrap();
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value;
        assert!(matches!(svc.value.as_ref().unwrap().value, Some(Value::StringValue(ref s)) if s == "test"));
        mock.delete();
    }
}
```

> If `received_requests_async` / body access differs in the pinned httpmock version, mirror the body-capture approach already used elsewhere in `libdd-data-pipeline/tests/`. Confirm `set_otlp_protocol` exists on the builder (Task 7) before running — order Task 7 before this step if executing strictly sequentially.

- [ ] **Step 4: Run the new test (after Task 7's builder method exists)**

Run: `cargo nextest run -p libdd-data-pipeline otlp_protobuf`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add libdd-data-pipeline/src/trace_exporter/mod.rs libdd-data-pipeline/tests/test_trace_exporter_otlp_protobuf_export.rs
git commit -m "feat(data-pipeline): dispatch OTLP encoder by protocol + protobuf test"
```

### Task 7: Builder `set_otlp_protocol`

**Files:**
- Modify: `libdd-data-pipeline/src/trace_exporter/builder.rs`

- [ ] **Step 1: Add the builder field + setter + use it in `build`**

In `libdd-data-pipeline/src/trace_exporter/builder.rs`:
- add a field `otlp_protocol: OtlpProtocol` (defaults to `OtlpProtocol::default()` = `HttpJson`) to the builder struct and its `Default`/initialization;
- add the setter near `set_otlp_endpoint`:

```rust
    /// Selects the OTLP export protocol. Accepts `OtlpProtocol::HttpJson` (default) or
    /// `OtlpProtocol::HttpProtobuf`. The host language resolves this from
    /// `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` / `OTEL_EXPORTER_OTLP_PROTOCOL`.
    pub fn set_otlp_protocol(&mut self, protocol: OtlpProtocol) -> &mut Self {
        self.otlp_protocol = protocol;
        self
    }
```
- in the `OtlpTraceConfig { ... }` construction, replace `protocol: OtlpProtocol::HttpJson` with `protocol: self.otlp_protocol`.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p libdd-data-pipeline`
Expected: success.

- [ ] **Step 3: Run Task 6's protobuf integration test now that the setter exists**

Run: `cargo nextest run -p libdd-data-pipeline otlp`
Expected: PASS (both JSON and protobuf OTLP tests).

- [ ] **Step 4: Commit**

```bash
git add libdd-data-pipeline/src/trace_exporter/builder.rs
git commit -m "feat(data-pipeline): add TraceExporterBuilder::set_otlp_protocol"
```

---

## Phase 4 — C FFI (`libdd-data-pipeline-ffi`)

### Task 8: `ddog_trace_exporter_config_set_otlp_protocol`

**Files:**
- Modify: `libdd-data-pipeline-ffi/src/trace_exporter.rs`

- [ ] **Step 1: Add the config field**

In the `TraceExporterConfig` FFI struct (near the `otlp_endpoint: Option<String>` field, ~line 85), add:
```rust
    otlp_protocol: Option<String>,
```

- [ ] **Step 2: Add the setter, modeled on `ddog_trace_exporter_config_set_otlp_endpoint`**

After the existing OTLP endpoint setter (~line 499):

```rust
/// Sets the OTLP export protocol. Accepts the OTel-standard values `http/json` (default) or
/// `http/protobuf`. `grpc` is rejected as not yet supported. The host language is responsible for
/// resolving the value (e.g. `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL`).
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_otlp_protocol(
    config: Option<&mut TraceExporterConfig>,
    protocol: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            let value = match sanitize_string(protocol) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            match value.as_str() {
                "http/json" | "http/protobuf" => {
                    handle.otlp_protocol = Some(value);
                    None
                }
                _ => gen_error!(ErrorCode::InvalidArgument),
            }
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}
```

- [ ] **Step 3: Apply the protocol in the exporter create function**

Where the create fn calls `builder.set_otlp_endpoint(url)` (~line 566), add:
```rust
            if let Some(ref proto) = config.otlp_protocol {
                if let Ok(p) = proto.parse::<libdd_data_pipeline::otlp::config::OtlpProtocol>() {
                    builder.set_otlp_protocol(p);
                }
            }
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p libdd-data-pipeline-ffi`
Expected: success. (Confirm `OtlpProtocol` is re-exported from `libdd_data_pipeline::otlp::config`; if the ffi crate has a narrower re-export, use that path.)

- [ ] **Step 5: Regenerate the C header**

Run:
```bash
cargo build -p libdd-data-pipeline-ffi
```
Then regenerate headers if the repo uses a header build step (check `builder`/`tools`); otherwise confirm the cbindgen-driven header includes `ddog_trace_exporter_config_set_otlp_protocol`.

- [ ] **Step 6: Commit**

```bash
git add libdd-data-pipeline-ffi/src/trace_exporter.rs
git commit -m "feat(data-pipeline-ffi): add ddog_trace_exporter_config_set_otlp_protocol"
```

---

## Phase 5 — libdatadog validation + PR

### Task 9: Full validation gauntlet

**Files:** none (validation).

- [ ] **Step 1: Format**

Run: `cargo +nightly-2026-02-08 fmt --all -- --check`
Expected: no diff. If it fails, run without `--check` and re-commit.

- [ ] **Step 2: Clippy**

Run: `cargo +stable clippy --workspace --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Tests (nextest + doc)**

Run:
```bash
cargo nextest run --workspace --no-fail-fast
cargo nextest run --workspace --all-features --exclude builder --exclude test_spawn_from_lib
cargo test --doc
```
Expected: all pass. (If `tracing_integration_tests::` need Docker, run `-E '!test(tracing_integration_tests::)'` and note it.)

- [ ] **Step 4: FFI examples**

Run: `cargo ffi-test`
Expected: C/C++ examples build + run.

- [ ] **Step 5: License CSV (if Cargo.lock changed)**

Run:
```bash
git diff --name-only origin/main -- Cargo.lock
```
If `Cargo.lock` is listed:
```bash
./scripts/update_license_3rdparty.sh
cargo deny check
git add Cargo.lock LICENSE-3rdparty.csv
git commit -m "chore: update 3rd-party license CSV"
```
Expected: `cargo deny check` clean. (Likely no Cargo.lock change since no new external crates were added.)

- [ ] **Step 6: Apache headers on new files**

Run: `./scripts/reformat_copyright.sh` then `git status`.
Expected: new `.rs` files carry the Apache header; commit any fixes.

### Task 10: Open the libdatadog PR

- [ ] **Step 1: Pre-push review (mandatory)**

Invoke the `/pre-push-review` skill on the diff.

- [ ] **Step 2: Push the branch**

```bash
git push -u origin brian.marks/otlp-http-protobuf-export
```

- [ ] **Step 3: Create the draft PR with the repo template**

Read `.github/pull_request_template.md`, fill all sections, and:
```bash
gh pr create --draft --label "AI Generated" --title "feat(data-pipeline): OTLP HTTP/protobuf trace export" --body-file <filled-template>
```

- [ ] **Step 4: Babysit CI**

Invoke `/dd:pr-babysit` until CI is green (excluding `devflow/mergegate`).

---

## Phase 6 — dd-trace-py wiring + local E2E (Tier 1)

### Task 11: Set up a dd-trace-py worktree pointed at local libdatadog

**Files:**
- Create: `<dd-trace-py-worktree>/src/native/.cargo/config.toml`

- [ ] **Step 1: Create a dd-trace-py worktree on a feature branch**

```bash
cd /Users/brian.marks/dd/dd-trace-py
git fetch origin && git checkout main && git pull origin main
git worktree add ../dd-trace-py-otlp-protobuf -b brian.marks/otlp-http-protobuf-export
```

- [ ] **Step 2: Add the git-keyed cargo patch (NOT crates-io)**

Create `../dd-trace-py-otlp-protobuf/src/native/.cargo/config.toml`:
```toml
[patch."https://github.com/DataDog/libdatadog"]
libdd-data-pipeline  = { path = "/Users/brian.marks/go/src/github.com/DataDog/libdatadog-otlp-http-protobuf-export/libdd-data-pipeline" }
libdd-trace-utils    = { path = "/Users/brian.marks/go/src/github.com/DataDog/libdatadog-otlp-http-protobuf-export/libdd-trace-utils" }
libdd-trace-protobuf = { path = "/Users/brian.marks/go/src/github.com/DataDog/libdatadog-otlp-http-protobuf-export/libdd-trace-protobuf" }
```
> Add a patch line for every libdatadog crate in the modified set. If `cargo` reports an unpatched/duplicated source, add the named crate it points at.

- [ ] **Step 3: Confirm the patch resolves**

```bash
cd ../dd-trace-py-otlp-protobuf/src/native && cargo metadata --format-version 1 >/dev/null && echo OK
```
Expected: `OK` (patch sources resolve).

### Task 12: PyO3 `set_otlp_protocol` binding

**Files:**
- Modify: `<dd-trace-py-worktree>/src/native/data_pipeline/mod.rs` (after `set_otlp_headers`, ~line 189)

- [ ] **Step 1: Add the binding, modeled on `set_otlp_endpoint`**

```rust
    fn set_otlp_protocol(mut slf: PyRefMut<'_, Self>, protocol: &'_ str) -> PyResult<Py<Self>> {
        slf.try_as_mut()?.set_otlp_protocol(
            protocol
                .parse()
                .map_err(|e: String| pyo3::exceptions::PyValueError::new_err(e))?,
        );
        Ok(slf.into())
    }
```
> Import the builder's `OtlpProtocol` if the `.parse()` turbofish needs it: `use libdd_data_pipeline::otlp::config::OtlpProtocol;`. Match the exact `try_as_mut()` accessor used by the neighboring setters.

- [ ] **Step 2: Build the native extension**

```bash
cd /Users/brian.marks/dd/dd-trace-py-otlp-protobuf
python -m venv .venv && . .venv/bin/activate
pip install -e . 2>&1 | tail -20
```
Expected: build succeeds against the patched local libdatadog.

- [ ] **Step 3: Smoke-test the binding from Python**

```bash
python -c "from ddtrace.internal.native import TraceExporterBuilder as B; b=B(); b.set_otlp_protocol('http/protobuf'); print('ok')"
```
Expected: `ok` (no exception). A bad value should raise `ValueError`.

- [ ] **Step 4: Commit (dd-trace-py)**

```bash
cd /Users/brian.marks/dd/dd-trace-py-otlp-protobuf
git add src/native/data_pipeline/mod.rs
git commit -m "feat(native): expose set_otlp_protocol on TraceExporterBuilder"
```

### Task 13: Wire `TRACES_PROTOCOL` through the writer

**Files:**
- Modify: `<dd-trace-py-worktree>/ddtrace/internal/writer/writer.py` (`_create_exporter`, ~line 827)
- Modify: `<dd-trace-py-worktree>/ddtrace/internal/settings/_opentelemetry.py` (comments)

- [ ] **Step 1: Pass the protocol when OTLP is enabled**

In `_create_exporter`, after `builder.set_otlp_endpoint(self._otlp_endpoint)`:
```python
            builder.set_otlp_protocol(otel_config.exporter.TRACES_PROTOCOL)
```

- [ ] **Step 2: Un-stub the comments in `_opentelemetry.py`**

Remove the "TRACES_PROTOCOL is collected for telemetry but not yet used to switch transport" comment and update the `_derive_traces_endpoint` "libdatadog currently only supports http/json" note to reflect protobuf support.

- [ ] **Step 3: Rebuild + commit**

```bash
pip install -e . 2>&1 | tail -5
git add ddtrace/internal/writer/writer.py ddtrace/internal/settings/_opentelemetry.py
git commit -m "feat(otlp): pass OTEL_EXPORTER_OTLP_TRACES_PROTOCOL to the native exporter"
```

### Task 14: Local protobuf-decoding receiver E2E

**Files:**
- Create (scratch, not committed): `/tmp/otlp_recv.py`, `/tmp/otlp_app.py`

- [ ] **Step 1: Write the receiver**

`/tmp/otlp_recv.py`:
```python
from http.server import BaseHTTPRequestHandler, HTTPServer
from opentelemetry.proto.collector.trace.v1.trace_service_pb2 import ExportTraceServiceRequest

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = self.rfile.read(n)
        ct = self.headers.get("content-type", "")
        assert ct == "application/x-protobuf", f"bad content-type: {ct}"
        req = ExportTraceServiceRequest()
        req.ParseFromString(body)  # raises on malformed protobuf
        span = req.resource_spans[0].scope_spans[0].spans[0]
        print("OK decoded:", span.name, "trace_id_len", len(span.trace_id))
        self.send_response(200); self.end_headers(); self.wfile.write(b"")

HTTPServer(("127.0.0.1", 4318), H).serve_forever()
```
Install the proto package in the venv: `pip install opentelemetry-proto`.

- [ ] **Step 2: Write the instrumented app**

`/tmp/otlp_app.py`:
```python
from ddtrace import tracer
with tracer.trace("e2e_protobuf_span", resource="GET /e2e"):
    pass
tracer.flush()
```

- [ ] **Step 3: Run protobuf E2E**

```bash
python /tmp/otlp_recv.py &  # terminal 1
OTEL_TRACES_EXPORTER=otlp \
OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf \
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://127.0.0.1:4318/v1/traces \
python /tmp/otlp_app.py
```
Expected: receiver prints `OK decoded: GET /e2e trace_id_len 16`. Ensure `DD_TRACE_AGENT_PROTOCOL_VERSION` is unset.

- [ ] **Step 4: Run JSON regression E2E**

Re-run with `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/json` and a JSON-aware receiver variant (assert `content-type: application/json`, `json.loads(body)`).
Expected: JSON path still works unchanged.

---

## Phase 7 — system-tests (Tier 2)

### Task 15: Run system-tests OTLP scenario against local builds

**Files:** none (uses `apm-ecosystems:system-tests-local`).

- [ ] **Step 1: Identify the OTLP trace-export scenario**

Invoke `apm-ecosystems:system-tests-local`. In the system-tests checkout, locate the scenario(s) covering OTLP trace export / `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` for Python. Record the scenario name(s).
> This is the one item the spec left open. Resolve it here before running.

- [ ] **Step 2: Build system-tests against the local dd-trace-py (which is built against local libdatadog)**

Follow the skill's flow to point system-tests at the `dd-trace-py-otlp-protobuf` build.

- [ ] **Step 3: Run the OTLP scenario with `http/protobuf`**

Run the identified scenario; assert it passes with protocol set to `http/protobuf`. Capture output.

- [ ] **Step 4: Record results**

Note pass/fail and any scenario gaps in the dd-trace-py PR description.

---

## Phase 8 — sdk-backend-verify (Tier 3)

### Task 16: Full-chain backend verification

**Files:** none (uses `apm-ecosystems:sdk-backend-verify` + the backend-integrated flow in CLAUDE.md).

- [ ] **Step 1: Start an OTLP-capable receiver that forwards to the backend**

Either the DD Agent with OTLP intake enabled on `:4318`, or the OTel Collector with a Datadog exporter. Use the local agent setup from CLAUDE.md (test-org API key from 1Password). Use a unique `DD_SERVICE` per run to avoid the RC/classification cache.

- [ ] **Step 2: Emit protobuf OTLP traffic**

```bash
OTEL_TRACES_EXPORTER=otlp \
OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf \
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://127.0.0.1:4318/v1/traces \
DD_SERVICE=bm-otlp-pb-$(date +%H%M) \
python /tmp/otlp_app.py
```

- [ ] **Step 3: Verify in the backend**

Invoke `apm-ecosystems:sdk-backend-verify` (or the spans search/aggregate APIs in CLAUDE.md) to confirm the spans landed with correct service, resource, and a 128-bit trace_id. Capture the evidence.

- [ ] **Step 4: Record results in the dd-trace-py PR**

---

## Phase 9 — dd-trace-py PR

### Task 17: Open the dd-trace-py PR (depends on a libdatadog release)

- [ ] **Step 1: Add the cargo dependency bump note**

The `src/native/Cargo.toml` git pins stay at the current `rev` until libdatadog ships a release containing Phase 1–4. Document in the PR that the rev bump + removal of the local `.cargo/config.toml` patch is required before merge. Do not commit the local `.cargo/config.toml` patch.

- [ ] **Step 2: Pre-push review + push**

Invoke `/pre-push-review`, then push `brian.marks/otlp-http-protobuf-export`.

- [ ] **Step 3: Create the draft PR with the repo template**

Read dd-trace-py's PR template, fill it (including the Tier 1–3 validation evidence), and:
```bash
gh pr create --draft --label "AI Generated" --title "feat(otlp): select OTLP trace protocol (http/json|http/protobuf)" --body-file <filled-template>
```

- [ ] **Step 4: Babysit CI**

Invoke `/dd:pr-babysit`.

---

## Self-review notes (plan vs spec)

- **Spec coverage:** type vendoring (Task 1), serde→prost converter (Task 2), encoders (Task 3), protocol `FromStr` (Task 4), content-type (Task 5), dispatch (Task 6), builder (Task 7), FFI (Task 8), validation gauntlet (Task 9), libdatadog PR (Task 10), dd-trace-py PyO3 + writer (Tasks 12–13), local E2E (Task 14), system-tests (Task 15), sdk-backend-verify (Task 16), dd-trace-py PR (Task 17). All spec sections covered.
- **Deviation:** dropped the `otlp-protobuf` cargo feature gate (justified in the header — types are unconditionally compiled via vendoring; YAGNI).
- **Known-unknown resolved in plan:** the system-tests scenario name is resolved in Task 15 Step 1 rather than left as a spec TODO.
- **Type consistency:** `OtlpProtocol` (config.rs) used consistently across Tasks 4/6/7/8/12; `encode_otlp_json`/`encode_otlp_protobuf` defined in Task 3 and used in Task 6; `ExportTraceServiceRequest` (serde) vs prost `ExportTraceServiceRequest` disambiguated via aliases.
- **Open verification points flagged inline:** exact generated prost field names (Task 2 Step 3 note), httpmock body-capture API (Task 6 Step 3 note), PyO3 `try_as_mut` accessor (Task 12 Step 1 note).
