# libdd-live-debugger-ffi

C FFI bindings for libdd-live-debugger dynamic instrumentation.

## Overview

`libdd-live-debugger-ffi` provides C-compatible FFI bindings for live debugging capabilities.

## Features

- **C API**: Complete C bindings for live debugging
- **Probe Management**: Create and manage probes
- **Remote Config**: Receive probe configurations
- **Data Capture**: Capture variable snapshots
- **Memory Safety**: Safe memory management across FFI

## Example Usage

```c
#include <datadog/live_debugger.h>

int main() {
    // Initialize live debugger
    ddog_live_debugger_t* debugger;
    ddog_live_debugger_init(&debugger, /* config */);
    
    // Probes are configured remotely
    // and applied automatically
    
    // Cleanup
    ddog_live_debugger_shutdown(debugger);
    return 0;
}
```

