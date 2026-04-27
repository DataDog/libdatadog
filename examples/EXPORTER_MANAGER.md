# ExporterManager Examples

This directory contains examples demonstrating how to use the `ExporterManager` for asynchronous profile export with fork-safety support.

## Overview

The `ExporterManager` provides:
- **Asynchronous export**: Profiles are queued and sent via a background worker thread
- **Bounded channel**: Prevents unbounded memory growth
- **Fork-safe operations**: Proper handling of process forks
- **Thread safety**: Safe to use from multiple threads

## Examples

### C FFI Example: `ffi/exporter_manager.c`

Demonstrates using the ExporterManager through the C FFI API.

**Features shown:**
- Creating an `ExporterManager` with a background worker
- Queueing profiles for async sending
- Aborting the manager to stop the worker thread
- Fork-safe usage with `prefork`, `postfork_child`, and `postfork_parent`

**Building:**
```bash
cd examples/ffi/build
cmake ..
make exporter_manager
```

**Running:**
```bash
# With DD_API_KEY set (sends to Datadog):
DD_API_KEY=your_api_key ./exporter_manager my-service

# Without DD_API_KEY (writes to /tmp for demonstration):
./exporter_manager my-service
```

### C++ CXX Example: `cxx/exporter_manager.cpp`

Demonstrates using the ExporterManager through the C++ CXX bindings.

**Features shown:**
- Creating an `ExporterManager` with type-safe C++ API
- Queueing profiles for async sending
- Aborting the manager
- Fork-safe usage with proper cleanup
- Different profiles in parent and child processes

**Building:**
```bash
cd examples/cxx
./build-exporter-manager.sh
```

**Running:**
```bash
# The script builds and runs the example automatically
# Or run directly after building:
DD_API_KEY=your_api_key ./exporter_manager my-service
```

## Fork-Safety Pattern

When using the `ExporterManager` in applications that fork:

### 1. Before Fork (Parent Process)

```c
// Call prefork to suspend the manager
ddog_prof_Handle_SuspendedExporterManager suspended = 
    ddog_prof_ExporterManager_prefork(&manager);
```

**What happens:**
- Background worker thread is stopped
- Worker thread is joined (no zombie threads)
- Inflight messages are captured
- Sender/receiver are disconnected

### 2. After Fork (Child Process)

```c
// Child gets a clean manager, inflight requests discarded
ddog_prof_Handle_ExporterManager child_manager = 
    ddog_prof_ExporterManager_postfork_child(&suspended);
```

**What happens:**
- New manager created with fresh worker thread
- Inflight requests from parent are discarded
- Child can profile independently

### 3. After Fork (Parent Process)

```c
// Parent gets a manager with inflight requests re-queued
ddog_prof_Handle_ExporterManager parent_manager = 
    ddog_prof_ExporterManager_postfork_parent(&suspended);
```

**What happens:**
- New manager created with fresh worker thread
- Inflight requests are re-queued
- No data loss from the fork

## API Comparison

### C FFI API

```c
// Create manager
ddog_prof_Result_HandleExporterManager result = 
    ddog_prof_ExporterManager_new(exporter);
ddog_prof_Handle_ExporterManager manager = result.ok;

// Queue a profile
ddog_prof_Result_Void queue_result = ddog_prof_ExporterManager_queue(
    &manager,
    encoded_profile,
    ddog_prof_Exporter_Slice_File_empty(),
    NULL,  // additional_tags
    NULL,  // process_tags
    NULL,  // internal_metadata
    NULL   // info
);

// Abort
ddog_prof_Result_HandleSuspendedExporterManager abort_result = 
    ddog_prof_ExporterManager_abort(&manager);
ddog_prof_Handle_SuspendedExporterManager suspended = abort_result.ok;

// Cleanup
ddog_prof_SuspendedExporterManager_drop(&suspended);
```

### C++ CXX API

```cpp
// Create manager
auto manager = new_manager(std::move(exporter));

// Queue a profile
manager->queue_profile(
    *profile,
    {},    // files_to_compress
    {},    // additional_tags
    "",    // process_tags
    "",    // internal_metadata
    ""     // info
);

// Abort
auto suspended = manager->abort();

// Cleanup is automatic via RAII
```

## Important Notes

### Profile Reset Behavior

**Important:** When you call `queue_profile()`, the profile is **reset** and the **previous** profile data is queued for sending. This allows continuous profiling:

```c
// Add samples to profile
add_samples_to_profile(profile);

// Queue sends the PREVIOUS data and resets for new samples
manager->queue_profile(profile, ...);

// Immediately start adding new samples
add_more_samples_to_profile(profile);
```

### Channel Capacity

The ExporterManager uses a bounded channel with capacity of 2:
- Prevents unbounded memory growth
- `queue()` will return an error if the channel is full
- Worker thread processes messages asynchronously

### Thread Safety

- The `ExporterManager` is thread-safe for queueing
- Multiple threads can safely call `queue_profile()`
- The background worker processes one request at a time

### Error Handling

Always check return values:

```c
// C FFI
if (result.tag != DDOG_PROF_RESULT_HANDLE_EXPORTER_MANAGER_OK_HANDLE_EXPORTER_MANAGER) {
    print_error("Failed", &result.err);
    ddog_Error_drop(&result.err);
    return 1;
}
```

```cpp
// C++ CXX - uses exceptions
try {
    auto manager = new_manager(std::move(exporter));
    manager->queue_profile(*profile, {}, {}, "", "", "");
} catch (const std::exception& e) {
    std::cerr << "Error: " << e.what() << std::endl;
}
```

## See Also

- [C FFI Exporter Example](ffi/exporter.cpp) - Basic ProfileExporter usage
- [C++ CXX Profiling Example](cxx/profiling.cpp) - Full profiling workflow
- [API Documentation](../README.md) - Full libdatadog documentation

