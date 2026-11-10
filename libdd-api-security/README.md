# libdd-api-security

A structural schema extraction proof of concept for API Security, independent of
AppSec. It is a pure Rust library: no Node.js binding, C FFI, sampling, remote
configuration, regex scanners, threads or global initialization.

## API

- `extract_schema(&serde_json::Value) -> Schema<'_>` borrows property names and
  avoids parsing/copying an existing input DOM. Serialize the result with serde.
- `extract_schema_json(&str) -> Result<String, SchemaError>` streams JSON directly
  into a schema and returns compact schema JSON. It does not construct a full
  input DOM or enable the workspace-banned serde_json `preserve_order` feature.

The schema retains property names, types, array lengths and truncation metadata.
It does not retain scalar values. Property names themselves can contain data;
schema extraction is not a general-purpose redactor.

## Compatibility

The reference is libddwaf's `src/processor/extract_schema.cpp` and
`tests/unit/processor/extract_schema_test.cpp`, inspected at revision
`4b46c924`. The reusable JSON fixtures cover the scanner-free, JSON-representable
subset, with additional tests for boundaries and canonical structural equality.

| Input | Schema |
| --- | --- |
| Null | `[1]` |
| Boolean | `[2]` |
| Signed/unsigned 64-bit integer | `[4]` |
| String | `[8]` |
| Floating point | `[16]` |
| Record | `[{"field": <schema>}]` |
| Array | `[[<distinct element schemas>], {"len": N}]` |

The root starts at depth 18. Containers at the last level have no children;
depth exhaustion alone does not set `truncated`. Arrays examine the first 10
elements before deduplication. Records examine the first 255 entries, skipping
empty keys without replenishing their quota. The JSON entry point uses source
order; the parsed-value API uses the supplied map iteration order (normally
key-sorted), since an existing DOM cannot recover original JSON field order. Oversized
containers add `"truncated": true` to their metadata.

Array lengths describe the supplied value, not a body before any upstream
binding truncation. Length and truncation metadata participate in structural
equality, including for nested arrays. Output record fields and array variants
are deterministic; libddwaf's iteration order is unspecified. Compare schemas
semantically, treating array variants as a set, rather than comparing raw bytes.

JSON has no libddwaf invalid/unknown value (type 0). The JSON entry point counts
all entries, including duplicates, against record limits and retains the first
selected value for a repeated key, matching libddwaf. A pre-parsed DOM may have
already collapsed duplicates. Discarded subtrees are syntax-checked and counted
with serde's IgnoredAny, without descending through the schema visitor. Visited
numbers outside serde_json's supported range return an error. Float
classification follows the parsed number representation (e.g. `1.0` is a float).

## Tests and benchmarks

```sh
cargo check -p libdd-api-security
cargo nextest run -p libdd-api-security
cargo test -p libdd-api-security --doc
cargo +stable clippy -p libdd-api-security --all-targets -- -D warnings
cargo +nightly-2026-07-26 fmt --all -- --check
./scripts/run-ast-grep.sh
cargo bench -p libdd-api-security --bench schema
```

Criterion separates extraction of a pre-parsed value, DOM parsing alone (a
comparison baseline), serialization of a precomputed schema, the full DOM
parse/extract/serialize pipeline, and the streaming JSON API. Fixtures cover
wide records, deep nesting, repeated and heterogeneous arrays, and realistic
bodies. Inputs are generated outside timed loops.

The workspace bench profile optimizes for speed; use
`cargo bench -p libdd-api-security --bench schema --profile release` to also
measure the shipped size-optimized profile. Record the compiler, platform,
profile and revision with results. Extraction-only timings do not include
tracer marshalling, compression, or transport.

## Local benchmark baseline

Measured on 2026-11-09, Apple M3 Max (arm64), macOS 26.7.2, Rust 1.98.1,
using the workspace bench profile (opt-level 3), with this PoC added locally
to workspace revision 1c6a91328. These are Criterion median
latencies from 30 samples per workload, with 200 ms warm-up and 500 ms
measurement time. The DOM pipeline includes parsing, this same engine and
serialization; the streaming API includes parsing and serialization.

| Workload | Extract pre-parsed value | Full DOM pipeline | Streaming JSON API |
| --- | ---: | ---: | ---: |
| Record, 10 fields | 0.27 µs | 1.24 µs | 0.60 µs |
| Record, 1,000 fields | 4.10 µs | 132.70 µs | 31.19 µs |
| Array, 1,000 records | 0.78 µs | 190.24 µs | 54.31 µs |
| Realistic body, 20 items | 0.97 µs | 5.43 µs | 2.35 µs |
| Realistic body, 500 items | 0.99 µs | 94.16 µs | 27.26 µs |

```sh
cargo +stable bench -p libdd-api-security --bench schema -- \
  --sample-size 30 --warm-up-time 0.2 --measurement-time 0.5 --noplot
```

All 50 benchmark cases completed. This is an initial local microbenchmark,
not a production latency guarantee or a cross-engine comparison. Allocation
counts, peak memory and host-runtime overhead have not been measured.
The near-constant extraction cost for repeated records follows the first-ten
array selection rule; reading JSON bytes still scales with input size.

## Before production

The JSON convenience API still reads the entire input, including discarded
subtrees, for syntax and lengths. No input-byte, total-node, execution-time or
output-byte budgets are implemented.
Per-container/depth limits do not provide a practical total work bound for a
branching tree. Those budgets, output-size handling,
scanner classification/configuration and language bindings are follow-up work.

The shared trace attribute model does not currently represent JSON null or
unsigned integers, so this PoC uses the existing serde_json dependency rather
than changing that model or inventing another input DOM. A future adapter can
reuse structured-value tokens without coupling this crate to tracer exporting.

## Caller representation options

`extract_schema_json_with_options` accepts `SchemaOptions` for languages with
different input representations. `numbers_as_float` normalizes integer tokens
before deduplication; `array_length_limit` caps length metadata before nested
variants are deduplicated. Defaults preserve the existing JSON API exactly.
Neither option bounds input processing. The original length still determines
the truncation flag, and discarded JSON tails are still syntax-checked.

## Bounded source adapters

The extract_schema_from_source API reads caller-owned values lazily, using the
same selection, metadata and deduplication rules. It requests no scalar contents
and no children beyond the selection/depth limits. Errors from visited values
propagate unchanged; ignored tails are not validated. Bindings implement language
conversions and cycle checks. Empty and omitted record entries consume the quota,
and original entry counts determine truncation. This can differ from serializing
non-JSON application objects first. Enumerating object keys, executing getters
and branching trees can still be expensive; this interface is not a total
allocation, node or time budget.
