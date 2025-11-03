# libdd-ipc

Inter-Process Communication (IPC) library for Datadog products.

## Overview

`libdd-ipc` provides cross-platform IPC primitives and transport mechanisms for communication between processes. It includes support for Unix domain sockets, Windows named pipes, and shared memory, with built-in serialization and rate limiting.

## Features

- **Cross-platform transport**: Unix domain sockets and Windows named pipes
- **Async/await support**: Built on top of Tokio
- **tarpc integration**: RPC-style communication with type-safe interfaces
- **Rate limiting**: Built-in rate limiter for controlling message flow
- **Platform-specific optimizations**: Native platform support for handles and file descriptors
- **Sequential messaging**: Support for sequential, ordered communication
- **Example interface**: Reference implementation for building IPC services

## Modules

- `platform`: Platform-specific IPC implementations (Unix/Windows)
- `transport`: Transport layer abstractions and implementations
- `handles`: Cross-platform handle management  
- `rate_limiter`: Message rate limiting utilities
- `sequential`: Sequential/ordered messaging support
- `example_interface`: Example IPC interface implementation

