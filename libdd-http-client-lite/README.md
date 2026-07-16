# libdd-http-client-lite

Allocation-free HTTP primitives for caller-provided transports and buffers.
The crate re-exports `reqwless` and does not start a runtime or resolve host
names behind the caller's back.

The optional `rustix-tcp` feature provides a blocking TCP stream implemented
with `rustix` system calls. The stream implements both `embedded-io` and
`embedded-io-async`; its async methods remain blocking and are intended for
constrained call sites without an async runtime.

The synchronous `dns::Resolver` trait is the generic DNS interface.
`DnsResolver` implements it by mapping hostnames to IP address strings through
the `OsEnv` interface. `OsEnv::get` returns an implementation-defined value by
value, allowing an OS implementation to return an owned copy. `Environment`
provides a no-allocation borrowed-slice implementation. Numeric IP addresses
bypass the environment.

The optional `libc_dns` feature exposes `libc_dns::Resolver`, which uses the
platform's blocking `getaddrinfo` implementation through the synchronous
`dns::Resolver` interface. libc may allocate, lock, or access global state, so
this resolver is not suitable for signal handlers or async executors.

Examples using a Datadog Agent listening on `127.0.0.1:8126`:

```bash
cargo run -p libdd-http-client-lite \
  --example rustix_sync_client \
  --features std,rustix-tcp

cargo run -p libdd-http-client-lite \
  --example rustix_async_client \
  --features std,rustix-tcp
```
