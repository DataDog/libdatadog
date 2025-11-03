# libdd-trace-normalization

Trace normalization and validation for Datadog APM.

## Overview

`libdd-trace-normalization` provides utilities for normalizing and validating distributed tracing data according to Datadog's requirements.

## Features

- **Span Normalization**: Normalize span names, resources, and metadata
- **Tag Normalization**: Validate and normalize span tags
- **Resource Naming**: Normalize resource names for consistent aggregation
- **Metadata Validation**: Ensure spans meet Datadog backend requirements
- **Sampling Priority**: Handle sampling decisions and priorities
- **Metrics Normalization**: Normalize span metrics

## Normalization Rules

The library applies various normalization rules:
- Truncate long strings to limits
- Normalize whitespace and special characters
- Validate required fields
- Standardize tag formats
- Clean up resource names
- Ensure consistent span types

## Example Usage

```rust
use libdd_trace_normalization::normalizer;

// Normalize a span
// let normalized_span = normalizer::normalize_span(span);
```

