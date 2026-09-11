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

## Ownership modes

The crate provides exactly one of two context ownership flavours, selected by
two mutually exclusive features:

- `owned-context` (default): `OwnedThreadContext`, a record owned by a single
  thread, which can be updated in place without reallocating. This is the one
  used by the FFI.
- `shared-context`: `SharedThreadContext`, an `Arc`-backed immutable record
  that can be cloned and attached on several threads. To be consumed by another
  Rust crate, Currently dd-trace-rs.

They are exclusive because the thread-local slot is untyped: interleaving owned
and shared contexts would misinterpret the pointer cause UB. If both features
are enabled, `owned-context` wins and the build script emits a warning.

## TLS

The TLS symbol `otel_thread_ctx_v1` and its TLSDESC accessor are defined
directly in Rust using `global_asm!` and `asm!` (both stable since Rust 1.65 /
1.59). This avoids a C build dependency while guaranteeing the TLSDESC dialect
on both x86-64 and aarch64 as required by the spec.

## Usage

See the crate-level documentation in `src/lib.rs` for examples.
