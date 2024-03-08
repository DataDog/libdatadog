# data-pipeline

## Modules

- **TraceExporter**: provides a minimum viable product (MVP) to send traces to agents. The aim of the project at this
state is to provide a basic API in order to test its viability and integration in different languages.

## Requirements
The current implementation assumes the following requisites must be met by the tracer:
- The protocol used is v0.4.
- All initialization must come from the tracer. The module won't try to infere any configuration.
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

### TraceExporterBuilder
```
impl TraceExporterBuilder
// Sets the timeouts for read write operations in milliseconds.
fn set_timeout(&mut self, timeout: u64) -> &mut TraceExporterBuilder 

// Sets the address and protocol for the agent connection.
fn set_host(&mut self, host: &str) -> &mut TraceExporterBuilder 

// Sets the port for the agent connection.
fn set_port(&mut self, port: u16) -> &mut TraceExporterBuilder 

// Sets the 'Datadog-Meta-Tracer-Version' header contents
fn set_tracer_version(&mut self, tracer_version: &str) -> &mut TraceExporterBuilder 

// Sets the 'Datadog-Meta-Lang' header contents
fn set_language(&mut self, lang: &str) -> &mut TraceExporterBuilder 

// Sets the 'Datadog-Meta-Version' header contents
fn set_language_version(&mut self, lang_version: &str) -> &mut TraceExporterBuilder 

// Sets the 'Datadog-Meta-Interpreter' header contents
fn set_language_interpreter(&mut self, lang_interpreter: &str) -> &mut TraceExporterBuilder 

// Creates a new TraceExporter
fn build(&mut self) -> TraceExporter
```
### TraceExporter
```
impl TraceExporter

// Sends a trace payload to the agent. 
// data: serialized trace in v0.4 format
// trace_count: number of traces contained in data. Used to set "X-Datadog-Trace-Count" header.
pub fn send(&mut self, data: Vec<u8>, trace_count: usize) -> Result<String, String>
```

### Examples
Initialization example: 

```
let mut builder = TraceExporterBuilder::default();
let exporter = builder
    .set_timeout(10)
    .set_host("http://127.0.0.1")
    .set_port(8127)
    .set_tracer_version("v0.1")
    .set_language("nodejs")
    .set_language_version("1.0")
    .set_language_interpreter("v8")
    .build();

```

Sending a trace:
```
match exporter.send(payload, payload_size) {
    Ok(r) => // handle response
    Err(e) => //handle error
}
```
## Development
For the sake of simplicity the binding projects for the different languages will be placed on libdatadog workspace
and use the data-pipeline crate as a dependency.

libdatadog             
|                      
+--data-pipeline       
|                      
+--data-pipeline-ffi   
|                      
+--...                 
|                      
|                      
+--data-pipeline-nodejs


## Future work
- Asynchronous interface.
- Handle transformations between different protocol versions.
- Agent API discovery.
