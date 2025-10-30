# libdd-library-config-ffi

C FFI bindings for libdd-library-config.

## Overview

`libdd-library-config-ffi` provides C-compatible FFI bindings for library configuration management.

## Features

- **C API**: Complete C bindings for configuration
- **Environment Loading**: Load config from environment
- **File Parsing**: Parse configuration files
- **Remote Config**: Query remote configuration
- **Type Conversion**: Safe type conversions across FFI

## Example Usage

```c
#include <datadog/library_config.h>

int main() {
    // Load configuration
    ddog_library_config_t* config;
    ddog_library_config_from_env(&config);
    
    // Query configuration
    const char* service = ddog_library_config_service_name(config);
    
    // Cleanup
    ddog_library_config_free(config);
    return 0;
}
```

