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

## Context records lifecycle

The majority SDKs using either the FFI or `OwnedThreadContext` directly via the
pure Rust API (see Ownership modes section below) **are responsible for
explicitly releasing the memory backing thread records**:

- For the FFI only, this means calling `ddog_otel_thread_ctx_free` on detached
  contexts once they are not needed anymore.

  Detaching a context from the pure Rust API will return a `OwnedThreadContext`
  that will be automatically freed when it goes out of scope, as any normal
  Rust struct. In that case, there's nothing special to do.
- For both the FFI and pure Rust API consumers, **you need to make sure no
  context remains attached (and thus allocated) when the thread exits**.
  Otherwise this will leak several hundred bytes for every thread created and
  destroyed, which can add up quickly.

There are two main approaches to manage the contexts lifecycle described
hereafter.

### Swapping

Some SDKs have their own internal representation of a context as a managed
object (managed here means *managed by a garbage collector*), let's call it
`managedContext:

1. Make a thin managed wrapper of `OwnedThreadContext` (or
   `ThreadContextHandle` for the FFI) to make it visible to the GC, if
   supported. Let's call it `threadContextWrapper`. This wrapper must have a
   finalizer that calls `ddog_otel_ctx_free` when the wrapper is reclaimed.
2. For each `managedContext`, at creation time, attach a fresh
   `threadContextWrapper` that you initialize and fill with the data coming
   from the context.
3. When the context is activated, `attach` the corresponding OTel thread
   context in `threadContextWrapper`. Similarly, when the context is
   deactivated, call `detach`.

Note that in this setting, the context is always attached and detached but
never updated in place.

This model entirely prevents the leak mentioned above. It's ok if a context
remains attached when a thread is exited: what's important is that eventually,
the owning `managedContext` will go out of scope and be reclaimed by the GC,
and the corresponding Rust object will be cleaned up.

Once the contexts are created, repeatedly swapping in and out an existing
context is fast (an atomic write to the TLS slot).

One disadvantage is that it might increase the allocation churn, since there's
one allocation per context (a mitigation could be to implement a thread context
pool).

### In-place update

Some SDKs might not have a `managedContext` equivalent with a well defined
lifetime. Or, for performance reasons, would like to reduce allocation churn.

A second approach is to allocate only one `OwnedThreadContext` per thread and
then always re-use the same context in-place through `update`. This typically
happens when using only the `update` API to publish contexts.

This is where the leaks happen: the context is created and installed implicitly
on the first call to `update` on a fresh thread, but no component takes care of
freeing it when the thread exits.

In this model, the SDK either needs to:

- (**Recommended**) Enable the `thread-exit-autoclean` feature, which will
  piggy back on Rust built-in TLS clean up mechanism to free any context still
  attached when the thread is exited.
- Manually install their own thread hooks to detach and free a potential
  context upon thread exits, typically using pthread, OS or runtime-based
  mechanisms.

The advantage of this approach is to reduce the allocation churn (one context
per thread). The disadvantage is that managing the lifecycle is more tedious,
as demonstrated by the potential leak. Additionally, each context swap requires
to write data to the context record. If a small set of contexts are swapped in
and out repeatedly, it might be more costly than pre-allocating them once and
then swap them as described in the Swapping section above.

## Ownership modes

The crate provides exactly one of two context ownership flavours, selected by
two mutually exclusive features:

- `owned-context` (default): `OwnedThreadContext`, a record owned by a single
  thread, which can be updated in place without reallocating. This is the one
  used by the FFI.
- `shared-context`: `SharedThreadContext`, an `Arc`-backed immutable record
  that can be cloned and attached on several threads. To be consumed by another
  Rust crate, currently dd-trace-rs.

They are exclusive because the thread-local slot is untyped: interleaving owned
and shared contexts would misinterpret the pointer and cause UB. If both features
are enabled, `owned-context` wins and the build script emits a warning.


## TLS

The TLS symbol `otel_thread_ctx_v1` and its TLSDESC accessor are defined
directly in Rust using `global_asm!` and `asm!` (both stable since Rust 1.65 /
1.59). This avoids a C build dependency while guaranteeing the TLSDESC dialect
on both x86-64 and aarch64 as required by the spec.

## Usage

See the crate-level documentation in `src/lib.rs` for examples.
