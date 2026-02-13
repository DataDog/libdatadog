# RFC 0012: Crashtracker Errors Intake Payload Schema

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in
[IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This RFC specifies the payloads produced by the crashtracker
`ErrorsIntakePayload` (`libdd-crashtracker/src/crash_info/errors_intake.rs`)
and uploaded to the `/api/v2/errorsintake` API (direct) or the
`/evp_proxy/v4/api/v2/errorsintake` agent proxy. It formalizes the
shape, required fields, extensibility expectations, and how crash
metadata is translated into `ddtags`.

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

This RFC covers:

- The JSON payload emitted by `ErrorsIntakePayload::from_crash_info`
  and `ErrorsIntakePayload::from_crash_ping`.
- The mapping from `CrashInfo` to `error`, `ddtags`, and other
  top-level fields.
- Expectations for optional / future fields and forward compatibility.

This RFC does **not** redefine the crash info schema itself—see RFC 0011
for the authoritative crash report format.

## Transport Overview

The payload described here is identical regardless of delivery path:

- **Direct submission:** HTTPS requests to
  `https://error-tracking-intake.<site>/api/v2/errorsintake` with
  `DD-API-KEY`.
- **Agent proxy:** HTTP POST to
  `http(s)://<agent>/evp_proxy/v4/api/v2/errorsintake` with the
  `X-Datadog-EVP-Subdomain: error-tracking-intake` header.

Producers MAY also write payloads to a `file://` endpoint (primarily for
tests) using the same schema.

## Payload Structure

### Top-Level Fields

- `timestamp`: **[required]** UNIX epoch in milliseconds. Derived from
  `CrashInfo.timestamp`
- `ddsource`: **[required]** Always the string `"crashtracker"` to allow
  downstream filtering.
- `ddtags`: **[required]** A comma-separated `key:value` string that
  encodes service metadata, runtime metadata, crash info, counters, and
  signal details (see [Tag Encoding](#tag-encoding)).
- `error`: **[required]** An `ErrorObject` describing the crash details
  (see [Error Object](#error-object)).
- `trace_id`: **[optional]** String trace identifier. Reserved for
  future correlation; currently unset by crashtracker.
- `os_info`: **[required]** Same structure as defined in RFC 0011:
  - `architecture`: **[required]** (e.g. `"arm64"`)
  - `bitness`: **[required]** (e.g. `"64-bit"`)
  - `os_type`: **[required]** (e.g. `"Linux"`)
  - `version`: **[required]** (e.g. `"6.8.0"`)
- `sig_info`: **[optional]** Present for Unix signal-based crashes.
  Reuses the fields from RFC 0011 (`si_addr`, `si_code`, `si_code_human_readable`,
  `si_signo`, `si_signo_human_readable`).

### Error Object

The nested `error` object is serialized from `ErrorObject`:

- `type`: A human-readable error kind. For signal-based
  crashes, this is the signal human-readable name (e.g. `"SIGSEGV"`).
  Otherwise `"Unknown"` unless upstream sets `CrashInfo.error.kind`.
- `message`: **[optional]** Human readable summary. For signals the
  default is `"Process terminated by signal <SIG>"`.
- `stack`: **[optional]** When the crashing thread stack contains at
  least one frame, this field embeds the crash stacktrace as defined in
  RFC 0011 (format `"Datadog Crashtracker 1.0"` plus frames).
- `is_crash`: Boolean. `true` for crash payloads and `false` for crash pings.
- `fingerprint`: **[optional]** Correlates to `CrashInfo.fingerprint`
  for deduplication.
- `source_type`: Always `"Crashtracking"` so downstream
  consumers can distinguish crashtracker-originated data from other
  producers.
- `experimental`: **[optional]** Pass-through copy of
  `CrashInfo.experimental`. MUST contain valid JSON when present to
  allow experimentation without schema churn.

### Tag Encoding

`ddtags` is constructed by concatenating comma-delimited `key:value`
pairs. Consumers SHOULD tolerate new tags and
order changes.

1. **Service tags** (always present):
   - `service:<name>` (defaults to `"unknown"` if missing)
   - `env:<env>` **[optional]**
   - `version:<service_version>` **[optional]**
2. **Runtime tags**:
   - `language_name:<language>` This should always be present, as the Errortracking product uses this tag to identify the runtime of the crashing service. We should use the same naming convention defined in [here](https://github.com/DataDog/logs-backend/blob/122abe4e9cef1b76cffccb2eb6fa10607fcc4c87/domains/event-platform/libs/processing/processing-common/src/main/java/com/dd/logs/processing/processors/errortracking/LanguageDetection.java#L95-L113).
   - `language_version:<version>` *(optional)*
   - `tracer_version:<version>` *(optional)*
3. **Crash info tags** (always present):
   - `data_schema_version:<value>`
   - `fingerprint:<value>` **[optional]**
   - `incomplete:<true|false>`
   - `is_crash:<true|false>`
   - `uuid:<CrashInfo.uuid>`
   - `<counter_name>:<counter_value>` for each entry in `CrashInfo.counters`
4. **Signal tags** *(conditional on `sig_info`)*:
   - `si_addr:<hex>` **[optional]**
   - `si_code:<int>`
   - `si_code_human_readable:<string>`
   - `si_signo:<int>`
   - `si_signo_human_readable:<string>`

Tags are appended as literal strings; no escaping is performed beyond
the standard JSON encoding of the `ddtags` field itself. The receiver
MUST accept unknown tags and preserve existing ones.

## Extensibility & Compatibility

- The schema follows the crash info semver. Because the payload embeds
  `CrashInfo`, additions to crash info fields may appear inside the
  `stack` or `experimental` objects without changing Errors Intake
  expectations.
- Producers MAY add additional top-level fields provided they do not
  conflict with existing keys. Consumers MUST ignore unknown fields.
- Additional tags MAY be appended to `ddtags`. Downstream systems MUST
  be resilient to new `key:value` pairs.
- Future work MAY populate `trace_id` when a crash occurs within a
  traced span; consumers MUST handle both presence and absence.

## Example Payload

```
{
  "timestamp": 1733420830123,
  "ddsource": "crashtracker",
  "ddtags": "service:checkout,env:prod,version:1.4.2,language_name:native,data_schema_version:1.4,incomplete:false,is_crash:true,uuid:f7e2...,collecting_sample:1",
  "error": {
    "type": "SIGSEGV",
    "message": "Process terminated by signal SIGSEGV",
    "stack": {
      "format": "Datadog Crashtracker 1.0",
      "frames": [
        {
          "function": "main",
          "file": "app.rs",
          "line": 42
        }
        // more frames ...
      ]
    },
    "is_crash": true,
    "fingerprint": "sigsegv-main",
    "source_type": "Crashtracking"
  },
  "trace_id": null,
  "os_info": {
    "architecture": "x86_64",
    "bitness": "64-bit",
    "os_type": "Linux",
    "version": "6.8.0"
  },
  "sig_info": {
    "si_code": 1,
    "si_code_human_readable": "SEGV_MAPERR",
    "si_signo": 11,
    "si_signo_human_readable": "SIGSEGV",
    "si_addr": "0x0000000000001234"
  }
}
```

This example omits optional fields such as `experimental`, `span_ids`,
and additional stack metadata for brevity. Refer to RFC 0011 for the
full stack trace schema.

