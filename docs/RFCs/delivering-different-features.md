# Building and delivering different features 

## Context

Our current build system is hard coded based on the profiling / telemetry requirements.
With the growth of libdatadog, we need to adjust our build system.

The current commands are :

```
./build_profiling-profiling.sh <package_directory>
```

OR

```
./build_telemetry-ffi.sh <package_directory>
```

Releases are then created through CI pipelines.

The issue is that we have several features:
- telemetry
- exporting profiles
- aggregating profiles
- crash tracking
- symbolization (ongoing).

Not every profiler / tracer needs every feature, so we need it here way of Building and delivering exactly what we need.

## Not a solution

### Delivering different libraries

By delivering features in several libraries, we expose ourselves to duplicated Rust runtimes (with possibly different versions and behaviours).

## Proposed solution

### Building everything together 

Building a single static library with all features bundled. This means that downstream builds would be in charge of removing the parts that are not required.
By linking against a static library, the compiler will remove the parts that are not needed.

Cons
- Longer build times
- Cross team dependencies to deliver artifacts (flaky tests)
- Shared libraries are no longer an option
A shared library with everything in it will be too big. You need to link against a static library to remove the unused parts of the library.
- Longer build times 
Some profilers / tracers require compiling from source. Building against static libraries means longer build times.

Pros
+ Simple build pipeline
+ Easier to experiment with a new feature (it is already availalbe locally)

### Features

Create new pipelines with a select amount of features.
For languages that need shared libraries, we should make sure we are able to select the features we publish.

The way this would work:

#### Build
We have as "shell" crate that pulls in all of the libdatadog features.
We select what we want at build time.

```
cargo build --features symbolizer telemetry crash-tracking
```

#### Delivery

CI pipelines are created with the required features.

## Recommended

I think we can do both proposed solutions. For the languages that require shared libraries, we can have the feature solution.
We can deliver a large static library for languages that link against a static library.
