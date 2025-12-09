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
