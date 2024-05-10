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

To package a release with the generated ffi header and CMake module, use the `build-profiling-ffi.sh` / `build-telemetry-ffi.sh` helper scripts.
Here's an example of using on of these scripts, placing the output inside `/opt/libdatadog`:

```bash
bash build-profiling-ffi.sh /opt/libdatadog
```

#### Build Dependencies

- Rust 1.71 or newer with cargo
- `cmake` and `protoc`

### Running tests

This project uses [cargo-nextest][nt] to run tests.

```bash
cargo nextest run
```

#### Installing cargo-nextest

The simplest way to install [cargo-nextest][nt] is to use `cargo install` like this.

```bash
cargo install --locked 'cargo-nextest@0.9.67'
```

Please note that the locked version is to make sure that it can be built using rust `1.71.1`, and if you are using a newer rust version, then it's enough to limit the version to `0.9.*`.

[nt]: https://nexte.st/
