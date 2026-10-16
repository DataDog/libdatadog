# libdd-http-client-lite

Blocking TCP transport for allocation-free HTTP clients such as `reqwless`.
The optional `rustix-tcp` feature provides a TCP stream using `rustix` system
calls. It implements both `embedded-io` and `embedded-io-async`. Its async
methods block the calling thread; use them only when the caller can block.

Without a timeout, an unresponsive peer can block indefinitely. Use
`TcpStream::connect_timeout` or `TcpConnector::with_timeout` to bound the
connection attempt and each read and write. The caller supplies the address;
this crate does not resolve hostnames.

To try the stream with a Datadog Agent on `127.0.0.1:8126`:

```bash
cargo run -p libdd-http-client-lite \
  --example rustix_sync_client \
  --features std,rustix-tcp
```
