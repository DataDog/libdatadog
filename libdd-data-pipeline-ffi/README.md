# libdd-data-pipeline-ffi

C FFI bindings for the libdd-data-pipeline library.

## Overview

`libdd-data-pipeline-ffi` provides C-compatible FFI bindings for `libdd-data-pipeline`, enabling high-performance trace processing from C, C++, PHP, Ruby, Python, and other languages.

## Features

- **C API**: Complete C bindings for trace pipeline
- **Trace Export**: Submit and export traces
- **Statistics**: Compute trace statistics
- **Configuration**: Pipeline configuration via C API
- **Error Handling**: C-compatible error handling
- **cbindgen Headers**: Auto-generated C headers
- **Memory Safety**: Safe memory management across FFI boundaries

## API Functions

The C API provides functions for:
- Pipeline creation and configuration
- Trace submission
- Statistics computation
- Flush control
- Error handling
- Cleanup

## Example Usage

```c
#include <datadog/data_pipeline.h>

int main() {
    // Create pipeline
    ddog_trace_exporter_config_t config = {
        .endpoint = "https://trace.agent.datadoghq.com",
        .api_key = "your-api-key",
        // ...
    };
    
    ddog_trace_exporter_t* exporter;
    ddog_trace_exporter_new(&exporter, &config);
    
    // Send traces
    ddog_trace_t traces[] = { /* ... */ };
    ddog_trace_exporter_send(exporter, traces, trace_count);
    
    // Flush and cleanup
    ddog_trace_exporter_flush(exporter);
    ddog_trace_exporter_free(exporter);
    
    return 0;
}
```

## Integration

```bash
# Using pkg-config
gcc myapp.c $(pkg-config --cflags --libs datadog_data_pipeline) -o myapp
```

## Library Files

- **Header**: `include/datadog/data_pipeline.h`
- **Static**: `libdatadog_data_pipeline.a`
- **Dynamic**: `libdatadog_data_pipeline.so` (Linux), `.dylib` (macOS), `.dll` (Windows)

