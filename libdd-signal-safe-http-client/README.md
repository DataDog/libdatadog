# libdd-signal-safe-http-client

`libdd-signal-safe-http-client` is a `no_std`-first low-level request API for [`reqwless`](https://docs.rs/reqwless).

It allows callers to construct HTTP requests and write them to an embedded-io transport. The default build depends on `reqwless` with `default-features = false`; it does not enable allocation, DNS, sockets, threads, locks, TLS, or a runtime.

Feature flags:

| Feature | Default | Purpose |
| --- | --- | --- |
| `alloc` | no | Enables allocation-backed setup helpers. |
| `std` | no | Reserved for standard library support and implies `alloc`. |
| `libc-dns` | no | Enables libc `getaddrinfo` resolver convenience for non-signal-handler use. |

Use `TelemetryMetricsRequestBuilder` to build the default `generate-metrics` header tuples and the reqwless request. Callers then write the request through reqwless using their own transport and response buffer.
