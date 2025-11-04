# libdd-ddsketch-ffi

C FFI bindings for the `libdd-ddsketch` crate.

## Overview

Provides C-compatible bindings for DDSketch, a data structure for accurate quantile estimation with configurable relative error guarantees.

## Main Features

- **Sketch Management**: Create and destroy DDSketch instances
- **Data Insertion**: Add values to sketches with optional counts/weights
- **Metrics**: Query total count of points in a sketch
- **Serialization**: Encode sketches to protobuf format for transmission

## Building

This crate currently is not intended to use it as-is so it is compiled and its methods re-exported through the builder artifacts.
```bash
cargo run --bin release --features ddsketch -- --out libdatadog
```

This generates C headers and builds the library as a dynamic library for use in C/C++ applications.
