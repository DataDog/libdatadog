# libdd-trace-utils

Utilities for handling distributed tracing spans, serialization, and data transport to Datadog APM.

## Overview

`libdd-trace-utils` provides essential utilities for distributed tracing including span processing, MessagePack encoding/decoding, payload handling, and HTTP transport with retry logic.

## Features

The opt-in `send` feature enables `send_data`, `send_with_retry`, the `trace_utils` sending re-exports, and `coalesce_send_data`.

```toml
libdd-trace-utils = { version = "16.0.0", features = ["send"] }
```

The default `https` feature configures TLS only when networking is enabled. Use `default-features = false` with `features = ["send"]` to supply a host transport without bundled TLS. `test-utils` implies `send`, while `mini_agent` independently enables networking.
