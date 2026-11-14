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

## Export

A `MetricBatch` collects the projections of one export window. Counters are
summed exactly as they are added, and Histogram samples are kept. At a flush the
caller encodes the batch and starts a new one; the batch is a plain value the
caller owns.

- **Datadog API**: `MetricBatch::take_intake` (or `datadog_series` for the
  points of one call) returns the series to submit to the metrics API.
  Counters are Rates over 10 s, and Histograms are Distributions (as
  sketches from `take_intake`), each with `{name}.sum` and `{name}.count`
  Rates. See [Datadog metric types](#datadog-metric-types).
- **DogStatsD**: `MetricBatch::dogstatsd_lines` returns one line per series
  point under the Datadog binding. Names are unchanged. Counters are `c` of
  their total. Histograms are `d` samples, each with `{name}.sum` and
  `{name}.count` counts. `{nanoUSD}` Counters are summed exactly and sent in
  USD. Every series carries `trajectory.profile:{id}/{version}`. Tag values
  are normalized the way Datadog stores them.

  `MetricBatch::dogstatsd_datagrams` (or `pack_dogstatsd` for lines of your
  own) packs them, with the client's constant tags, into datagrams of at most
  `DOGSTATSD_UDP_MAX_PAYLOAD` (1432) or `DOGSTATSD_UDS_MAX_PAYLOAD` (8192)
  bytes; a longer line is a datagram of its own, up to
  `DOGSTATSD_MAX_LINE_BYTES` (8192),
  and only a line past that is dropped and counted.
- **OTLP** (feature `otlp`, on by default): `MetricBatch::encode_otlp` returns an
  `ExportMetricsServiceRequest` in protobuf for an OTLP/HTTP metrics endpoint.
  There is one scope per profile, carrying the attribute `trajectory.profile`. Counters are monotonic
  delta sums, and Histograms are delta explicit-bucket histograms using the
  OpenTelemetry GenAI advisory bucket bounds where the conventions give them.
  OTLP is for backends other than Datadog; see
  [OTLP and Datadog](#otlp-and-datadog) before sending it to Datadog.

Sending the bytes or datagrams is the caller's job.

### Datadog metric types

Datadog keeps one type and one interval per metric and organization, set by
whichever writer submitted it last, and reads every point of the metric with
them: `as_count()` on a RATE multiplies each value by its interval. So every
writer of a metric must state the same type, or the type flips and totals read
up to 10 times too high or too low.

The Datadog Agent stores every DogStatsD count (`c`) as a RATE: the count over
its 10 s flush bucket divided by 10, with an interval of 10. It has no way to
keep one a COUNT. This crate therefore gives every Counter the same type on
every transport:

| Instrument | DogStatsD (through the Agent) | Datadog API | Stored as |
| --- | --- | --- | --- |
| Counter, and the `.sum` and `.count` of a Histogram | `c` of the total | `type: rate` (API v2 type 2), `interval: 10`, value = total / 10 | RATE, interval 10 |
| Histogram sample (durations, per-call tokens and costs) | `d` | `distribution` (a sketch from `take_intake`) | DISTRIBUTION |

`DATADOG_RATE_INTERVAL_SECS` is the 10. An API Rate is the exact sum divided
once, by 10 (and by 10^9 for `{nanoUSD}`), so its value times 10 is the
DogStatsD total to within the last bit. The interval is 10 whatever the time
between takes: `as_count()` still reads each point as the sum since the last
take, and a writer that takes every 10 s also gives the Agent's per-second
values.

For example, 100 tokens sent over DogStatsD and 100 over the API in one 10 s
window are both stored as a Rate of 10 over 10 s, and `as_count()` reads 200.
Were the API series a COUNT of 100, whichever type was written last would be
applied to both points: read as Rates over 10 s, 10 * 10 + 100 * 10 = 1,100; read
as Counts, 10 + 100 = 110.

Consumers submit each series exactly as returned: they must not override its
type, interval or value, set a metric's type in its metadata, or divide again.

### OTLP and Datadog

Datadog stores an OTLP delta monotonic sum as a COUNT, and Datadog's OTLP
intake can make nothing else. The Agent's OTLP receiver (7.80 and later) can
store one as a RATE only when the data point carries the attribute
`datadog.metric.as_type=rate`, its interval-inference feature gate is on and
the export runs every 10 s. This crate does not set that attribute. So a
Counter sent to Datadog over OTLP is stored as a COUNT, which clashes with the
RATE the same metric gets over DogStatsD and the API.

For a Datadog destination, use DogStatsD or the API path above. OTLP is for
other backends. `OtlpBinding::Datadog` (`{nanoUSD}` Counters in USD,
`{id}@{version}` values with `/`) makes the values match the Datadog binding,
but not the type. Use it only for metrics that Datadog receives over OTLP
alone.

## Provider responses

What every integration reads from a provider before it can describe a call is
here too, so that one response gives one observation wherever it was read:

- `parse_provider_response`, `parse_provider_body` and `UsageStream` read the
  usage of a response, a stream event or a whole stream (OpenAI Chat
  Completions and Responses, Anthropic Messages, Gemini `usageMetadata`,
  Google's OpenAI-compatible Gemini endpoint, Bedrock Converse `TokenUsage`,
  or `Auto` for an SDK's normalized usage object) into the observation's usage
  members, with the model and service tier the provider named. Counts not
  reported are `None`; a usage object of zeros (cache counts included) is a
  placeholder, not usage. A stream reports whether its terminal event arrived
  (`message_stop`, `response.completed`, `response.incomplete`, `[DONE]`, a
  Gemini `finishReason`, Bedrock `messageStop`) or it failed in-band, and
  splits lines only at CR and LF. A stream that starts with a NUL byte is an
  AWS event stream: Bedrock ConverseStream's `metadata` event, and the model
  events InvokeModelWithResponseStream wraps in `chunk.bytes`, are read from
  its messages.
- `gen_ai_provider_name` maps a gateway's or SDK's provider name to
  `gen_ai.provider.name` (`bedrock` is `aws.bedrock`, `vertex_ai` is
  `gcp.vertex_ai`, ...).
- `classify_error`, `error_type_for_status` and `error_type_for_name` give
  `error.type` in one vocabulary from an HTTP status, a provider error type or
  an SDK exception class.
- `provider_id_attributes` and `bedrock_arn_account` find the provider account
  and project a call was billed to; `is_person_identifier` and
  `is_deployment_value` keep `user.id` to people and identifiers.

`libdd-ai-usage-ffi` exposes all of it to other languages through a C
interface.

## Conformance

The rules are those of the Open Trajectory portable metric profiles. This crate
is a port of their Rust reference implementation. `tests/data` holds the shared
conformance cases, the metric registry, and the Datadog binding cases;
`cargo nextest run -p libdd-ai-usage` runs every case and checks
every projected point against the registry.

`examples/vector_driver.rs` is a driver for the conformance tool:

```sh
cargo build -p libdd-ai-usage --example vector_driver
trajectory-conformance run-vectors --suite metrics --driver target/debug/examples/vector_driver
```

A change to a rule is a change to the specification and its cases first, then
to this crate.
