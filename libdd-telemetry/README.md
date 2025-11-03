# libdd-telemetry

Internal telemetry library for reporting Datadog library metrics and events.

## Overview

`libdd-telemetry` provides telemetry collection and reporting for Datadog libraries, allowing them to report their own operational metrics, configuration, and errors back to Datadog.

## Features

- **Metrics Collection**: Collect library performance metrics
- **Configuration Reporting**: Report library configuration
- **Error Tracking**: Track and report library errors
- **Host Information**: Automatic host and container metadata
- **HTTP Transport**: Send telemetry data to Datadog intake
- **Worker Pattern**: Async background telemetry worker
- **Dependency Tracking**: Report library dependencies
- **Application Metadata**: Track application information

## Modules

- `config`: Telemetry configuration
- `data`: Telemetry data types and structures
- `info`: System and host information gathering
- `metrics`: Metrics collection and aggregation
- `worker`: Background telemetry worker

## Telemetry Data Types

The library collects and reports:
- **Metrics**: Count, gauge, rate, and distribution metrics
- **Logs**: Library log events
- **Configurations**: Library configuration changes
- **Dependencies**: Loaded library versions
- **Integrations**: Active integrations
- **App Started**: Application lifecycle events

## Example Usage

```rust
use libdd_telemetry::{build_host, data};

// Build host information
let host = build_host();

// Create telemetry data
let app = data::Application {
    service_name: "my-service".to_string(),
    env: Some("production".to_string()),
    // ...
};
```

## Host Information

Automatically gathers:
- Hostname
- Container ID
- OS name and version
- Kernel information
- Entity ID

