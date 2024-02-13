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

Not every profiler / tracer needs every feature, so we need it a way of pulling in exactly what we need.

## Not a solution

### Splitting features in several libraries

You could imagine delivering several libraries:
- telemetry library
- profiling library 
- symbolization library

However by doing so, we expose ourselves to duplicated Rust runtimes (with possibly different versions and behaviours). We can also imagine symbol and ABI issues resulting from such a build pipeline.

## Proposed solution

### 1 Building everything together 

Building a single static library with all features bundled. This means that downstream builds would be in charge of removing the parts that are not required.
By linking against a static library, the compiler will remove the parts that are not needed.

Cons
- Cross team dependencies to deliver artifacts (flaky tests)
- Shared libraries need an extra step (refer to option 3)
A shared library with everything in it will be too big. You need to link against a static library to remove the unused parts of the library.
- Longer build times when linking against static libraries
Some profilers / tracers require compiling from source. Building against static libraries means longer build times.

Pros
+ Simple build pipeline
+ Easier to experiment with a new feature (it is already availalbe locally)

### 2 Features

Create new pipelines with a select amount of features.
For languages that need shared libraries, we should make sure we are able to select the features we publish.

Cons
- More CI pipelines
- A slightly weird pattern (shell crate)

Pros
+ Easy to select the features you want to build
+ More modular and controlled usage of dependencies

#### Example

*Build step*

We have as "shell" crate that pulls in all of the libdatadog features.
We select what we want at build time.

```
cargo build --features symbolizer telemetry crash-tracking
```

*Delivery step*

CI pipelines are created with the required features.

### 3 Intermediate builds to produce shared libraries

For the languages that need shared libraries, we can add an intermediate build step, which selects the APIs that need to be kept and published.
This intermediate build step can be either in the per-language CIs or within the libdatadog distribution steps.

Pros
+ Fine grain control over what is delivered

Cons
- Additional complexity (slower iterations to produce artifacts)

#### Example through Ruby

*Current state*

- Libdatadog release builds a static and a dynamic library for profiling, which gets uploaded as a tarball to GitHub.
- The scripts in the ruby/ folder in the libdatadog repository take the GitHub release, keeps only the dynamic library, adds a few Ruby helpers, packages and uploads it as the "libdatadog" gem to rubygems.org.
- Downstream dd-trace-rb consumes the "libdatadog" gem, using the shared library inside.

*Proposed change*

- Libdatadog release builds only a static library with all features. This gets released.
- We update the scripts in the ruby folder to take as an input the static library and to repackage it as a dynamic library containing only the APIs that Ruby makes use of. That dynamic library gets uploaded as the "libdatadog" gem to rubygems.org
- Same as above -- no changes needed.

### 4 Interdependent shared libraries

Split different features into shared libraries that expose their symbols and depend on each other.
For example, we can imagine an exporter library on which the profiling aggregation library depends. This same export library is shared by the tracing aggregation.

Pros:
- Pull in exactly what you need
- Single CI builds

Cons:
- Dependencies are harder to maintain. 
You need to think exactly about what should be published and in what library.
- Duplication of Rust runtime APIs (example: panic handlers)

## Recommended

Solution 1 and 2.

We can deliver a single static library (with all features) for languages that can link against a static library (solution 1).
For the languages that require shared libraries, we will use the feature solution (solution 2). This also allows developers to build and test only what they are currently working on.
