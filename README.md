# `libdatadog`

<table>
<tr>
<td width="70%">

`libdatadog` provides a shared library containing common code used in the implementation of Datadog's libraries,
including [Continuous Profilers](https://docs.datadoghq.com/tracing/profiler/).

**NOTE**: If you're building a new Datadog library/profiler or want to contribute to Datadog's existing tools, you've come to the
right place!
Otherwise, this is possibly not the droid you were looking for.

</td>
<td width="30%" align="center">
  <img src="docs/logo.png" alt="libdatadog logo" width="150"/>
</td>
</tr>
</table>

## Development

### Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

### Building

Build `libdatadog` as usual with `cargo build`.

#### Builder crate

You can generate a release using the builder crate. This will trigger all the necessary steps to create the libraries, binaries, headers and package config files needed to use a pre-built libdatadog binary in a (non-rust) project.
The default build does not include any capability so you'll need to list all features you want to include. You can see a full, up-to-date list of features in the `builder/Cargo.toml` file.

Here's one example of using the builder crate:

```bash
mkdir output-folder
cargo run --bin release --features profiling,telemetry,data-pipeline,symbolizer,crashtracker,library-config,log,ddsketch -- --out output-folder
```

#### Build dependencies

- Rust 1.84.1 or newer with cargo. See the Cargo.toml for information about bumping this version.
- `cbindgen` 0.29
- `cmake` and `protoc`

### Running tests

This project uses [cargo-nextest][nt] to run tests.

```bash
cargo nextest run
```

#### Installing cargo-nextest

The simplest way to install [cargo-nextest][nt] is to use `cargo install` like this.

```bash
cargo install --locked 'cargo-nextest@0.9.96'
```

#### Dev Containers

Dev Containers allow you to use a Docker container as a full-featured development environment with VS Code.

##### Prerequisites

- Install the [Dev Containers Extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers) in VS Code.
- Docker must be installed and running on your host machine.

##### Available Containers

We provide two Dev Container configurations:
- **Ubuntu**: Full-featured development environment with all dependencies installed
- **Alpine**: Lightweight alternative with minimal dependencies

##### Steps

1. Open a local VS Code window on the cloned repository.
2. Open the command palette (`Ctrl+Shift+P` or `Cmd+Shift+P` on macOS) and select **"Dev Containers: Reopen in Container"**.
3. Choose either **Ubuntu** or **Alpine** configuration when prompted.
4. VS Code will build and connect to the selected container environment.

The container includes all necessary dependencies for building and testing `libdatadog`.

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

[nt]: https://nexte.st/
