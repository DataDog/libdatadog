# libdd-http-primitives

`libdd-http-primitives` provides transport-agnostic, `no_std`-first HTTP request and response building blocks based on [`reqwless`](https://docs.rs/reqwless).

It allows callers to construct HTTP requests and write them to an embedded-io transport. The default build depends on `reqwless` with `default-features = false`; it does not enable allocation, DNS, sockets, threads, locks, TLS, or a runtime.

Those properties do not make every use of the crate signal-safe. Callers are responsible for ensuring that their transport and any platform operations they invoke are safe in their execution context.

Feature flags:

| Feature | Default | Purpose |
| --- | --- | --- |
| `alloc` | no | Enables allocation-backed setup helpers. |
| `std` | no | Reserved for standard library support and implies `alloc`. |
| `libc-dns` | no | Enables libc `getaddrinfo` resolver convenience for non-signal-handler use. |

The initial protocol-specific helper, `TelemetryMetricsRequestBuilder`, builds the default `generate-metrics` header tuples and request. Callers then write the request through reqwless using their own transport and response buffer.
