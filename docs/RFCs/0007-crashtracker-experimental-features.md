# RFC 0007: Crashtracker Structured Log Format (Version 1.2). Adds experimental field.

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This document describes version 1.2 of the crashinfo data format.

## Motivation

The `libdatadog` crashtracker detects program crashes.
It automatically collects information relevant to the characterizing and debugging the crash, including stack-traces, the crash-type (e.g. SIGSIGV, SIGBUS, etc) crash, the library version, etc.
In addition to these standard Crashtracker developers may wish to collect data for which there is no existing schema.
Having an experimental section in the data-format allows this data to collected without requiring frequent changes to the data-format for data that may not prove worth collecting in the long term.
Developers SHOULD promote data to a structured field defined with an RFC once its value is established, and the data-format is stabilized.

## Proposed format

The format is an extension of the [1.1 json schema](0006-crashtraker-incomplete-stacktraces.md), (which incorporates the [1.0 json schema](0005-crashtracker-structured-log-format.md)) with the following changes.
The updated schema is given in Appendix A.
Any field not listed as "Required" is optional.
Consumers MUST accept json with elided optional fields.

### Fields

- `data_schema_version`: **[required]** \*\*[UPDATED]
  A string containing the semver ID of the crashtracker data schema ("1.2" for the current version).
- `experimental`: **[optional]** **[NEW]**
  Any valid json object can be used as the value here.
  Note that the object MUST be valid json.
  Consumers of the format SHOULD pass this field along unmodified as the report is processed.

## Appendix A: Json Schema

[Available here](artifacts/0007-crashtracker-schema.json)
