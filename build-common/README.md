# build-common

Common build utilities for libdatadog crates.

## Overview

`build-common` provides shared build script utilities used across multiple libdatadog crates, particularly for cbindgen integration and C header generation.

## Features

- **cbindgen Integration**: Generate C headers from Rust code
- **Build Script Helpers**: Common build.rs utilities
- **Header Generation**: Automated C/C++ header generation
- **Configuration**: Shared build configuration

## Usage in build.rs

```rust
use build_common::cbindgen;

fn main() {
    cbindgen::generate_headers().expect("Failed to generate headers");
}
```

## Feature Flags

- `cbindgen`: Enable cbindgen support for header generation

