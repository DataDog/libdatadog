# Build FFI examples

To build the FFI libraries, headers, and example executables, run this command from the project root directory:

```bash
./examples/ffi/build-examples.sh
```

This will automatically build the FFI libraries and compile all the example executables.

## Adding new FFI examples

When adding an example for a new FFI crate, you may need to update the features list in the build script to ensure the
crate is included in the build:

1. Open `build-examples.sh`
2. Find the `FEATURES` array
3. Add your new feature to the array
4. The script will automatically include it in the build

# Run FFI examples

The build command will create executables in the examples/ffi/build folder. You can run any of them with:

```
./examples/ffi/build/ddsketch
./examples/ffi/build/telemetry
./examples/ffi/build/crashtracker
./examples/ffi/build/trace_exporter
# ... etc
```
