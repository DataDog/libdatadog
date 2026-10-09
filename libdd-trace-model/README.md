# libdd-trace-model

Shared data model for Datadog trace data.

## Overview

`libdd-trace-model` defines the building blocks that trace processing code (encoders, decoders,
sampling, obfuscation, stats) can share regardless of how a span is stored in memory. It has no
dependencies: storage crates implement its traits for their own types.

## Contents

- **`TraceText` / `TraceBytes`**: the string and byte types a trace can be stored with (e.g.
  `String`, `Arc<str>`, `Cow<str>`, or `libdd_tinybytes::BytesString` through tinybytes'
  `trace-model` feature).
- **`AttributeValue`**: a typed attribute value (string, bool, int, float, bytes, array, or nested
  key-value list), generic over its storage through **`ValueTypes`**.
- **`ValueMap` / `ValueMapMut`, `ValueArray` / `ValueArrayMut`**: access to the containers backing
  arrays and key-value lists. Map keys are unique: implementations tolerating duplicate entries
  must hide the shadowed ones.
