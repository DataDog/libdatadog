# libdd-otel-thread-ctx-ffi

FFI bindings for the OTel thread-level context publisher. Exposes a C API for
attaching, detaching, and updating per-thread OpenTelemetry context records
that external readers (e.g. the eBPF profiler) can discover.

Currently Linux-only (x86-64 and aarch64).
