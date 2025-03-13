test
test2

# `libdatadog`

`libdatadog` provides a shared library containing common code used in the implementation of Datadog's libraries,
including [Continuous Profilers](https://docs.datadoghq.com/tracing/profiler/).

(In a past life, `libdatadog` was known as [`libddprof`](https://github.com/DataDog/libddprof) but it was renamed when
we decided to increase its scope).

**NOTE**: If you're building a new Datadog library/profiler or want to contribute to Datadog's existing tools, you've come to the
right place!
Otherwise, this is possibly not the droid you were looking for.

## Development

### Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

### Building

Build `libdatadog` as usual with `cargo build`.

#### Builder crate
To generate a release with the builder crate use `cargo build -p builder` this will trigger all the necessary steps to
create the libraries, binaries, headers and package config files needed to use libdatadog in your project. The default
build does not include any capability so in order to add them here is the list of allowed features:
- profiling: includes the profiling ffi calls and headers to the package.
- telemetry: adds the telemetry symbols and headers to the package.
- data-pipeline: includes the data pipeline ffi calls to the library and headers to the package.
- crashtracker: adds crashtracking capabilities to the package.
- symbolizer: adds symbolizer capabilities to the package.

In order to set an output directory there's the `LIBDD_OUTPUT_FOLDER` environment varibale. Here's an example to create
a package with all the features and save the relese on `/opt/release` folder:
```bash
LIBDD_OUTPUT_FOLDER=/opt/release cargo build -p builder --features profiling,telemetry,data-pipeline,crashtracker,symbolizer
```

#### Build scripts
This is the non-prefered way of building a release, it's use is discouraged and it will be soon deprecated in favour of
using the builder crate.

To package a release with the generated ffi header and CMake module, use the `build-profiling-ffi.sh` / `build-telemetry-ffi.sh` helper scripts.
Here's an example of using on of these scripts, placing the output inside `/opt/libdatadog`:

```bash
bash build-profiling-ffi.sh /opt/libdatadog
```

#### Build dependencies

- Rust 1.78.0 or newer with cargo. See the Cargo.toml for information about bumping this version.
- `cbindgen` 0.26
- `cmake` and `protoc`

### Running tests

This project uses [cargo-nextest][nt] to run tests.

```bash
cargo nextest run
```

#### Installing cargo-nextest

The simplest way to install [cargo-nextest][nt] is to use `cargo install` like this.

```bash
cargo install --locked 'cargo-nextest@0.9.85'
```

#### Dev Containers

##### Prerequisites

- Install the [Dev Containers Extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers) in VS Code.

##### Steps

1. Open a local VS Code window on the cloned repository.
2. Open the command palette (`Ctrl+Shift+P` or `Cmd+Shift+P` on macOS) and select **"Dev Containers: Reopen in Container"**.
3. Choose the **Rust Container**.
4. VS Code will open a new window connected to the selected container.

#### Docker container
A dockerfile is provided to run tests in a Ubuntu linux environment. This is particularly useful for running and debugging linux-only tests on macOS.

To build the docker image, from the root directory of the libdatadog project run
```bash
docker build -f local-linux.Dockerfile -t libdatadog-linux .
```

To start the docker container, you can run
```bash
docker run -it --privileged -v "$(pwd)":/libdatadog -v cargo-cache:/home/user/.cargo libdatadog-linux
```

This will:
1. Start the container in privileged mode to allow the container to run docker-in-docker, which is necessary for some integration tests.
1. Mount the current directory (the root of the libdatadog workspace) into the container at `/libdatadog`.
1. Mount a named volume `cargo-cache` to cache the cargo dependencies at ~/.cargo. This is helpful to avoid re-downloading dependencies every time you start the container, but isn't absolutely necessary.

The `$CARGO_TARGET_DIR` environment variable is set to `/libdatadog/docker-linux-target` in the container, so cargo will use the target directory in the mounted volume to avoid conflicts with the host's default target directory of `libdatadog/target`.

#### Skipping tracing integration tests

Tracing integration tests require docker to be installed and running. If you don't have docker installed or you want to skip these tests, you can run:

```bash
cargo nextest run -E '!test(tracing_integration_tests::)'
```

Please note that the locked version is to make sure that it can be built using rust `1.78.0`, and if you are using a newer rust version, then it's enough to limit the version to `0.9.*`.

[nt]: https://nexte.st/
