# libdd-api-security

A structural schema extraction proof of concept for API Security, independent of
AppSec. It is a pure Rust library: no Node.js binding, C FFI, sampling, remote
configuration transport, threads or global initialization.

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
`4b46c924`. The fixtures include schema and scanner cases; a custom test source retains
unknown values and duplicate keys that a parsed JSON map cannot represent.

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
full configuration-language compatibility and language bindings are follow-up work.

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

## Scanner and collection compatibility (2026-11-12)

Optional compiled scanners attach string metadata using libdd-regex and the
existing ipnet dependency. Exact matching uses sorted standard-library vectors.
Regex shorthand classes and word boundaries follow RE2's ASCII behavior using
the existing regex-syntax parser. This does not claim complete RE2 dialect
equivalence.

Use extract_schema_with_scanners, extract_schema_json_with_scanners, or
extract_schema_from_source_with_scanners. The last requires ScannerSource in
addition to SchemaSource. It reads selected string contents only when a value
matcher needs them, checks scanner keys first, and shares one string read among
value matchers. Structural extraction never requests string contents. The first
matching scanner wins, including when it has empty tags. Tags participate in
structural equality. ScalarType::Unknown supports non-JSON invalid values.

load_scanners accepts native-format regex/exact/IP conditions and an explicit
semver compatibility version. load_processors and load_processor_overrides
handle schema mappings, version bounds and scanner references. Invalid items
produce per-item diagnostics while valid neighboring definitions remain usable.

CollectionBuilder accepts named configuration sources and builds immutable
Collector snapshots. Replacing/removing a source changes future snapshots only.
Regex compilation happens at update time; scanner selection happens at build
time. Selection precedence is exclusion by ID, inclusion by ID, exclusion by
tags, then inclusion by tags. Requests do not resolve those references again.
A collector accepts an address lookup supplied by its caller, so request
persistence, ephemeral overlays and sampling remain tracer responsibilities.

The compatibility layer implements extract_schema processors and typed equality
gates on resolved input values. It does not implement general WAF expressions,
attack evaluation, rule actions, fingerprint/JWT/URI processors, or every native
scanner operator. Native scanner ordering can depend on its internal storage;
this library uses caller order for direct extraction and lexical IDs for
collector snapshots. Overlapping classifiers should therefore have an explicit
ordering policy in their caller. No deadline or total work/output budget is
implemented.

tests/fixtures/libddwaf-coverage.json maps all 103 inventoried reference cases at
revision 4b46c924: 24 schema, seven scanner, 16 scanner-parser, 17 processor-parser,
10 override-parser, 13 schema integration, 13 override integration and three
diagnostics cases. The ports retain the reference inputs and expected schema/
metadata outputs, including unknown values and duplicate keys. Integration
ports check collection routing/configuration behavior; WAF event assertions,
WAF return codes, timeout fields and rule-only known addresses are outside this
library's scope. This is coverage of the selected reference cases, not proof of
complete libddwaf feature or regex equivalence.

Run python3 libdd-api-security/tools/check_libddwaf_coverage.py ../libddwaf
from the workspace root to audit source hashes and mappings, together with
cargo nextest run -p libdd-api-security to execute the assertions.
The suite currently has 200 tests, including property tests and bounded string
read/error regression tests. Binding and HTTP validation of these new scanner/
configuration changes remains pending; the earlier Node HTTP benchmark covered
the structural implementation at revision 181758dff.


## Matched structural performance check (2026-11-12)

Three alternating before/after rounds ran all 50 Criterion cases on the same
machine/compiler, with no concurrent builds or tests. The baseline was
181758dff; the candidate includes the scanner/configuration changes above.
Each round used 30 samples, 200 ms warm-up and 500 ms measurement. The stored
report identifies the compiler, OS, candidate source hash and every per-round median.

These structural paths are approximately flat: extraction median changes span
-2.3% to +0.4%, and the raw JSON API spans -2.9% to +2.9%. Per-round variation is
larger for some cases. This does not establish a general performance win.
The earlier 15–19% small-JSON/deep-JSON regression in a draft was removed by
specializing structural traversal so it carries no scanner/key state.

| Workload | Extraction before / after (µs) | JSON API before / after (µs) |
| --- | ---: | ---: |
| record-10 | 0.216 / 0.214 | 0.597 / 0.579 |
| record-255 | 2.875 / 2.856 | 12.057 / 12.149 |
| record-1000 | 2.883 / 2.880 | 30.319 / 30.633 |
| array-10 | 0.789 / 0.782 | 1.369 / 1.363 |
| array-1000 | 0.794 / 0.788 | 51.681 / 52.056 |
| depth-18 | 0.398 / 0.390 | 0.925 / 0.913 |
| depth-100 | 0.396 / 0.396 | 2.151 / 2.131 |
| realistic-20 | 0.919 / 0.904 | 2.276 / 2.309 |
| realistic-500 | 0.919 / 0.898 | 26.650 / 27.410 |
| heterogeneous-10 | 0.822 / 0.826 | 1.948 / 1.902 |

This comparison checks the cost of adding optional classification to structural
entry points. It does not measure scanner-enabled workloads, Node/WASM overhead,
request CPU, RSS or allocation counts. Those require separate matched tests.
The source adapter's key-first classification optimization has direct read-count
tests (zero string reads for rejected keys/key-only matches; one read shared by
multiple value matchers), but is not an HTTP speedup claim.

Results: [JSON estimates](benches/results/2026-11-12-core-comparison.json) and
[CSV medians](benches/results/2026-11-12-core-comparison.csv).

Validation: 200 crate tests, three doc tests, Clippy, formatting, structural lint
and workspace dependency-declaration checks pass. The third-party license CSV
regeneration is unchanged. The workspace cargo-deny check still fails on
existing advisories and license policy: no package versions or sources changed.
