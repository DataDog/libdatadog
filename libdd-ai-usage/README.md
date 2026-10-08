# libdd-ai-usage

Usage and cost metric points for LLM provider calls and gateway requests,
following the OpenTelemetry GenAI semantic conventions.

An integration describes one call as a plain observation: provider, models,
duration, the raw token counts and how they were counted, a cost and where it
came from. This crate checks the observation, normalizes its usage, and returns
the metric points with their names, units, instruments and attributes. The
integration does no arithmetic and knows no metric names.

The crate has no I/O, no state, no threads, and reads no environment.

## Profiles

| Profile | Recorded per | Metrics |
| --- | --- | --- |
| `gen_ai.client.provider_attempt@0.1.0` | request to a model provider | `gen_ai.client.inference.duration` (`gen_ai.client.operation.duration` for embeddings), `gen_ai.client.inference.operation.{input,output}_tokens`, `gen_ai.client.inference.usage.{input,output}_tokens`, `gen_ai.client.inference.time_to_first_chunk`, `trajectory.gen_ai.client.inference.usage.server_tool.requests`, `trajectory.gen_ai.client.inference.usage.cost`, `trajectory.gen_ai.client.operation.cost` |
| `trajectory.gen_ai.client.token_breakdown@0.1.0` | request to a model provider | `gen_ai.client.inference.usage.cache_read.input_tokens`, `gen_ai.client.inference.usage.cache_write.input_tokens`, `gen_ai.client.inference.usage.reasoning.output_tokens`, `trajectory.gen_ai.client.inference.usage.uncached.input_tokens` |
| `gen_ai.client.provider_streaming@0.1.0` | streamed request | `gen_ai.client.inference.time_per_output_chunk` |
| `trajectory.gen_ai.gateway.request@0.1.0` | client request to a gateway | `trajectory.gen_ai.gateway.request.{duration,provider_operations,retries,fallbacks,estimated_cost}`, `trajectory.gen_ai.gateway.cache.operations` |

`trajectory.*` names are used only where OpenTelemetry defines nothing.

## Rules

- Input tokens include cached input, and output tokens include reasoning, as
  OpenTelemetry defines them. An observation states whether its raw totals
  already include those parts (`input_basis`, `output_basis`).
- A value that was not observed is omitted, never recorded as zero.
- A token count the provider did not report is marked
  `trajectory.token.source=estimated`.
- Every cost point carries its source (`reported`, `calculated` or
  `estimated`). Costs are counted in integer nano-USD.
- Every point says where the call was observed
  (`trajectory.observation.point`).
- Contradictory input is rejected with a stable `ErrorCode`. Partial or
  inconsistent usage is projected as far as it can be, with `IssueCode`s.

## Usage

```rust
use libdd_ai_usage::{project_result, Json, Profile};

let observation = Json::parse(
    r#"{
        "operation_name": "chat",
        "provider_name": "anthropic",
        "request_model": "claude-sonnet-4-5",
        "duration_seconds": 1.84,
        "streaming": true,
        "input_tokens": 41200,
        "cache_read_input_tokens": 39000,
        "input_basis": "includes_cache",
        "output_tokens": 512,
        "cost_usd": 0.02013,
        "cost_source": "estimated",
        "observation_point": "gateway"
    }"#,
)?;
let projection = project_result(Profile::ProviderAttempt, &observation)?;
for point in &projection.points {
    println!("{} {} {:?}", point.name, point.value, point.attributes);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`with_deployment_attributes` adds the opt-in attributes a deployment declares
(for example `user.id` or `trajectory.team.id`) after projection; resource
attributes such as `service.name` are returned separately.

## Conformance

The rules are those of the Open Trajectory portable metric profiles. This crate
is a port of their Rust reference implementation. `tests/data` holds the shared
conformance cases and the metric registry; `cargo nextest run -p libdd-ai-usage`
runs every case and checks every projected point against the registry.

`examples/vector_driver.rs` is a driver for the conformance tool:

```sh
cargo build -p libdd-ai-usage --example vector_driver
trajectory-conformance run-vectors --suite metrics --driver target/debug/examples/vector_driver
```

A change to a rule is a change to the specification and its cases first, then
to this crate.
