# libdd-telemetry-ffi

C FFI bindings for libdd-telemetry internal telemetry library.

## Overview

`libdd-telemetry-ffi` provides C-compatible FFI bindings for `libdd-telemetry`, enabling internal telemetry reporting from applications written in C, C++, and other languages.

## Features

- **C API**: Complete C bindings for telemetry reporting
- **Worker Management**: Start and manage telemetry worker
- **Metric Reporting**: Report counts, gauges, and distributions
- **Log Reporting**: Report library logs
- **Configuration Reporting**: Report library configuration
- **Application Metadata**: Track application information
- **Dependency Tracking**: Report library dependencies

## API Functions

- Worker lifecycle management
- Metric collection (count, gauge, rate, distribution)
- Log event reporting
- Configuration change reporting
- Dependency tracking
- Error handling

## Example Usage

```c
#include <datadog/telemetry.h>

int main() {
    // Initialize telemetry
    ddog_telemetry_config_t config = {
        .service_name = "my-service",
        .endpoint = "https://instrumentation-telemetry-intake.datadoghq.com",
        // ...
    };
    
    ddog_telemetry_handle_t* telemetry;
    ddog_telemetry_init(&telemetry, &config);
    
    // Report metrics
    ddog_telemetry_add_count(telemetry, "my.metric", 1.0, NULL, 0);
    
    // Cleanup
    ddog_telemetry_shutdown(telemetry);
    return 0;
}
```

