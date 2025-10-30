# libdd-live-debugger

Dynamic instrumentation and live debugging for Datadog.

## Overview

`libdd-live-debugger` provides live debugging capabilities including dynamic log injection, metric collection, and span creation without redeploying applications.

## Features

- **Dynamic Logging**: Inject log lines at runtime
- **Metric Probes**: Collect metrics from specific code locations
- **Span Probes**: Create spans dynamically
- **Snapshot Capture**: Capture variable snapshots
- **Conditional Probes**: Probes with conditions
- **Remote Configuration**: Receive probe configurations remotely

## Probe Types

- **Log Probes**: Emit logs when code locations are hit
- **Metric Probes**: Increment counters or record values
- **Span Probes**: Create distributed tracing spans
- **Snapshot Probes**: Capture local variable values

## Example Usage

```rust
use libdd_live_debugger;

// Probe definitions received via remote config
// Applied dynamically to running application
```

## Use Cases

- Debug production issues without redeployment
- Collect metrics from specific code paths
- Add temporary logging for investigation
- Create spans for new code paths

