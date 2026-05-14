# libdd-healthplatform

`no_std + alloc` Rust types for Datadog's [`agent-payload` healthplatform protobuf schema][upstream-proto].
Encoding is always available; decoding helpers are gated behind the `decode` cargo feature.

## Regenerating bindings

`src/healthplatform.rs` is checked in. Re-generate it whenever `src/pb/healthplatform.proto` changes:

```sh
cargo build -p libdd-healthplatform --features generate-protobuf
```

The vendored `.proto` records the upstream commit SHA in its header — bump it when re-vendoring.

## Example client

A minimal end-to-end demo that POSTs a `HealthReport` through the trace-agent's `evp_proxy` endpoint
lives at `examples/evp_proxy_send.rs`. Build it with:

```sh
cargo build -p libdd-healthplatform --example evp_proxy_send --features example-client,decode
```

[upstream-proto]: https://github.com/DataDog/agent-payload/blob/master/proto/healthplatform/healthplatform.proto
