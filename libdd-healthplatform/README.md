# libdd-healthplatform

`no_std + alloc` Rust types for Datadog's [`agent-payload` healthplatform protobuf schema][upstream-proto].
Encode and decode are provided by the `prost::Message` trait that every generated type already implements
— bring `prost::Message` into scope on the consumer side and call `encode_to_vec()` / `decode(bytes)`.

## Regenerating bindings

`src/healthplatform.rs` is checked in. Re-generate it whenever `src/pb/healthplatform.proto` changes:

```sh
cargo build -p libdd-healthplatform --features generate-protobuf
```

The vendored `.proto` records the upstream commit SHA in its header — bump it when re-vendoring.

## Example client

A minimal end-to-end demo that POSTs a JSON `HealthReport` through the trace-agent's `evp_proxy` endpoint
lives at `examples/evp_proxy_send.rs`. Build it with:

```sh
cargo build -p libdd-healthplatform --example evp_proxy_send --features example-client
```

[upstream-proto]: https://github.com/DataDog/agent-payload/blob/master/proto/healthplatform/healthplatform.proto
