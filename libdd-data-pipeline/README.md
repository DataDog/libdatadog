# libdd-data-pipeline

Data pipeline for processing and exporting APM data to Datadog.

## Overview

`libdd-data-pipeline` provides a high-performance pipeline for processing distributed tracing data, applying normalization and obfuscation, computing statistics, and exporting to Datadog backends.

## Status
Currently the project is a proof of concept so the API is not definitive and possible frequent changes should be
expected. 

## Features

- **Trace Processing**: Normalize and validate trace data
- **Statistics Computation**: Compute trace statistics with time-bucketing
- **Obfuscation**: Remove sensitive data from traces
- **Compression**: Zstd and gzip compression support
- **Batching**: Efficient payload batching
- **Export**: HTTP/HTTPS export to Datadog intake
- **Retry Logic**: Automatic retry with backoff
- **Metrics**: Built-in pipeline metrics
- **Remote Config**: Support for remote configuration

## Pipeline Stages

1. **Ingestion**: Receive traces from applications
2. **Normalization**: Apply span normalization rules
3. **Obfuscation**: Remove sensitive information
4. **Stats Computation**: Aggregate into statistics buckets
5. **Serialization**: Encode in MessagePack or Protobuf
6. **Compression**: Compress payloads
7. **Export**: Send to Datadog backend with retry

## Modules

- `trace_exporter`: Main trace export functionality. Provides a minimum viable product (MVP) to send traces to agents. The aim of the project at this state is to provide a basic API in order to test its viability and integration in different languages.
- `stats`: Statistics computation
- `normalize`: Trace normalization
- `obfuscate`: Sensitive data obfuscation
- `serialize`: Payload serialization
- `http`: HTTP client and transport

## Requirements
The current implementation assumes the following requisites must be met by the tracer:
- The protocol used is v0.4.
- All initialization must come from the tracer. The module won't try to infer any configuration.
- The trace must be serialized in msgpack before passing it to the data-pipeline module.
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

Currently the **data-pipeline** crate exports just one module `TraceExporter` which follows a builder pattern to
configure the communication.

See [`trace_exporter.rs`](src/trace_exporter.rs).

## Integrating the TraceExporter in the tracers
### \[WIP\]Importing a binary into an existing project
In case you want to use a binary to hook C-like functions in your language we will provide a crate to build such binary.
This crate is located in:
libdatadog             
|                      
+--data-pipeline-ffi   

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
new crate in libdatadog workspace and use the data-pipeline crate as a dependency.

#### Create new crate
```
cargo new data-pipeline-nodejs --lib
```

#### Set up dependencies
In order to use the TraceExporter in your project the `data-pipeline` crate needs to be added in the bindings dependency
list.

```
[package]
# Package attributes

[lib]
crate-type = ["cdylib"]

[dependencies]
# Language bindings framework dependencies.

data-pipeline = { path = ../data-pipeline }

[build-dependencies]
# Build framework dependencies
```

#### Build and artifact generation
The building and artifact generation process will depend heavily on the framework selected.

## Expected workflow from teams integrating data-pipeline
- Create your own language bindings.
- Hook it in the tracer.
- Feedback about difficulties about integrating the solution, performance and package size.

## Future work
- Asynchronous interface.
- Handle transformations between different protocol versions.
- Agent API discovery.


## Example Usage

```rust
use libdd_data_pipeline;

// Create pipeline
// let pipeline = TracePipeline::new(config)?;

// Process traces
// pipeline.send_traces(traces).await?;

// Pipeline automatically:
// - Normalizes spans
// - Computes statistics  
// - Obfuscates sensitive data
// - Batches and compresses
// - Exports to Datadog
```

## Configuration

The pipeline supports configuration for:
- Normalization rules
- Obfuscation settings
- Statistics bucketing
- Compression levels
- Batch sizes
- Retry policies
- Backend endpoints
