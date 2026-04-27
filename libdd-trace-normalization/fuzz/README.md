# Fuzzing for libdd-trace-normalization

This directory contains fuzz targets for the `libdd-trace-normalization` library using `cargo-fuzz` (libFuzzer).

## Prerequisites

Install `cargo-fuzz`:

```bash
cargo install cargo-fuzz
```

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
