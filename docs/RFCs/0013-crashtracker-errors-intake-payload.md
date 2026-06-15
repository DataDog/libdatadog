# RFC 0013: Crashtracker Errors Intake Payload Schema

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in
[IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This RFC specifies the JSON payloads that the crashtracker sends to the
`/api/v2/errorsintake` endpoint. It describes the wire format—field
names, types, presence rules, and value semantics—so that intake
consumers, backend teams, and language integrations have a single
reference for what arrives on the wire.

## Motivation

The crashtracker structured log format (RFC 0011) documents the content
stored locally, but downstream systems must also understand how that
information is serialized for the Errors Intake pipeline. Having a
single reference schema:

- Guarantees consistency between language integrations that link
  `libdatadog`.
- Enables backend and observability teams to validate new fields before
  rollout.
- Clarifies how crashtracker concepts map onto `error-tracking-intake`.

## Scope

This RFC covers the JSON body sent to Errors Intake for:

1. **Crash reports** — full crash payloads with stack traces, thread
   state, and signal details.
2. **Crash pings** — lightweight notifications sent at the start of
   crash processing.

It does **not** redefine the crash info file format (see RFC 0011).

## Transport

| Path | Auth | Notes |
|---|---|---|
| Direct: `https://error-tracking-intake.<site>/api/v2/errorsintake` | `DD-API-KEY` header | Used when direct submission is enabled. |
| Agent proxy: `http(s)://<agent>/evp_proxy/v4/api/v2/errorsintake` | `X-Datadog-EVP-Subdomain: error-tracking-intake` | Default path through the local agent. |
| File: `file://<path>` | — | Test/debug only; writes pretty-printed JSON with `.errors` extension. |

All HTTP requests carry `Content-Type: application/json`.

### Configuration

| Variable | Default | Purpose |
|---|---|---|
| `DD_CRASHTRACKING_ERRORS_INTAKE_ENABLED` | `true` | Set `false` to disable errors intake entirely. |
| `_DD_DIRECT_SUBMISSION_ENABLED` | `false` | When `true` and `DD_API_KEY` is set, bypass the agent and submit directly. |
| `DD_API_KEY` | — | Required for direct submission. |
| `DD_SITE` | `datadoghq.com` | Datadog site; used to build the direct URL. |
| `DD_ERRORS_INTAKE_DD_URL` | — | Overrides the direct-submission base URL (takes priority over `DD_SITE`). |
| `DD_TRACE_AGENT_URL` | — | Highest-priority agent URL. Supports `http://`, `https://`, `unix://`. |
| `DD_AGENT_HOST` | `localhost` | Agent host (used if `DD_TRACE_AGENT_URL` is unset). |
| `DD_TRACE_AGENT_PORT` | `8126` | Agent port. |
| `DD_TRACE_PIPE_NAME` | — | Windows named pipe. |

**Agent endpoint priority:** `DD_TRACE_AGENT_URL` > named pipe
(Windows) > UDS at `/var/run/datadog/apm.socket` (Unix) >
`DD_AGENT_HOST:DD_TRACE_AGENT_PORT` > `localhost:8126`.

---

## Crash Report Payload

### Top-Level Fields

| Field | Type | Presence | Description |
|---|---|---|---|
| `timestamp` | integer | always | UNIX epoch **milliseconds**. Parsed from the crash timestamp (RFC 3339 string); falls back to current time on parse failure. |
| `ddsource` | string | always | `"crashtracker"` |
| `ddtags` | string | always | Comma-separated `key:value` pairs. See [Tag Encoding](#tag-encoding). |
| `error` | object | always | See [error object](#error-object). |
| `os_info` | object | always | See [os_info object](#os_info-object). |
| `sig_info` | object | when signal-based | See [sig_info object](#sig_info-object). |
| `proc_info` | object | when available | See [proc_info object](#proc_info-object). |
| `ucontext` | object | when available | See [ucontext object](#ucontext-object). |
| `files` | object | when non-empty | See [files object](#files-object). |
| `trace_id` | string \| null | always | Reserved for APM correlation. Currently `null`. |

### `error` object

| Field | Type | Presence | Description |
|---|---|---|---|
| `type` | string | always | Error classification. For signal crashes: the signal name (e.g. `"SIGSEGV"`). Otherwise: the error kind (e.g. `"Panic"`, `"UnixSignal"`). |
| `message` | string | optional | Human-readable summary. Resolution order: (1) an explicit message set by the caller, (2) if `sig_info` is present, auto-generated as `"Process terminated with <si_code_human_readable> (<si_signo_human_readable>)"` (e.g. `"Process terminated with SEGV_MAPERR (SIGSEGV)"`), (3) otherwise absent. |
| `stack` | object | optional | Crashing thread stacktrace. Omitted when no frames were captured. See [stack object](#stack-object). |
| `threads` | array | optional | State of all threads at crash time. See [thread entry](#thread-entry). |
| `thread_name` | string | optional | Name of the crashing thread. |
| `is_crash` | boolean | always | `true` for crash reports. |
| `source_type` | string | always | `"Crashtracking"` |
| `experimental` | object | optional | Experimental / unstable data. See [experimental object](#experimental-object). |

> **Note:** The official intake schema defines an `error.fingerprint`
> field for Custom Grouping. Crashtracker currently encodes this value
> in `ddtags` (`fingerprint:<value>`) rather than as a field on the
> error object.

### `stack` object

| Field | Type | Description |
|---|---|---|
| `format` | string | Always `"Datadog Crashtracker 1.0"`. |
| `frames` | array | Ordered stack frames (top of stack first). See [frame fields](#frame-fields). |
| `incomplete` | boolean | `true` if the stack could not be fully captured. |

#### Frame fields

All frame fields are optional; a frame includes whichever were resolved.

| Field | Type | Description |
|---|---|---|
| `ip` | string | Instruction pointer (hex). |
| `sp` | string | Stack pointer (hex). |
| `module_base_address` | string | Base address of the containing module (hex). |
| `symbol_address` | string | Start address of the symbol (hex). |
| `relative_address` | string | Offset within the binary (hex). |
| `path` | string | File path of the binary/shared object. |
| `build_id` | string | Build ID of the binary. |
| `build_id_type` | string | Build ID type (e.g. `"Gnu"`, `"Go"`). |
| `file_type` | string | Binary file type (e.g. `"Elf"`, `"MachO"`). |
| `function` | string | Demangled function/symbol name. |
| `mangled_name` | string | Raw mangled symbol name. |
| `type_name` | string | Enclosing type/class name. |
| `file` | string | Source file path. |
| `line` | integer | Source line number. |
| `column` | integer | Source column number. |
| `comments` | array[string] | Diagnostic annotations from enrichment. |

#### Thread entry

| Field | Type | Description |
|---|---|---|
| `crashed` | boolean | Whether this is the crashing thread. |
| `name` | string | Thread name. |
| `stack` | object | Same schema as the [stack object](#stack-object). |
| `state` | string (optional) | Thread state (e.g. `"S"` for sleeping). |

### `os_info` object

| Field | Type | Presence | Description |
|---|---|---|---|
| `architecture` | string | required | CPU architecture (e.g. `"x86_64"`, `"arm64"`). Required by the intake for callstack resolution. |
| `bitness` | string | optional | e.g. `"64-bit"` |
| `os_type` | string | optional | e.g. `"Linux"`, `"Mac OS"` |
| `version` | string | optional | OS version/kernel release (e.g. `"6.8.0"`, `"14.7.0"`) |

### `sig_info` object

Present only for Unix signal-based crashes.

| Field | Type | Presence | Description |
|---|---|---|---|
| `si_signo` | integer | required | Signal number (e.g. `11`). |
| `si_signo_human_readable` | string | required | Signal name (e.g. `"SIGSEGV"`). |
| `si_code` | integer | required | Signal code (e.g. `1`). |
| `si_code_human_readable` | string | required | Code name (e.g. `"SEGV_MAPERR"`). |
| `si_addr` | string | optional | Faulting address (hex, e.g. `"0x0000000000001234"`). |

### `proc_info` object

| Field | Type | Presence | Description |
|---|---|---|---|
| `pid` | integer | required | Process ID. |
| `tid` | integer | optional | Thread ID of the crashing thread. |

### `ucontext` object

CPU register state captured from the signal handler.

| Field | Type | Presence | Description |
|---|---|---|---|
| `arch` | string | required | Architecture (e.g. `"x86_64"`, `"aarch64"`). |
| `registers` | object | required | Map of register name → hex value (e.g. `{"rip": "0x00007f..."}`). |
| `raw` | string | optional | Full debug-formatted ucontext preserving FPU/signal-mask state. |

### `files` object

A map of `filename → array[string]` attaching auxiliary file contents
captured at crash time (e.g. `/proc/self/maps`). Omitted when empty.

### `experimental` object

| Field | Type | Description |
|---|---|---|
| `additional_tags` | array[string] | Extra `key:value` tag strings. |
| `runtime_stack` | object (optional) | Language-runtime-level stack. Contains `format` (string), `frames` (array), and/or `stacktrace_string` (string). |

---

## Tag Encoding

The `ddtags` string is a comma-separated list of `key:value` pairs
assembled in the following order. Consumers MUST tolerate new tags and
ordering changes.

### 1. Service tags (always present)

| Tag | Presence | Description |
|---|---|---|
| `service:<name>` | always | Defaults to `"unknown"` if not configured. |
| `env:<env>` | optional | |
| `version:<version>` | optional | Service version. |

### 2. Runtime tags

| Tag | Presence | Description |
|---|---|---|
| `language_name:<lang>` | should be present | Identifies the runtime for Error Tracking grouping. Values SHOULD follow the [LanguageDetection naming convention](https://github.com/DataDog/logs-backend/blob/122abe4e9cef1b76cffccb2eb6fa10607fcc4c87/domains/event-platform/libs/processing/processing-common/src/main/java/com/dd/logs/processing/processors/errortracking/LanguageDetection.java#L95-L113). |
| `language_version:<version>` | optional | |
| `tracer_version:<version>` | optional | |

### 3. Crash info tags

| Tag | Presence | Description |
|---|---|---|
| `data_schema_version:<semver>` | always | Schema version of the crash data (e.g. `"1.8"`). |
| `fingerprint:<value>` | optional | Custom grouping fingerprint. |
| `incomplete:<bool>` | always | Whether the crash info is incomplete. |
| `is_crash:<bool>` | always | `true` for crashes, `false` for pings. |
| `uuid:<uuid>` | always | Unique crash identifier. |
| `<counter>:<int>` | per counter | One tag per counter entry (e.g. `collecting_sample:1`). |

### 4. Signal tags (when `sig_info` is present)

| Tag | Presence | Description |
|---|---|---|
| `si_addr:<hex>` | optional | Faulting address. |
| `si_code:<int>` | always | |
| `si_code_human_readable:<name>` | always | |
| `si_signo:<int>` | always | |
| `si_signo_human_readable:<name>` | always | |

### 5. Platform tag (always present)

| Tag | Description |
|---|---|
| `runtime_platform:<target_triple>` | Compilation target (e.g. `x86_64-unknown-linux-gnu`). |

No escaping is performed on tag values beyond standard JSON string
encoding of the `ddtags` field itself. Receivers MUST accept unknown
tags.

---

## Crash Ping Payload

A crash ping is a lightweight notification sent at the start of crash
processing, before the full report is ready.

### Differences from crash reports

| Field | Crash Report | Crash Ping |
|---|---|---|
| `timestamp` | From crash event | Current time at send |
| `error.is_crash` | `true` | `false` |
| `error.stack` | Present (if frames exist) | Absent |
| `error.threads` | Present (if captured) | Absent |
| `error.thread_name` | Present (if known) | Absent |
| `error.experimental` | Present (if set) | Absent |
| `error.message` | Crash-derived | See [message format](#crash-ping-message-format) |
| `proc_info` | Present (if available) | Absent |
| `ucontext` | Present (if captured) | Absent |
| `files` | Present (if non-empty) | Absent |
| `os_info` | From crash data | Detected at ping send time |

### Crash ping message format

The `error.message` for a crash ping follows one of:

- `"Crashtracker crash ping: crash processing started - Process terminated with <si_code_human_readable> (<si_signo_human_readable>)"`
- `"Crashtracker crash ping: crash processing started - <custom_message>"`
- `"Crashtracker crash ping: crash processing started - Process terminated due to <error_kind>"`

### Crash ping `ddtags`

Crash pings use a reduced tag set:

| Tag | Presence |
|---|---|
| `uuid:<crash_uuid>` | always |
| `is_crash_ping:true` | always |
| `service:<name>` | always |
| `language_name:<lang>` | when available |
| `language_version:<version>` | optional |
| `tracer_version:<version>` | optional |
| `env:<env>` | optional |
| `version:<version>` | optional |
| `si_code_human_readable:<name>` | when signal-based |
| `si_signo:<int>` | when signal-based |
| `si_signo_human_readable:<name>` | when signal-based |
| `runtime_platform:<target_triple>` | always |

Crash pings do **not** include `data_schema_version`, `incomplete`,
`is_crash`, `fingerprint`, or counter tags.

---

## Intake Schema Alignment

The official Errors Intake schema defines fields that crashtracker does
not currently populate:

| Intake Field | Status | Notes |
|---|---|---|
| `error.fingerprint` | Sent via `ddtags` | Not set as a dedicated field on the error object. May be promoted in a future version. |
| `trace_id` | Placeholder | Always `null`. Will be set when crashes are correlated with APM traces. |
| `_dd.error_tracking_standalone.error` | Not sent | Reserved for APM Error Tracking Standalone mode. |

---

## Extensibility & Compatibility

- Consumers MUST ignore unknown top-level fields and unknown tags.
- Producers MAY append new tags to `ddtags` and new fields to the
  payload without a schema version bump.
- The `experimental` object allows adding unstable data without
  affecting the stable schema contract.
- Future versions MAY populate `trace_id` and
  `_dd.error_tracking_standalone.error` when applicable.

---

## Example Crash Report

```json
{
  "timestamp": 1733420830123,
  "ddsource": "crashtracker",
  "ddtags": "service:checkout,env:prod,version:1.4.2,language_name:native,data_schema_version:1.8,incomplete:false,is_crash:true,uuid:f7e2a1b3-4c5d-6e7f-8a9b-0c1d2e3f4a5b,collecting_sample:1,not_profiling:0,si_addr:0x0000000000001234,si_code:1,si_code_human_readable:SEGV_MAPERR,si_signo:11,si_signo_human_readable:SIGSEGV,runtime_platform:x86_64-unknown-linux-gnu",
  "error": {
    "type": "SIGSEGV",
    "message": "Process terminated with SEGV_MAPERR (SIGSEGV)",
    "thread_name": "main",
    "stack": {
      "format": "Datadog Crashtracker 1.0",
      "frames": [
        {
          "ip": "0x00007f7e11d3a2b0",
          "function": "main",
          "file": "app.rs",
          "line": 42,
          "path": "/usr/bin/myapp",
          "relative_address": "0x1a2b0",
          "build_id": "abcdef1234567890",
          "build_id_type": "Gnu"
        }
      ],
      "incomplete": false
    },
    "threads": [
      {
        "crashed": false,
        "name": "worker-1",
        "stack": {
          "format": "Datadog Crashtracker 1.0",
          "frames": [],
          "incomplete": true
        },
        "state": "S"
      }
    ],
    "is_crash": true,
    "source_type": "Crashtracking"
  },
  "os_info": {
    "architecture": "x86_64",
    "bitness": "64-bit",
    "os_type": "Linux",
    "version": "6.8.0"
  },
  "sig_info": {
    "si_addr": "0x0000000000001234",
    "si_code": 1,
    "si_code_human_readable": "SEGV_MAPERR",
    "si_signo": 11,
    "si_signo_human_readable": "SIGSEGV"
  },
  "proc_info": {
    "pid": 12345
  },
  "ucontext": {
    "arch": "x86_64",
    "registers": {
      "rip": "0x00007f7e11d3a2b0",
      "rsp": "0x00007ffee3b4c8a0",
      "rbp": "0x00007ffee3b4c910"
    }
  },
  "files": {
    "/proc/self/maps": [
      "55a1b2c3d000-55a1b2c4e000 r-xp 00000000 08:01 12345 /usr/bin/myapp"
    ]
  }
}
```

## Example Crash Ping

```json
{
  "timestamp": 1733420829500,
  "ddsource": "crashtracker",
  "ddtags": "uuid:f7e2a1b3-4c5d-6e7f-8a9b-0c1d2e3f4a5b,is_crash_ping:true,service:checkout,language_name:native,env:prod,version:1.4.2,si_code_human_readable:SEGV_MAPERR,si_signo:11,si_signo_human_readable:SIGSEGV,runtime_platform:x86_64-unknown-linux-gnu",
  "error": {
    "type": "SIGSEGV",
    "message": "Crashtracker crash ping: crash processing started - Process terminated with SEGV_MAPERR (SIGSEGV)",
    "is_crash": false,
    "source_type": "Crashtracking"
  },
  "os_info": {
    "architecture": "x86_64",
    "bitness": "64-bit",
    "os_type": "Linux",
    "version": "6.8.0"
  },
  "sig_info": {
    "si_addr": "0x0000000000001234",
    "si_code": 1,
    "si_code_human_readable": "SEGV_MAPERR",
    "si_signo": 11,
    "si_signo_human_readable": "SIGSEGV"
  }
}
```
