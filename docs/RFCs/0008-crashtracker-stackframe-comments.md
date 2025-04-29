# RFC 0008: Crashtracker Structured Log Format (Version 1.3). Adds stackframe comments.

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This document describes version 1.3 of the crashinfo data format.

## Motivation

The `libdatadog` crashtracker detects program crashes.
It automatically collects information relevant to the characterizing and debugging the crash, including stack-traces, the crash-type (e.g. SIGSIGV, SIGBUS, etc) crash, the library version, etc.
In some cases, a crashtracker collector may have information related to stackframes that does not fit in the current schema.
For example, if a stackframe failed to symbolicate, the crashtracker implementation may wish to record the reason for the failure to allow debugging.

## Proposed format

The format is an extension of the [1.0 json schema](0005-crashtracker-structured-log-format.md), with the following changes.
The updated schema is given in Appendix A.
Any field not listed as "Required" is optional.
Consumers MUST accept json with elided optional fields.

### Fields

- `data_schema_version`: **[required]** \*\*[UPDATED]
  A string containing the semver ID of the crashtracker data schema ("1.3" for the current version).

### Stackframes

A stackframe consists of all the fields specified in [1.0 json schema](0005-crashtracker-structured-log-format.md), with the additional

- `comments`: **[optional]** **[NEW]**
  An array of Strings, containing comments about the given stackframe.

## Appendix A: Json Schema

[Available here](artifacts/0006-crashtracker-schema.json)
