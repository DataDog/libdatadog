# RFC 0006: Crashtracker Structured Log Format (Version 1.1). Adds incomplete stacktraces.

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This document describes version 1.1 of the crashinfo data format.

## Motivation

The `libdatadog` crashtracker detects program crashes.
It automatically collects information relevant to the characterizing and debugging the crash, including stack-traces, the crash-type (e.g. SIGSIGV, SIGBUS, etc) crash, the library version, etc.
In some cases, these stack traces may be incomplete.
This can occur intentionally, when deep traces are truncated for performance reasons, or unintentionally, if stack collection fails, e.g. because the stack is corrupted.
Having an (optional) flag on the stack trace allows the consumer of the crash report to know that frames may be missing from the trace.

## Proposed format

The format is an extension of the [1.0 json schema](0005-crashtracker-structured-log-format.md), with the following changes.
The updated schema is given in Appendix A.
Any field not listed as "Required" is optional.
Consumers MUST accept json with elided optional fields.

### Fields

- `data_schema_version`: **[required]** \*\*[UPDATED]
  A string containing the semver ID of the crashtracker data schema ("1.1" for the current version).

### Stacktraces

A stacktrace consists of

- `format`: **[required]**
  An identifier describing the format of the stack trace.
  Allows for extensibility to support different stack trace formats.
  The format described below is identified using the string "Datadog Crashtracker 1.0"
- `frames`: **[required]**
  An array of `StackFrame`, described below.
  Note that each inlined function gets its own stack frame in this schema.
- `incomplete`: **[optional]** **[NEW]**
  A boolean denoting whether the stacktrace may be missing frames, either due to intentional truncation, or an inability to fully collect a corrupted stack.

## Appendix A: Json Schema

[Available here](artifacts/0006-crashtracker-schema.json)
