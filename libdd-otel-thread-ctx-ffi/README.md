# libdd-otel-thread-ctx-ffi

FFI bindings for the OTel thread-level context publisher. Exposes a C API for
attaching, detaching, and updating per-thread OpenTelemetry context records
that external readers (e.g. the eBPF profiler) can discover.

Currently Linux-only (x86-64 and aarch64).

## Context records lifecycle

Please refer to [libdd-otel-thread-ctx's
README](../libdd-otel-thread-ctx/README.md) (the same "Context records
lifecycle" section) for a detailed explanation. TLDR:

- **If you're using the in-place `update` API, enable the
  `thread-exit-autoclean` feature to avoid leaking contexts upon thread exit**. 
- If you're instead attaching/deattaching contexts wrapped in GC-managed
  objects, you're all set. 
