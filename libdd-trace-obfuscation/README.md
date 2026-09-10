# libdd-trace-obfuscation

Trace obfuscation library for Datadog.

## Overview

This crate provides trace obfuscation functionality, implementing the same obfuscation logic as the Datadog Agent. It supports obfuscation for:

- SQL queries
- Redis commands
- Memcached commands
- HTTP URLs
- Credit card numbers
- Stack traces

For more details on trace obfuscation, see the [Datadog documentation](https://docs.datadoghq.com/tracing/configure_data_security/?tab=net#trace-obfuscation).

## Batch obfuscation

Create one `obfuscate::V04Obfuscator` for each batch. Call `obfuscate_span` for every v0.4 span.
The instance reuses the most recent repeated SQL result until the batch ends.
