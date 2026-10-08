# Metric conformance cases

These are the Open Trajectory portable metric conformance cases for the
profiles this crate implements, and the metric registry they are checked
against. They are copied unchanged from the specification; do not edit them
here. Update them by copying a newer specification release, then fix the crate
until `tests/conformance.rs` passes.

| Directory | Profile |
| --- | --- |
| `provider-attempt/` | `gen_ai.client.provider_attempt@0.1.0` |
| `provider-token-breakdown/` | `trajectory.gen_ai.client.token_breakdown@0.1.0` |
| `provider-streaming/` | `gen_ai.client.provider_streaming@0.1.0` |
| `gateway-request/` | `trajectory.gen_ai.gateway.request@0.1.0` |
| `registry.json` | the metric registry, version 0.1.0 |

A valid case holds `profile`, `observation`, optional `deployment_attributes`,
and the `expected_metrics`, `expected_issues` and
`expected_resource_attributes` of the projection. An invalid case holds
`expected_error_code`; its `expected_error` text is a hint and is never
compared.

## Comparing points

1. **Counters are aggregated.** For each metric name and attribute set, a case
   has one Counter point whose value is the sum of that case's increments,
   added exactly and after deployment attributes are applied. A Counter with no
   increments has no point.
2. **Histograms are samples.** Each recorded sample is its own point.
3. **Points compare as a multiset.** An implementation's points and
   `expected_metrics` match when they contain the same points, each with the
   same name, instrument, unit, value, and exact attribute set, the same number
   of times. Order does not matter.

To compare, sort both lists into the canonical order and compare element by
element: by metric name; then by attributes, taken as (key, value) pairs
sorted by key and compared pair by pair, key first, where a list that is a
prefix of another sorts first; then by value. Values compare as JSON numbers,
so `1` and `1.0` are equal.

Cases whose names contain `too-large-for-double` hold the number `1e400`, which
no double holds. The crate's own JSON reader reads it as an infinity, which
every rule treats as above every bound.
