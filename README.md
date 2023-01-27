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

Build `libdatadog` as usual with `cargo build`. To package a release with the generated ffi header and CMake module,
use the `ffi-build.sh` helper script. To stick the output in `/opt/libdatadog`, you would do:

```bash
bash ffi-build.sh /opt/libdatadog
```

#### Build Dependencies

Rust 1.60 or newer with cargo. Some platforms may need protoc; others have it shipped in prost-build.
