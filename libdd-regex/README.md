# libdd-regex

Workspace-wide regex engine facade for Datadog libraries.

## Overview

libdatadog crates that need regexes import them through this facade instead of
depending on a regex implementation directly. Which engine backs the facade is
a per-artifact choice, made once through Cargo features, so a whole build can
switch engines without every crate opting in individually.

## Engines

- **`full`** (default): the `regex` crate, with Unicode tables and its
  performance optimizations.
- **`regex-lite`**: `regex_lite`, which drops Unicode character classes and
  other advanced features for a smaller binary and faster compile times.

Cargo features are additive across a build: enabling `regex-lite` from any
crate switches every `libdd-regex` consumer in that build to `regex_lite`.

**`require-regex-full`** forces the full engine even when `regex-lite` is
enabled somewhere in the same build. Consumers that evaluate user-provided
regexes (libdd-ffe today) enable it to guarantee Unicode support regardless of
the artifact's engine choice.

Crates that relay an artifact-level engine choice to their own dependents
forward the feature:

```toml
[features]
regex-lite = ["libdd-regex/regex-lite"]
```
