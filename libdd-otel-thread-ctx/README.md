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

The TLS symbol `otel_thread_ctx_v1` and its TLSDESC accessor are defined
directly in Rust using `global_asm!` and `asm!` (both stable since Rust 1.65 /
1.59). This avoids a C build dependency while guaranteeing the TLSDESC dialect
on both x86-64 and aarch64 as required by the spec.

## Usage

See the crate-level documentation in `src/lib.rs` for examples.
