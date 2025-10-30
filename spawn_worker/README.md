# spawn_worker

Utilities for spawning worker processes.

## Overview

`spawn_worker` provides cross-platform utilities for spawning and managing worker processes, particularly for the Datadog sidecar and IPC.

## Features

- **Process Spawning**: Cross-platform process creation
- **IPC Setup**: Set up inter-process communication channels
- **Error Handling**: Robust error handling for process failures
- **Platform Support**: Unix (fork/exec) and Windows support
- **Resource Cleanup**: Proper cleanup of resources
- **Signal Handling**: Handle process signals correctly

## Use Cases

- Spawn sidecar processes
- Create telemetry workers
- Fork crash reporter processes
- Isolate potentially crashy code

## Platform-Specific Behavior

### Unix
- Uses `fork()` and `exec()` for process creation
- Proper file descriptor handling
- Signal mask management

### Windows
- Uses `CreateProcess` API
- Handle inheritance management
- Process group management

