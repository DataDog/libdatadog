# libdd-trace-obfuscation

Trace obfuscation library for Datadog.

## Overview

This crate provides trace obfuscation functionality, implementing the same obfuscation logic as the Datadog Agent. It supports obfuscation for:

- SQL queries
- Redis commands
- Memcached commands
- HTTP URLs
- Credit card numbers
- Stack traces

For more details on trace obfuscation, see the [Datadog documentation](https://docs.datadoghq.com/tracing/configure_data_security/?tab=net#trace-obfuscation).

## Agentless policy artifact

The Generated trace obfuscation workflow builds and tests the v0.4 policy in Wasm.
It uploads `trace-obfuscation-js-<revision>` with the Wasm binary, the two Node modules, `manifest.json`, `LICENSE`, and `NOTICE`.

Select the workflow run for the source revision you want to use.
Check `manifest.json` for that revision and verify the policy file digests before vendoring.
Keep `LICENSE` and `NOTICE` with the policy files.
Commit the verified files with the tracer so its build does not depend on the workflow artifact retention period.

Node bundles can use `agentless-policy.cjs` and `agentless-policy-bytes.cjs`. The second file encodes the same Wasm bytes as `agentless_policy.wasm`, so the bundle does not need a sidecar file at runtime.

`agentless_policy.wasm` calls the Rust v0.4 exporter's payload preparation function. It accepts normalized v0.4 MessagePack from the tracer and returns obfuscated agentless JSON. It does not normalize input to v0.4, compress, or send traces.

On Node, load `agentless-policy.cjs`, call the loader once, and use the returned factory to create a policy with configuration and tracer metadata. The policy's `prepare` method returns a copy of the JSON bytes or `undefined` if the guest rejects the input. The loader does not expose the process environment to the guest.

ABI version 1 operates on one batch at a time per instance. Call `input_buffer` with the byte length before writing UTF-8 JSON configuration, UTF-8 JSON metadata, or a batch into linear memory. `configure()` accepts `ObfuscationConfig` JSON. `configure_metadata(client_computed_top_level)` accepts `hostname`, `env`, `language`, `languageVersion`, `tracerVersion`, and `runtimeID` string fields. Configure both before `prepare(client_side_stats)`. Boolean flags are `0` or `1`.

Inputs must be trusted, writer-encoded batches of at most 50 MiB. A zero address or status indicates failure. `prepare` returns the JSON byte length. Read `output_buffer()` after a successful `prepare` with a fresh view of memory. The next allocation invalidates the output. Copy it before reusing the instance. If preparation fails or traps, discard the batch rather than send it under a different policy.

```sh
RUSTC=$(rustup which rustc)
export RUSTC PATH="${RUSTC%/*}:$PATH"
rustup target add wasm32-wasip1
CARGO_PROFILE_RELEASE_OPT_LEVEL=3 CARGO_PROFILE_RELEASE_STRIP=symbols \
  cargo rustc --release --locked --target wasm32-wasip1 --crate-type cdylib \
    -p libdd-data-pipeline-core --example agentless_policy
output=target/generated-trace-obfuscation
node libdd-data-pipeline-core/examples/package-agentless-policy.cjs \
  target/wasm32-wasip1/release/examples/agentless_policy.wasm "$output"
AGENTLESS_POLICY_WASM="$output/agentless_policy.wasm" AGENTLESS_POLICY_HOST="$output/agentless-policy.cjs" \
  node --test libdd-data-pipeline-core/tests/agentless-policy.test.cjs
```
