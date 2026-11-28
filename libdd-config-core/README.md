# Tracer configuration core

Compile a tracer's effective configuration manifest and resolve it against
the sources in precedence order. The caller prepares release-specific entries
and owns the lifetime of the resolved settings. One compiled schema can
resolve multiple independent snapshots.

~~~rust
use libdd_config_core::{ConfigSchema, ParsePolicy, Resolution, ResolveInputs, TransformRegistry};

let registry = TransformRegistry::with_builtins();
let schema = ConfigSchema::from_path("supported-configurations.json", ParsePolicy::default(), &registry)?;
let Resolution::Complete(config) = schema.resolve(ResolveInputs::default())? else {
    unreachable!("this registry has no host transforms");
};
let enabled = config.get("DD_TRACE_ENABLED").and_then(|setting| setting.value.as_bool());
# Ok::<(), libdd_config_core::Error>(())
~~~

Use `from_bytes` when packaging provides a virtual asset. Version 2
manifests specify types, defaults, aliases, property paths, and programmatic
names. Provide source layers from lowest to highest priority. For each layer,
an explicit canonical environment name takes precedence over aliases. If
multiple aliases are set, the layer can use source order or manifest order.
A spelling with its own definition remains a separate setting so a tracer
can apply cross-setting fallbacks after resolution. If the selected value
fails parsing, regexp validation, or transformation, the previous accepted
value stays in the snapshot.

`allowed` describes a regexp for the complete parsed scalar value. It accepts
strings, booleans, and numbers. The object form
accepts `pattern`, `ignoreCase`, and `validateDefault`. The resolver binds
registered transforms by input kind before startup. Callers can register a
Rust function or a host slot with `TransformRegistry::register`. Host
transforms return a pending resolution containing ordered requests. Feed
back the result for each request in order to finish the snapshot.

A resolved value carries its origin, supplied name, and stable configuration
ID when applicable. Enable source events for telemetry that needs overridden
sources. Sensitive entries stay available in the snapshot and are omitted
from source events. The resolver clears detail from sensitive transform
diagnostics.

When a transform or regexp changes or rejects a parsed value, the source
event includes that parsed value. Use `skip_defaults` when applying a new
layer over defaults already materialized by the caller.

## WASI Preview 1

Build the reactor with:

~~~sh
cargo build --release --target wasm32-wasip1 -p libdd-config-core-wasi
node libdd-config-core/wasi/test/resolve.mjs target/wasm32-wasip1/release/libdd_config_core_wasi.wasm
~~~

The caller preopens the directory that contains the prepared manifest. The
first request can load that guest path and resolve one snapshot together.

~~~json
{
  "version": 1,
  "operation": "initialize",
  "manifest": { "kind": "path", "value": "/config/supported-configurations.json" },
  "inputs": { "layers": [], "report_sources": false }
}
~~~

Use a manifest of kind `json` and put the UTF-8 JSON text in `value` when
the host provides a virtual asset. A `resolve` request reuses the compiled
schema with fresh inputs. Use `resolve_once` with the same request fields
for one snapshot. With `skip_defaults`, it compiles only the rules
referenced by the supplied names and skips default validation. It does
not install a schema for later `resolve` requests.

Each reactor supports one pending resolution at a time. A `pending` reply
includes the ordered host requests. Send a `resume`
request with `outcomes`, or send `cancel` to discard the pending request.
A `resume` with the wrong outcome count or an incompatible value discards
the pending request. Repeat the original request to retry.

Allocate a request buffer and a separate eight-byte result descriptor with
`config_v1_alloc(length)`. Then call
`config_v1_resolve(request_pointer, request_length, descriptor_pointer)`.
With status zero, the descriptor holds the response pointer and length as
two unsigned 32-bit little-endian values. Schema and input errors are JSON
replies with `status: "error"`. A nonzero export status indicates a buffer
or response failure. Read the descriptor from the current memory after the
call, copy the response, then pass all three pointers to `config_v1_free`.
