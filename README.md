# libddprof

`libddprof` provides a shared library containing common code used in the implementation of Datadog's
[Continuous Profilers](https://docs.datadoghq.com/tracing/profiler/).


**NOTE**: If you're building a new profiler or want to contribute to Datadog's existing profilers, you've come to the
right place!
Otherwise, this is possibly not the droid you were looking for.

## Development

### Contributing

See <CONTRIBUTING.md>.

### Building

Build libddprof as usual with `cargo build`. To package a release with the generated ffi header and CMake module,
use the `ffi-build.sh` helper script. To stick the output in `/opt/libddprof`, you would do:

```bash
bash ffi-build.sh /opt/libddprof
```

#### Build Dependencies

Rust 1.47 or newer with cargo. Some platforms may need protoc; others have it shipped in prost-build.
