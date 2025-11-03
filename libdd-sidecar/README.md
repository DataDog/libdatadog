# libdd-sidecar

Datadog sidecar process for managing telemetry and APM data.

## Overview

`libdd-sidecar` provides a sidecar process that runs alongside applications to handle telemetry collection, trace processing, and communication with Datadog backends without blocking the main application.

## Features

- **Process Isolation**: Runs as separate process, doesn't affect main app
- **IPC Communication**: Fast inter-process communication with applications
- **Trace Processing**: Aggregates and processes trace data
- **Remote Configuration**: Receives and distributes remote config
- **Telemetry Collection**: Aggregates telemetry from multiple sources
- **Background Processing**: Non-blocking data processing and transmission
- **Session Management**: Manages connections from multiple application instances
- **Health Monitoring**: Built-in health checks and monitoring

## Architecture

The sidecar acts as an intermediary between applications and Datadog:
```
Application(s) <--IPC--> Sidecar <--HTTP--> Datadog Backend
```

Benefits:
- Reduced overhead in application process
- Shared connection pooling
- Cross-process aggregation
- Consistent configuration management

## Modules

- `service`: Core sidecar service implementation
- `interface`: IPC interface definitions
- `config`: Sidecar configuration
- `dogstatsd`: DogStatsD server
- `telemetry`: Telemetry aggregation
- `tracer`: Trace processing
- `remote_config`: Remote configuration client

## Use Cases

- **Multi-process Applications**: Share telemetry infrastructure across processes
- **Serverless**: Optimize cold starts by offloading work
- **High-throughput**: Batch and compress data efficiently
- **Resource Constrained**: Minimize per-process overhead

## Running the Sidecar

```bash
# Start the sidecar
datadog-sidecar --config config.yaml
```

## IPC Endpoints

The sidecar exposes endpoints for:
- Trace submission
- Metric submission
- Telemetry events
- Remote config retrieval
- Session management

