# libdd-sidecar-ffi

C FFI bindings for the libdd-sidecar library.

## Overview

`libdd-sidecar-ffi` provides C-compatible FFI bindings for interacting with the Datadog sidecar process from C, C++, PHP, Ruby, Python, and other languages.

## Features

- **C API**: Complete C bindings for sidecar communication
- **Session Management**: Create and manage sidecar sessions
- **Trace Submission**: Submit traces via IPC
- **Metric Submission**: Send DogStatsD metrics
- **Telemetry**: Report telemetry events
- **Remote Config**: Query remote configuration
- **Cross-platform**: Unix and Windows support
- **cbindgen Headers**: Auto-generated type-safe C headers

## API Functions

The C API provides functions for:
- Session creation and management
- Trace flushing and submission
- Metric sending
- Telemetry event reporting
- Configuration queries
- Error handling

## Example Usage

```c
#include <datadog/sidecar.h>

int main() {
    // Connect to sidecar
    ddog_sidecar_session_t* session;
    ddog_sidecar_connect(&session, /* config */);
    
    // Send traces
    ddog_sidecar_send_trace(session, /* trace data */);
    
    // Send metrics
    ddog_sidecar_send_metric(session, "my.metric", 42.0, /* tags */);
    
    // Cleanup
    ddog_sidecar_disconnect(session);
    return 0;
}
```

## Integration

```bash
# Using pkg-config
gcc myapp.c $(pkg-config --cflags --libs datadog_sidecar) -o myapp
```

## Library Files

- **Header**: `include/datadog/sidecar.h`
- **Static**: `libdatadog_sidecar.a`
- **Dynamic**: `libdatadog_sidecar.so` (Linux), `.dylib` (macOS), `.dll` (Windows)

