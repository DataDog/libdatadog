# RFC 0009: Crashtracker Structured Log Format (Version 1.4). Adds stackframe mangled_name field.

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This document describes version 1.4 of the crashinfo data format.

## Motivation

The `libdatadog` crashtracker detects program crashes.
It automatically collects information relevant to characterizing and debugging the crash, including stack-traces, the crash-type (e.g. SIGSIGV, SIGBUS, etc) crash, the library version, etc.
When symbol names in stack traces are demangled for better readability, the original mangled names are lost.
This makes it difficult to debug issues where the mangled name is needed, such as when comparing against compiler-generated symbols or when working with specific ABI formats.

## Proposed format

The format is an extension of the [1.3 json schema](0008-crashtracker-stackframe-comments.md), with the following changes.
The updated schema is given in Appendix A.
Any field not listed as "Required" is optional.
Consumers MUST accept json with elided optional fields.

### Fields

- `data_schema_version`: **[required]** **[UPDATED]**
  A string containing the semver ID of the crashtracker data schema ("1.4" for the current version).

### Stackframes

A stackframe consists of all the fields specified in [1.3 json schema](0008-crashtracker-stackframe-comments.md), with the additional

- `mangled_name`: **[optional]** **[NEW]**
  A string containing the original mangled name of the function, if the function name was demangled.
  This field is only present when the function name has been demangled and the original mangled name differs from the demangled name.

## Appendix A: Json Schema

[Available here](artifacts/0009-crashtracker-schema.json) 