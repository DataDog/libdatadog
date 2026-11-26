# Fuzzing for libdd-trace-normalization

This directory contains fuzz targets for the `libdd-trace-normalization` library using `cargo-fuzz` (libFuzzer).

## Prerequisites

Install `cargo-fuzz`:

```bash
cargo install cargo-fuzz
```

## Fuzz targets

Each target checks the result of the normalization against the guarantees in
`src/invariants.rs` (valid UTF-8, length limits, allowed characters, and that normalizing again
changes nothing), not only that it doesn't panic:

- `fuzz_normalize_span`: the v0.4 normalizer, on `pb::Span`.
- `fuzz_normalize_span_v1`: the V1 normalizer, on spans with attributes of any type.
- `fuzz_normalize_strings`: each string normalizer, on the valid UTF-8 prefix of the input.

The unit tests also run these targets on random inputs (`cargo nextest run -p
libdd-trace-normalization fuzz::`), so the checks are exercised without `cargo-fuzz`.

## Running the Fuzzer

Run the fuzzer indefinitely (Ctrl+C to stop):

```bash
cd /path/to/libdd-trace-normalization
cargo fuzz run fuzz_normalize_span
```

Reproduce a finding:

```bash
cargo fuzz run fuzz_normalize_span fuzz/artifacts/fuzz_normalize_span/crash-<id>
```

To generate coverage information:

```bash
cargo fuzz coverage fuzz_normalize_span
```
