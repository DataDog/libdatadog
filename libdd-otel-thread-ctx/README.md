# libdd-otel-thread-ctx

Publisher side of the [OTel Thread Context OTEP
(#4947)](https://github.com/open-telemetry/opentelemetry-specification/pull/4947)
for Datadog profiling.

## Overview

Allows OpenTelemetry SDKs (tracers) to publish per-thread span context (trace
ID, span ID, and custom attributes) into a specific thread-local variable. An
out-of-process reader such as the eBPF profiler can then discover and read this
data.

Linux only for now.

## TLS

The C shim (`src/tls_shim.c`) is required because `rustc` does not yet support
the TLSDESC TLS dialect required by the spec to export `otel_thread_ctx_v1`.
Since the reader and the writer must agree on the TLS dialect/model, we rely on
the C compiler to emit the right access pattern.

## Usage

See the crate-level documentation in `src/lib.rs` for examples.
