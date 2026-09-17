# libdd-data-pipeline-ffi

C FFI bindings for the libdd-data-pipeline library.

## Overview

`libdd-data-pipeline-ffi` provides C-compatible FFI bindings for `libdd-data-pipeline`, enabling high-performance trace processing from C, C++, PHP, Ruby, Python, and other languages.

## Dependencies

This crate depends on `tokio-util` for its `CancellationToken` type. The cancellation token created by `ddog_trace_exporter_cancel_token_new` and passed to `ddog_trace_exporter_send_trace_chunks` is a `tokio_util::sync::CancellationToken`, which the data pipeline uses to cooperatively abort an in-flight send. The token is exposed opaquely to C, so callers never need to depend on `tokio-util` themselves.
