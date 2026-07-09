# libdd-signal-safe-http-client

`libdd-signal-safe-http-client` is a `no_std`-first HTTP client façade and Datadog telemetry helper layer for [`reqwless`](https://docs.rs/reqwless).

The default build depends on `reqwless` with `default-features = false`; it does not enable allocation, DNS, sockets, threads, locks, TLS, or a runtime. Request construction remains reqwless-based. For consumers that do not want async in their public API, this crate exposes a synchronous `HttpClient` over a preconnected `embedded_io::Read + embedded_io::Write` transport.

Feature flags:

| Feature | Default | Purpose |
| --- | --- | --- |
| `alloc` | no | Enables allocation-backed setup helpers. |
| `std` | no | Reserved for standard library support and implies `alloc`. |
| `libc-dns` | no | Enables libc `getaddrinfo` resolver convenience for non-signal-handler use. |

Use `TelemetryMetricsRequestBuilder` to build the default `generate-metrics` header tuples and the reqwless request, then send it through `HttpClient::send` with a caller-owned response-header buffer. C-facing libraries can wrap file descriptors or callbacks behind the same synchronous embedded-io traits without exposing async across the ABI. If reqwless cannot complete against that synchronous transport, `HttpClient::send` returns `ClientError::WouldBlock`.
