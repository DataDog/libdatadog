# libdd-data-pipeline

Data pipeline for processing and exporting APM data to Datadog.

## Overview

`libdd-data-pipeline` provides a high-performance pipeline for processing distributed tracing data, applying normalization and obfuscation, computing statistics, and exporting to Datadog backends.

## Status
Currently the project is a proof of concept so the API is not definitive and possible frequent changes should be
expected. 

## Modules

- **TraceExporter**: Main trace export functionality. Provides a minimum viable product (MVP) to send traces to agents. The aim of the project at this state is to provide a basic API in order to test its viability and integration in different languages.

## Requirements
The current implementation assumes the following requisites must be met by the tracer:
- The protocol used is v0.4.
- All initialization must come from the tracer. The module won't try to infer any configuration.
- The trace must be serialized in msgpack before passing it to the libdd-data-pipeline module.
- Sending process is synchronous.
- The agent's response will be handled by the tracer.


## Dataflow

```
  +--------------+                 +--------------+                  +--------------+
  |              |  msgpack(v0.4)  |              |   msgpack(v0.x)  |              |
  |              +---------------->|              +----------------->|              |
  |    Tracer    |                 |   Exporter   |                  |    Agent     |
  |              |                 |              |                  |              |
  |              |    Response     |              |   Response       |              |
  |              |<----------------+              |<-----------------+              |
  +--------------+                 +--------------+                  +--------------+
```

## API

Currently the **libdd-data-pipeline** crate exports just one module `TraceExporter` which follows a builder pattern to
configure the communication.

See [`trace_exporter.rs`](src/trace_exporter.rs).

## Integrating the TraceExporter in the tracers
### \[WIP\]Importing a binary into an existing project
In case you want to use a binary to hook C-like functions in your language we will provide a crate to build such binary.
This crate is located in:
libdatadog             
|                      
+--libdd-data-pipeline-ffi   

#### Build
```
cargo build --debug/release
```
#### Artifacts
The build will produce two artifacts:
- `libdata-pipeline-ffi.so`
- `libdata-pipeline.h`
They will be located in `libdatadog/target/\[debug|release\]/`. 

### Building the bindings directly in libdatadog
In case of using a Rust framework in order to build the bindings for you language there is the possibility to create a
new crate in libdatadog workspace and use the libdd-data-pipeline crate as a dependency.

#### Create new crate
```
cargo new libdd-data-pipeline-nodejs --lib
```

#### Set up dependencies
In order to use the TraceExporter in your project the `libdd-data-pipeline` crate needs to be added in the bindings dependency
list.

```
[package]
# Package attributes

[lib]
crate-type = ["cdylib"]

[dependencies]
# Language bindings framework dependencies.

libdd-data-pipeline = { path = ../libdd-data-pipeline }

[build-dependencies]
# Build framework dependencies
```

#### Build and artifact generation
The building and artifact generation process will depend heavily on the framework selected.

## Expected workflow from teams integrating libdd-data-pipeline
- Create your own language bindings.
- Hook it in the tracer.
- Feedback about difficulties about integrating the solution, performance and package size.

## Future work
- Asynchronous interface.
- Handle transformations between different protocol versions.
- Agent API discovery.
