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
- **`Attributes` / `AttributesIter`**: typed reads (`attribute_str`, `attribute_f64`,
  `attribute_i64`) and iteration over the attributes of a span, span event, span link or chunk.
- **`Span`, `SpanEvent`, `SpanLink`, `TraceChunk`**: read access to spans and trace chunks.
  Promoted tags (`span.kind`, `env`, `version`, `component` on spans; `_dd.origin`,
  `_sampling_priority_v1`, `_dd.p.dm` on chunks) live in dedicated fields, never in the
  attributes; `tag_str` looks a tag up by name wherever it is stored.
- **`SpanTags`**: iteration over every tag of a span by name, promoted ones included.
- **`ChunkSpanView`**: a span seen through its enclosing chunk, falling back to the chunk's
  attributes and origin for tags the span doesn't have.
