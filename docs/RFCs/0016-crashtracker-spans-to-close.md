# RFC 0016: Crashtracker Spans to Close

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in
[IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

**Date:** November 11, 2026

## Summary

This RFC adds a receiver interface for **spans to close**. Before a
crash, a producer (typically a tracer) describes the spans its process
has open. If the process crashes, the producer's description reaches the
crashtracker receiver inside the crash report. The receiver then:

1. ends each described span at the crash time;
2. marks it as an error caused by the crash and links it to the crash
   report;
3. sends it to the Datadog agent as a v1 trace payload.

The description is a single line of JSON in a new crash report section,
`SPANS_TO_CLOSE`.

## Problem

A span is only sent to the agent once it finishes. When a process
crashes, every span it has open is lost: APM never sees it, and the
trace view shows no sign of the work that was in progress when the
process died.

Long-running spans, such as one covering a job, a worker's lifetime or a
long request, are the ones most likely to be open at a crash, and they are
often the most useful ones to have. Today the crash report and the trace
are disconnected: errors tracking shows the crash, while APM shows
nothing.

The process itself cannot finish these spans. After a crash signal, only
async-signal-safe work is possible: no allocation, no locks, no network
I/O. The crashtracker receiver is a separate, healthy process that
already reads a report from the crashing process and sends it on. It can
finish the spans as well, if it is told what they are.

## Goals

- **Finish open spans at a crash.** Each described span arrives at the
  agent as an ordinary finished span, ending at the crash and marked as
  crashed.
- **Link spans to the crash.** The span carries the same error type and
  message as the crash report sent to errors intake, plus the crash's
  UUID, so the trace view can link to the crash's issue.
- **Generic across producers.** The interface describes spans as plain
  data: IDs, names, timing and flat key/value attributes. It does not
  depend on any producer's internal representation or on an agent
  wire format.
- **Cheap and async-signal-safe for producers.** A producer can prepare
  the description ahead of time, on its normal path, so that at crash
  time it only has to write bytes it already holds.
- **The receiver owns the outcome.** The receiver sets every field that
  depends on the crash (end time, error) and every detail of how the
  spans are sent (format, endpoint, headers). Producers cannot get these
  wrong, and the receiver can change them without changing producers.
- **Never harm the crash report.** A block that cannot be read or sent
  is noted in the report and skipped. The report is still sent, and its
  upload is neither delayed nor lost because of spans to close.

## Non-Goals

- **A collector-side staging API.** How a producer holds its description
  and writes it into the crash report is up to the producer. An FFI or
  collector API for staging blocks MAY be added later. It would emit the
  same section described here.
- **Discovering open spans.** The receiver only finishes what the block
  describes. It never infers spans from the stack trace or other report
  data.
- **Validating span semantics.** The producer is trusted to describe
  complete, correct spans. The receiver only checks what it needs to
  build a valid payload.


## Design

### Overview

```text
 crashing process                   receiver process                      agent
 ────────────────                   ────────────────                      ─────
 (before the crash)
 producer renders a JSON line
 describing its open spans

 (at the crash)
 crash report written to the  ──▶  parse the SPANS_TO_CLOSE block
 receiver, including:               │
   BEGIN_SPANS_TO_CLOSE             ▼
   <JSON line>                     build a v1 payload from it
   END_SPANS_TO_CLOSE               │
                                    ▼
                                   finish every span at the crash
                                    │
                                    ├──▶ upload the crash report     ──▶ errors intake / telemetry
                                    └──▶ POST /v1.0/traces (concurrently) ──▶ agent
```

### The crash report section

Spans to close are a new section of the line-oriented crash report that
the receiver reads on its stdin, alongside the existing
`DD_CRASHTRACK_BEGIN_*` / `DD_CRASHTRACK_END_*` sections:

```text
DD_CRASHTRACK_BEGIN_SPANS_TO_CLOSE
{"version":1,"process":{...},"traces":[...]}
DD_CRASHTRACK_END_SPANS_TO_CLOSE
```

- A block MUST contain exactly one line between its markers: one JSON
  document with no embedded newlines.
- A report MAY carry any number of blocks. Each block is parsed,
  finished and sent independently, and a failure in one does not affect
  the others.
- Blocks MAY also be added to a crash report programmatically through
  `CrashInfoBuilder::with_spans_to_close`.

### Schema (version 1)

```jsonc
{
  "version": 1,                          // REQUIRED; any other value is rejected
  "agent_url": "http://localhost:8126",  // OPTIONAL; http:// or unix://

  "process": {                           // OPTIONAL; every field OPTIONAL, default ""
    "language": "python",
    "language_version": "3.12.3",
    "tracer_version": "1.2.3",
    "runtime_id": "29869766-4991-4107-b7b8-177710c164ac",
    "container_id": "",
    "hostname": "",
    "env": "prod",
    "app_version": "",
    "attributes": { "_dd.tags.process": "entrypoint.name:worker" }
  },

  "traces": [                            // REQUIRED; at least one span across all traces
    {
      "trace_id": "0000000000000000348b2cdfb6268fc4",  // REQUIRED; exactly 32 hex digits
      "priority": 1,                     // OPTIONAL i32 sampling priority
      "sampling_mechanism": 4,           // OPTIONAL u32
      "origin": "",                      // OPTIONAL
      "attributes": {},                  // OPTIONAL trace-level attributes
      "spans": [                         // REQUIRED
        {
          "span_id": "66e44e6f74923f29", // REQUIRED; exactly 16 hex digits
          "parent_id": "0000000000000000", // OPTIONAL; absent or zero means root
          "service": "worker",           // REQUIRED
          "name": "job.run",             // REQUIRED
          "resource": "nightly-export",  // OPTIONAL; defaults to name
          "type": "worker",              // OPTIONAL
          "start_unix_ns": 1794415527420000000, // REQUIRED i64, ns since the epoch
          "kind": "internal",            // OPTIONAL;
          "env": "prod",                 // OPTIONAL
          "version": "",                 // OPTIONAL
          "component": "",               // OPTIONAL
          "attributes": {                // OPTIONAL; flat key/value scalars
            "job.id": "8812",
            "job.attempt": 2,
            "job.progress": 0.73,
            "job.retryable": true,
            "_top_level": 1              // OPTIONAL; see "Top-level spans"
          }
        }
      ]
    }
  ]
}
```

A minimal valid block:

```json
{"version":1,"traces":[{"trace_id":"0000000000000000000000000000002a","spans":[{"span_id":"0000000000000007","service":"svc","name":"op","start_unix_ns":1794415527420000000}]}]}
```

### Parsing rules

- **Required fields:**
  - top level: `version`, `traces`;
  - each trace: `trace_id`, `spans`;
  - each span: `span_id`, `service`, `name`, `start_unix_ns`.

  Every other field is optional and defaults as shown in the schema.
- **IDs** are hex strings of exact length: 32 digits for a trace ID, 16
  for a span or parent ID. Producers SHOULD write lowercase. IDs are
  strings because many JSON writers lose precision on integers above
  2^53.
- **Attribute values** MUST be scalars:

  | JSON value | Attribute |
  |---|---|
  | string | string |
  | `true` / `false` | boolean |
  | integer that fits in `i64` | integer |
  | any other number (`1.0`, `1e3`, above `i64::MAX`) | float |
  | array, object, `null` | the block is rejected |

  Keys are flat (`"job.id"`, never `{"job": {"id": ...}}`). If a key
  repeats, the last value wins.
- **Unknown fields** are ignored at every level, so optional fields MAY
  be added without a version change. A change that alters the meaning of
  an existing field MUST bump `version`.
- **Limits:**
  - A block line MUST be at most 64 KiB. Producers that need more SHOULD
    split their spans across several blocks.
  - A block MUST contain at least one span.
- **Failures:** a block that is empty, has more than one line, exceeds
  the limit, has an unsupported version or fails to parse is dropped.
  The reason is added to the crash report's `log_messages`
  (`Dropped spans to close: …`), and the rest of the report is
  unaffected.

### Receiver processing

#### 1. Crash details

The receiver takes these from the crash report once, and applies them to
every block in it:

- **End time:** the report's timestamp. If it cannot be parsed, the
  receive time is used and a note is added to `log_messages`.
- **Error type and message:** the same values the report sends to errors
  intake ([RFC 0013](0013-crashtracker-errors-intake-payload.md)):
  - the type is the signal name (e.g. `SIGSEGV`), else the error kind;
  - the message is the collector's message, else one naming the signal.

  Both paths use one shared implementation, so a crashed span and its
  errors-tracking issue always agree.
- **Crash UUID:** the report's UUID.

#### 2. Building the payload

Each block becomes one v1 `TracerPayload`, with one `TraceChunk` per
trace and one `Span` per span:

- `process` maps to the payload-level fields;
- trace fields map to the chunk;
- span fields map to the span.

Strings are borrowed from the parsed block rather than copied. Every
attribute is carried over unchanged, with the type given by the parsing
rules.

#### 3. Finishing the spans

The payload is changed only through the shared `libdd-trace-model`
mutation traits (`SpanMut`, `TraceChunkMut`, `AttributesMut`), not
through format-specific code. Every span in the block is treated as
ended by the crash:

| Field | Value |
|---|---|
| `duration` | `crash_time − start`, never negative |
| `error` | `true` |
| `error.type` | the crash's error type |
| `error.message` | the crash's error message. If the report has none, a message the producer staged is kept. |
| `error.is_crash` | `"true"` |
| `dd.crash_tracking.uuid` | the crash report's UUID |
| chunk `priority` | `2` (user keep) when the producer gave none; an explicit priority is respected |
| `_top_level` | `1` on spans the block shows as top-level, unless the producer staged a marker; see [Top-level spans](#top-level-spans) |

`error.is_crash` and `dd.crash_tracking.uuid` are what lets the trace
view link a span to its crash report.

##### Top-level spans

A span is top-level when it is a root, or when its parent belongs to
another service. The agent's trace metrics count top-level spans, and the
request says the receiver computed them (`Datadog-Client-Computed-Top-Level`),
so getting this wrong inflates or loses those metrics.

- **Producer staged a marker** (`_top_level` or `_dd.top_level`): it is
  sent as is, like any other attribute, and the receiver adds nothing.
  The agent reads it with its usual meaning: `1` in either key marks the
  span top-level.
- **Otherwise:** the receiver decides from the spans in the same block,
  and writes `_top_level: 1` on the top-level ones:
  - a root is top-level;
  - a span whose parent is in the block is top-level if the parent's
    service differs;
  - a span whose parent is not in the block is top-level, as for a
    remote (distributed) parent. This is the agent's own rule.

A parent can be missing from a block without being remote, for example
when it was already sent before the crash or is described in another
block. Only the producer knows this, so producers SHOULD stage a
marker on every span whose parent is not in the same block.

The receiver never sets producer-specific attributes. A producer that
wants a domain attribute to reflect the failure (for example
`status=failed`) SHOULD stage it that way, since a block is only ever
used if the process crashed.

#### 4. Choosing the destination

- **`agent_url` given:** spans go to that agent at `/v1.0/traces`.
  The URL MUST be `http://` or `unix://`. Any other scheme (`https://`,
  `file://`, `windows:`, …) is refused while preparing, with a note in
  `log_messages`.
- **`agent_url` absent or empty:** spans go to the agent the crashtracker
  itself reports to (the receiver configuration's endpoint), at
  `/v1.0/traces`. That endpoint's timeout, test token and resolver
  settings carry over.

The fallback only applies when that endpoint is an `http://` or `unix://`
agent. If it sends directly to the intake (it has an API key), writes to
a `file://` path, uses another scheme, or is not configured, a block without `agent_url` is not sent, and a
note is added to `log_messages` (`Spans to close #N not sent: …`).

Producers that send traces to a different agent from the one their crash
reports go to SHOULD set `agent_url`.

#### 5. Sending

- **Preparation happens before the report upload.** Payloads are built
  and finished first, so notes about blocks that cannot be sent are part
  of the uploaded report.
- **The spans and the report are sent concurrently.** Neither waits on
  the other, and neither is lost if the other fails.
- **One request per block:** `POST /v1.0/traces` with a v1 msgpack
  body. No retry.
- **Agents without v1:** an agent that does not serve `/v1.0/traces`
  replies 404. The receiver then sends the same finished spans, encoded
  as v0.4, to `/v0.4/traces`, with the same headers. Both bodies are
  encoded while preparing, and both attempts share one 3-second budget.
- **Failures** (connection error, timeout or any other non-2xx reply) are
  written to the receiver's stderr, since the report is already on its
  way.
- **The receiver writes every header:**

  | Header | Value |
  |---|---|
  | `Content-Type` | `application/msgpack` |
  | `X-Datadog-Trace-Count` | the number of chunks in the payload |
  | `Datadog-Client-Computed-Top-Level` | `yes` |
  | `Datadog-Meta-Lang`, `-Lang-Version`, `-Tracer-Version` | from `process`, when set |
  | `Datadog-Container-ID` | from `process.container_id`, when set |
  | `User-Agent`, entity headers | the receiver's endpoint defaults |

### Spans and traces

- **A span** is one timed operation: IDs, service, name, resource, a start
  time and duration, an error flag and attributes.
- **A trace** is the tree of spans that share a trace ID, linked by their
  parent IDs. It has no end of its own. The agent and backend assemble it
  by trace ID, possibly from many payloads sent at different times.

The v1 payload nests three levels:

```text
TracerPayload      one per request: process-level fields and attributes
└── TraceChunk     one per trace in the request: trace ID, sampling, origin
    └── Span       the spans of that trace that this request carries
```

The receiver finishes **spans**, not traces. A chunk carries only the
spans the block describes. Other spans of the same trace may already have
been sent normally before the crash, and the backend joins them by trace
ID. Spans that are not in the block are never touched.

A span that a producer has already reported in a partial or "running"
state, under the same span ID, is completed by its crashed version, which
replaces the earlier ones.

## Producer Requirements

The receiver's guarantees depend on what producers write. Producers:

- **MUST** emit only blocks their bundled receiver understands. Receivers
  that predate this section do not skip unknown sections: they record
  every line of one in `log_messages`.
- **MUST NOT** describe a span that has already finished normally. It
  would be sent a second time under the same span ID, marked as crashed,
  and could replace the real version. A producer MUST withdraw a span's
  description before, or as part of, finishing that span.
- **MUST NOT** rely on sending an end time, duration, error flag or HTTP
  headers. The receiver sets or overwrites them.
- **SHOULD** give the real sampling priority. If it is left out, the
  receiver keeps the trace (priority 2), which would keep traces the
  sampler had dropped. A producer SHOULD NOT stage spans of traces it has
  already decided to drop.
- **SHOULD** stage `_top_level` or `_dd.top_level` on every span whose
  parent is not in the same block (see [Top-level spans](#top-level-spans)).
- **SHOULD** stage spans in their failed state, with any domain attributes
  that only make sense on failure.

### Emitting at crash time

Writing a block MUST be async-signal-safe wherever the producer's crash
path requires it. The interface is designed so that this is cheap:

- **Render ahead of time.** The JSON line is produced on the normal path,
  where allocation, locking and formatting are allowed. It is refreshed
  when the spans change.
- **Publish without locks.** Keep the rendered bytes in preallocated
  storage, for example two buffers and an atomic index naming the
  current one. Write the idle buffer, then switch the index with release
  ordering. A crash in the middle of an update then still sees the
  previous complete block.
- **Write, don't encode.** At crash time, read the index and `write(2)`
  that buffer, framed by the section markers. Nothing is rendered,
  allocated or locked.

A producer whose crash path copies memory while other threads keep
running (for example a child created with `clone()` without `CLONE_VM`)
can capture a buffer that another thread was updating at that moment.
The result is usually invalid JSON, which the receiver rejects and notes
in the report. Producers that need a stronger guarantee SHOULD
store a checksum or sequence number with each buffer and skip a buffer
that does not match.

## Compatibility

- **Additive.** Reports without the section are handled exactly as
  before. The new `CrashInfo` field holding parsed blocks is never
  serialized, so the crash report schema does not change.
- **Versioned.** The receiver rejects any `version` other than the ones
  it supports. New optional fields MAY be added to version 1.
- **Receiver-side format.** Producers never see the agent wire format, so
  the receiver MAY change the format, endpoint or headers it uses without
  any producer change.

## Security and Privacy

- Spans to close only contain what the producer would have sent in the
  span anyway. The receiver adds the crash's error type and message (which
  already go to errors intake) and the crash UUID.
- The block never becomes part of the crash report sent to errors intake
  or telemetry. Only the spans built from it go to the agent.
- `agent_url` is supplied by the crashing process, which already chooses
  where its traces go. The receiver never attaches an API key to these
  requests: when it falls back to its own endpoint, it refuses one that
  carries an API key.
