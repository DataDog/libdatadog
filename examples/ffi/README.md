
# Build FFI examples

In order to be able to run FFI examples, you need to build the shared library and headers with the command:
```bash
cargo run --bin release --features profiling,telemetry,data-pipeline,symbolizer,crashtracker --release -- --out
```

You can then build the examples with:

```bash
# Run the below commands at the root level
# Datadog Root env is read from the DatadogConfig.cmake
Datadog_ROOT=$PWD/release cmake -S examples/ffi -B examples/ffi/build
cmake --build ./examples/ffi/build
```

# Run FFI examples

The build command will create executables in the examples/ffi/build folder. You can run any of them with:
````
./examples/ffi/build/test-name
````

# Working example with Crashtracking

```
cargo run --bin release --features profiling,telemetry,data-pipeline,symbolizer,crashtracker --release -- --out
Datadog_ROOT=$PWD/release cmake -S examples/ffi -B examples/ffi/build
cmake --build ./examples/ffi/build 
# You can also focus on a single target
# cmake --build ./examples/ffi/build -t crashtracking
./examples/ffi/build/crashtracking
```
