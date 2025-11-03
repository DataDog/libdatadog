# libdd-log

Logging library for Datadog products.

## Overview

`libdd-log` provides structured logging capabilities for Datadog libraries with support for log levels, structured fields, and integration with the Datadog logging backend.

## Features

- **Structured Logging**: Key-value structured log fields
- **Log Levels**: Debug, Info, Warn, Error
- **Context**: Attach context to log messages
- **Backend Integration**: Send logs to Datadog
- **Performance**: Low overhead logging
- **Thread Safety**: Safe for concurrent use

## Example Usage

```rust
use libdd_log;

// Log messages
// log::info!("Processing request", request_id = "abc123");
// log::error!("Failed to connect", error = &err);
```

