# libdd-signal-safe-http-client

`libdd-signal-safe-http-client` is a `no_std`-first HTTP/1.1 request emitter for Datadog payloads that may need to leave an async signal handler.

The default build has no allocation, DNS, socket, thread, lock, TLS, or runtime dependency. It formats complete HTTP requests into caller-owned sinks. Signal safety for the actual submission depends on the sink implementation; the default crate only guarantees that request construction and emission do not allocate or call the OS.

Feature flags:

| Feature | Default | Purpose |
| --- | --- | --- |
| `alloc` | no | Enables owned request buffers such as `Request::to_vec`. |
| `std` | no | Enables standard library support and implies `alloc`. |
| `libc-dns` | no | Enables libc `getaddrinfo` resolver convenience for non-signal-handler use. |

The telemetry helper emits `POST /telemetry/proxy/api/v2/apmtelemetry` with the Datadog telemetry metrics headers for `generate-metrics` payloads.
