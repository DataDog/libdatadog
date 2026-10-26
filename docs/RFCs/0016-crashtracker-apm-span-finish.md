# RFC 0016: Crashtracker APM Span Finish on Crash

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This RFC introduces support in the crashtracker receiver for a new `DD_CRASHTRACK_BEGIN_APM_SPAN` wire protocol block. When a collector sends this block as part of a crash report, the receiver parses the embedded span snapshot and submits it as a finished trace span to the local Datadog Agent via `PUT /v0.4/traces`. This ensures that long-running spans active at crash time are properly closed with an error status in the APM trace view.

## Motivation

If the process crashes while a span is active, the span is never finished and never reaches the Datadog backend. The trace simply disappears.

This RFC closes that gap on the receiver side by parsing an APM span snapshot from the crash protocol and submitting a finished span with:
- Accurate `duration` reflecting the crash time
- `error: 1` marking the span as errored
- `_dd.was_long_running: 1.0` so the Agent treats it as the final version, replacing any earlier partial snapshots

## Design

### Wire Protocol Block

A new block type is recognized by the receiver's state machine parser:

```
DD_CRASHTRACK_BEGIN_APM_SPAN
<json>
DD_CRASHTRACK_END_APM_SPAN
```

The JSON payload has the following schema:

| Field | Type | Description |
|-------|------|-------------|
| `schema_version` | integer | Currently `2`. Receivers MUST ignore blocks with an unrecognized version. |
| `trace_id` | decimal string | Full 128-bit trace ID as a decimal integer. |
| `span_id` | decimal string | The span ID. |
| `parent_id` | decimal string | The parent span ID. |
| `start_unix_ns` | decimal string | Span start time as epoch nanoseconds. |
| `duration_ns` | decimal string | Span duration in nanoseconds, computed up to crash time. Minimum `1`. |
| `error` | integer | Always `1`. |
| `has_sampling_priority` | boolean | Whether `sampling_priority` is meaningful. |
| `sampling_priority` | integer | Trace sampling priority. |
| `service` | string | Service name (max 128 bytes). |
| `name` | string | Operation name (max 128 bytes). |
| `resource` | string | Resource name (max 512 bytes). |
| `type` | string | Span type (max 64 bytes). |

All ID and timestamp fields are encoded as decimal strings to avoid precision loss with 64-bit values in JSON.

If no span was active at crash time, the block is omitted by the collector. The receiver MUST tolerate its absence.

### Receiver Processing

The receiver's `StdinState` enum gains an `ApmSpan` variant. When the parser encounters `DD_CRASHTRACK_BEGIN_APM_SPAN` in the `Waiting` state, it transitions to `ApmSpan` and deserializes the JSON line into an `ApmSpanSnapshot` struct stored on the `CrashInfoBuilder`.

After the crash report is fully parsed and the crashing process is released (pipe dropped), the receiver:

1. Constructs a v0.4 `SpanBytes` from the snapshot:
   - Parses the decimal `trace_id` string into a `u128`. The v0.4 encoder automatically injects `meta["_dd.p.tid"]` when the upper 64 bits are non-zero.
   - Sets `error: 1` and `meta["error.type"] = "crash"`, `meta["error.message"] = "Process crashed during this span"`.
   - Sets `metrics["_dd.was_long_running"] = 1.0`.
   - Copies `_sampling_priority_v1` from the snapshot if present.
2. Submits the span to the local Datadog Agent via `TraceExporter` using `build_async` / `send_trace_chunks_async` (to avoid runtime nesting, since the receiver already runs inside a tokio runtime).
3. Proceeds with the normal crash report upload (errors intake + telemetry).

The span submission is best-effort: failures are logged but do not prevent the crash report from being uploaded.

### Agent URL Resolution

The receiver resolves the Agent URL using the same environment variables inherited from the crashing process:
1. `DD_TRACE_AGENT_URL`
2. `DD_AGENT_URL`
3. `DD_AGENT_HOST` + `DD_TRACE_AGENT_PORT` (default `http://127.0.0.1:8126`)

### CrashInfo Schema Impact

The `apm_span` field is added to `CrashInfo` as an optional, schema-skipped field (`#[schemars(skip)]`). It is **not** part of the crash report JSON uploaded to the errors intake — it exists only to pass data from the protocol parser to the span submission logic within the receiver process.

The crash report JSON schema version is bumped to **1.9**. The `apm_span` field is included in the schema as an optional `ApmSpanSnapshot` object, making the format self-describing for any consumer that inspects the crash report JSON.

## Future Work

- **Multi-span support**: The current design accepts a single APM span block per crash report. A future version could support multiple blocks for capturing the full thread context stack.
- **Tag preservation**: The snapshot schema could be extended with a `meta` map to carry user-set tags from the collector.
- **V1 trace protocol**: The current implementation submits via the v0.4 format. When the Agent fully supports v1.0 traces, the submission should be updated accordingly.
