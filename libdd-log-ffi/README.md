# libdd-log-ffi

C FFI bindings for libdd-log logging library.

## Overview

`libdd-log-ffi` provides C-compatible FFI bindings for structured logging.

## Features

- **C API**: Complete C bindings for logging
- **Log Levels**: Debug, info, warn, error
- **Structured Fields**: Attach key-value pairs to logs
- **Performance**: Low overhead C interface

## Example Usage

```c
#include <datadog/log.h>

int main() {
    // Initialize logger
    ddog_log_init();
    
    // Log messages
    ddog_log_info("Processing started", NULL, 0);
    ddog_log_error("Connection failed", NULL, 0);
    
    // Cleanup
    ddog_log_shutdown();
    return 0;
}
```

