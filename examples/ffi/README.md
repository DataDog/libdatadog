
# Build FFI examples
*All the commands in this README are executed at the root level.*

In order to be able to run FFI examples, you need to build the shared library and headers with the command:
```c
// (dont worry too much about the fact that this is named "profiling")
./build-profiling-ffi.sh ./build
````

You can then build the examples with:

```c
cmake -S examples/ffi -B examples/ffi/build -D Datadog_ROOT=./build
cmake --build ./examples/ffi/build
````

# Run FFI examples

The build command will create executables in the examples/ffi/build folder. You can run any of them with:
````
./test-name
````
