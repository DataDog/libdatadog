# FFI Examples

This directory contains C and C++ examples that demonstrate how to use the FFI
bindings.

## Quick Start: Run All Tests

The easiest way to build and run all FFI examples is using the cargo command:

```bash
cargo ffi-test
```

This command will:
1. Build all FFI libraries with the required features
2. Compile all C/C++ examples using CMake
3. Run each example and report pass/fail status

### Options

```bash
cargo ffi-test --skip-build          # Skip build, run existing examples
cargo ffi-test --filter telemetry    # Only run examples matching "telemetry"
cargo ffi-test --keep-artifacts      # Keep temp directory with generated files
cargo ffi-test --help                # Show help with all options
```

In CI, use `--keep-artifacts` to preserve generated files for debugging failed
tests.

## Manual Build

To build the FFI libraries and examples manually:

```bash
./examples/ffi/build-examples.sh
```

This will automatically build the FFI libraries and compile all the example
executables.

## Adding New FFI Examples

When adding an example for a new FFI crate:

1. Add your C or C++ source file to this directory
2. Update `CMakeLists.txt` to add the new executable target
3. Update `build-examples.sh` to include your feature in the `FEATURES` array
4. Update the skip/expected-failure lists in `tools/src/bin/ffi_test.rs` if needed

## Running Examples Manually

The build command creates executables in the `examples/ffi/build` folder:

```bash
./examples/ffi/build/ddsketch
./examples/ffi/build/telemetry
./examples/ffi/build/profiles
./examples/ffi/build/trace_exporter
# ... etc
```

## Notes

- **crashtracking**: This example may intentionally trigger a crash to test crash handling.
  Consider adding it to the skip list in `tools/src/bin/ffi_test.rs` if running it causes issues.
- Tests run in a temporary directory. Test data paths (like `datadog-ffe/tests/data`) are
  symlinked into the temp directory automatically.
- Generated artifacts are listed at the end of the run and cleaned up unless `--keep-artifacts` is used.
