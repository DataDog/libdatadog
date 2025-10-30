# libdd-sketch-ffi

C FFI bindings for libdd-sketch DDSketch implementation.

## Overview

`libdd-sketch-ffi` provides C-compatible FFI bindings for `libdd-sketch`, allowing DDSketch usage from C, C++, and other languages for accurate quantile estimation.

## Features

- **C API**: Complete C bindings for DDSketch
- **Sketch Creation**: Create sketches with configurable accuracy
- **Value Addition**: Add values to sketches
- **Quantile Queries**: Query percentiles (p50, p95, p99, etc.)
- **Sketch Merging**: Combine multiple sketches
- **Serialization**: Serialize sketches to protobuf
- **Memory Safety**: Safe memory management

## Example Usage

```c
#include <datadog/ddsketch.h>

int main() {
    // Create sketch with 2% relative error
    ddog_ddsketch_t* sketch = ddog_ddsketch_new(0.02);
    
    // Add values
    ddog_ddsketch_add(sketch, 42.0);
    ddog_ddsketch_add(sketch, 100.0);
    ddog_ddsketch_add(sketch, 250.0);
    
    // Query quantiles
    double p50 = ddog_ddsketch_quantile(sketch, 0.5);
    double p99 = ddog_ddsketch_quantile(sketch, 0.99);
    
    // Cleanup
    ddog_ddsketch_free(sketch);
    return 0;
}
```

## Use Cases

- Latency monitoring from C/C++ applications
- Request size distributions
- Any metric requiring quantile tracking

