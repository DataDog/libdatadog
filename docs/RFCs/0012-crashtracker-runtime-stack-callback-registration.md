# RFC 0012: Crashtracker Runtime Stack Emission

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This RFC describes how the crashtracker emits runtime (language-level) stack information in addition to native backtraces. It documents the registration API exposed to runtimes, the streaming format used by the collector, and how the receiver ingests and stores the data. The goal is to provide a stable contract for language runtimes (Ruby, Python, PHP, etc.) to supply stack details that can be attached to crash reports without changing the core native collection path.

## Motivation

- Native stack traces often lack the runtime-level context needed to debug crashes that originate in managed languages.
- Each language/runtime already knows how to walk its stack; letting it provide frames avoids embedding runtime-specific logic in the collector.

## Goals

- Allow runtimes to register a signal-safe callback that emits either structured frames or a preformatted stacktrace string.
- Stream runtime stack data through the existing crashtracker pipe, bounded by explicit markers, and keep ordering consistent with other crash sections.
- Preserve crashtracker resilience: no heap allocations inside signal context, tolerate missing callbacks, and avoid blocking the main crash path.
- Store runtime stacks in the experimental payload so the format can evolve without bumping the core schema.

## Non‑Goals

- Symbolication or demangling of runtime frames inside the collector.
- Changing the core crashinfo schema beyond the existing experimental `runtime_stack` field.

## Background

During crash handling, the collector streams crash info to an out-of-process receiver. After emitting native metadata, siginfo, counters, spans, traces, and backtraces, the collector optionally emits runtime stacks if a runtime callback is registered. Two variants are supported:

1. **Frame-by-frame**: runtime invokes an `emit_frame` callback for every frame, which the collector serializes as JSON lines between `DD_CRASHTRACK_BEGIN_RUNTIME_STACK_FRAME` / `DD_CRASHTRACK_END_RUNTIME_STACK_FRAME`.
2. **Stacktrace string**: runtime emits a full text stacktrace string, streamed between `DD_CRASHTRACK_BEGIN_RUNTIME_STACK_STRING` / `DD_CRASHTRACK_END_RUNTIME_STACK_STRING`.

The receiver’s line-based state machine recognizes these markers, accumulates frames or string data, and attaches them to `experimental.runtime_stack` in the resulting `CrashInfo`.

## Design

### Registration API (runtime side)

- Runtimes call exactly one of:
  - `register_runtime_frame_callback(RuntimeFrameCallback)`
  - `register_runtime_stacktrace_string_callback(RuntimeStacktraceStringCallback)`
- The registered callback is stored in an atomic pointer; re-registration replaces the previous callback.
- `RuntimeStackFrame` uses byte slices for `function`, `type_name`, and `file` to avoid UTF-8 assumptions and allocations in the signal handler.
- Callbacks must try to be as safe as possible (see [Safety and failure handling](#safety-and-failure-handling)).

### Collector emission (crash side)

- After native backtrace emission, the collector checks `is_runtime_callback_registered()`. If none is present, it skips this step.
- In **frame mode**, the collector writes the frame markers, then invokes the runtime callback with an `emit_frame` function that serializes each frame as JSON and flushes after every frame to avoid losing data if interrupted and to avoid having to preallocate a fixed size buffer to store frames
- In **string mode**, the collector writes the string markers, calls the runtime callback with an `emit_stacktrace_string` function that writes the raw bytes plus a newline, then closes the section.
- Emission is intentionally simple: it only writes to the provided `Write` and avoids allocations. Errors bubble up through `EmitterError` and abort further emission but leave previously written sections intact.

### Receiver ingestion

- The receiver state machine treats runtime stack sections as optional blocks.
- On `BEGIN_RUNTIME_STACK_FRAME`, it collects per-line JSON frames into a `Vec<StackFrame>` and, on the end marker, wraps them as `RuntimeStack { format: "Datadog Runtime Callback 1.0", frames, stacktrace_string: None }`.
- On `BEGIN_RUNTIME_STACK_STRING`, it buffers lines until the end marker, then stores them as `RuntimeStack { format: "Datadog Runtime Callback 1.0", frames: [], stacktrace_string: Some(joined_text) }`.
- Collected runtime stacks are stored under `experimental.runtime_stack`, preserving the experimental contract described in RFC 0007 and the v1.X structured format (RFC 0011).

### Ordering and framing

Runtime stack emission occurs after native stack collection and before the final `DD_CRASHTRACK_DONE` marker to ensure core crash data is prioritized but runtime stacks are still streamed when available. All sections are delimited with BEGIN/END markers defined in `shared::constants`.

### Safety and failure handling

- If no callback is registered, nothing is emitted.
- If the callback produces invalid UTF-8 in frame fields, the receiver converts bytes with `from_utf8_lossy`, ensuring the crash report remains serializable.
- Any write error while emitting runtime stacks stops further emission but leaves already-written crash data for salvage.
- A runtime callback that is not signal-safe can jeopardize crash handling; runtime owners MUST honor the signal-safety contract.
- Runtime stack callbacks may need internal runtime APIs (e.g., CPython/CRuby) that are inherently risky even when written to be as safe as possible.
  - The crashtracker streams core crash data first, so if runtime stack collection crashes again, the core crash report remains preserved.
  - The crashtracker has a one time trigger guard, so there will never by recursive crashes where the crashtracker is invoked more than once.

## Data model

- `RuntimeStack` (experimental): `format: String`, `frames: Vec<StackFrame>`, `stacktrace_string: Option<String>`.
- `RuntimeStackFrame` (collector/runtime side): byte slices for names/files, optional line/column.
- Current format identifier: `"Datadog Runtime Callback 1.0"`.


## Rollout and compatibility

- Feature is optional and gated by runtime registration. Existing users without a runtime callback see no change.
- Stored under `experimental.runtime_stack`; backends should continue to tolerate its absence and ignore unknown fields.
