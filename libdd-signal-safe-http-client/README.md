# libdd-signal-safe-http-client

`libdd-signal-safe-http-client` is a `no_std`-first set of Datadog telemetry helpers for [`reqwless`](https://docs.rs/reqwless).

The default build depends on `reqwless` with `default-features = false`; it does not enable allocation, DNS, sockets, threads, locks, TLS, or a runtime. HTTP request construction and emission remain reqwless APIs. This crate only supplies Datadog telemetry paths, header tuples, and request-builder helpers.

Feature flags:

| Feature | Default | Purpose |
| --- | --- | --- |
| `alloc` | no | Enables allocation-backed setup helpers. |
| `std` | no | Reserved for standard library support and implies `alloc`. |
| `libc-dns` | no | Enables libc `getaddrinfo` resolver convenience for non-signal-handler use. |

Use `telemetry_metrics_headers` to build the default `generate-metrics` header tuples, then pass those headers to `agent_telemetry_metrics_request` or `telemetry_metrics_request` to get a `reqwless::request::Request`.
