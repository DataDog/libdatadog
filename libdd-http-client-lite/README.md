# libdd-http-client-lite

Blocking TCP transport and DNS adapters for allocation-free HTTP clients such
as `reqwless`. The optional `rustix-tcp` feature provides a TCP stream using
`rustix` system calls. It implements both `embedded-io` and
`embedded-io-async`. Its async methods block the calling thread; use them only
when the caller can block.

Without a timeout, an unresponsive peer can block indefinitely. Use
`TcpStream::connect_timeout` or `TcpConnector::with_timeout` to bound the
connection attempt and each read and write.

The synchronous `dns::Resolver` trait is the generic DNS interface.
`DnsResolver` maps hostnames to IP address strings through the `OsEnv`
interface. `Environment` provides a borrowed-slice implementation without
allocation. Numeric IP addresses bypass the environment.

The optional `libc_dns` feature exposes `libc_dns::Resolver`, which calls
`getaddrinfo`. libc may allocate, lock, or access global state, so this
resolver is not suitable for signal handlers or async executors.

Examples using a Datadog Agent listening on `127.0.0.1:8126`:

```bash
cargo run -p libdd-http-client-lite \
  --example rustix_sync_client \
  --features std,rustix-tcp

cargo run -p libdd-http-client-lite \
  --example rustix_async_client \
  --features std,rustix-tcp
```

For allocation-free telemetry submission, see `send_metrics` and the
`signal_safe_metrics` example in `libdd-telemetry`.
