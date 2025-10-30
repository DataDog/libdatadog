# bin_tests

Binary integration tests for libdatadog.

## Overview

`bin_tests` provides integration tests that verify the functionality of libdatadog binaries and components in realistic scenarios.

## Test Types

- Binary execution tests
- Cross-process communication tests
- Integration tests for various components
- End-to-end workflow tests

## Running Tests

```bash
cargo test --package bin_tests
```

## Test Binaries

The crate includes test binaries for:
- Crashtracker receiver testing
- Sidecar testing
- IPC communication testing
- Various integration scenarios

