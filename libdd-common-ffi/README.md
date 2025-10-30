# libdd-common-ffi

C FFI bindings for libdd-common shared utilities.

## Overview

`libdd-common-ffi` provides C-compatible FFI bindings for `libdd-common`, exposing common utilities like tags, endpoints, and error handling to C and other languages.

## Features

- **Tag Management**: C API for creating and managing tags
- **Endpoint Configuration**: Configure HTTP endpoints
- **Error Handling**: C-compatible error types
- **String Utilities**: Safe string handling across FFI
- **Vec Types**: C-compatible vector types
- **Slice Types**: Safe slice handling
- **Memory Management**: RAII-style memory management for C

## API Types

- `ddog_Vec_Tag`: Vector of tags
- `ddog_Endpoint`: HTTP endpoint configuration
- `ddog_Error`: Error type
- `CharSlice`: String slice for FFI
- Tag creation and manipulation functions

## Example Usage

```c
#include <datadog/common.h>

int main() {
    // Create tags
    ddog_Vec_Tag tags = ddog_Vec_Tag_new();
    ddog_Vec_Tag_push(&tags, 
        ddog_CharSlice_from_str("env"),
        ddog_CharSlice_from_str("prod"));
    
    // Use tags...
    
    // Cleanup
    ddog_Vec_Tag_drop(tags);
    return 0;
}
```

