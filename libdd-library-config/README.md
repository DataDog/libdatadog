# libdd-library-config

Configuration management library for Datadog products.

## Overview

`libdd-library-config` provides functionality for reading and managing configuration settings from multiple sources with defined precedence rules.

## Features

- **Multiple Configuration Sources**: Support for local and managed configuration files
- **Rule-Based Matching**: Apply different configurations based on process characteristics
- **Priority Handling**: Higher-priority sources override lower-priority ones
- **Template Support**: Dynamic value substitution in configuration strings
- **Type Safety**: Strongly-typed configuration structures

## Tracer metadata without `std`

Depend on `libdd-library-config` with `default-features = false` to use
`tracer_metadata::TracerMetadata` with `core` and `alloc` only.

On Linux, hostname and origin discovery use rustix directly for system calls. Environment
reads use the process-wide shadow environment in `libdd-common`, re-exported as
`tracer_metadata::discovery::environment`.

Initialize the shadow once during single-threaded startup, before metadata discovery:

```rust
use libdd_library_config::tracer_metadata::discovery::environment;

// SAFETY: startup runs before any threads or native libraries can mutate the environment.
unsafe { environment::initialize()?; }
# Ok::<(), environment::InitError>(())
```

Initialization copies every process variable into owned, immutable storage. Later process
environment changes do not affect reads, and missing snapshot keys never fall back to the
system environment. Before initialization, `std` builds use the process environment and
`no_std` builds report absent variables. Alternatively, `environment::initialize_from`
accepts caller-supplied UTF-8 pairs safely, and `initialize_from_envp` copies a stable C
`envp` array, preserving non-UTF-8 values.

```rust
use libdd_library_config::tracer_metadata::{
    TracerMetadata, discovery::{self, EntityInfo},
};

#[cfg(target_os = "linux")]
fn collect_metadata() -> anyhow::Result<(TracerMetadata, EntityInfo)> {
    let origin = discovery::discover_origin();
    let metadata = TracerMetadata {
        tracer_language: "c".into(),
        hostname: discovery::read_hostname()?,
        container_id: origin.container_id.clone(),
        ..Default::default()
    };
    Ok((metadata, origin))
}
```

`read_hostname()` uses rustix's `uname` syscall to get the current UTS namespace's hostname,
including inside containers. `read_container_id()` reads `/proc/self/cgroup` using rustix,
returning `None` for a missing file or an unrecognized container ID.

`discover_origin()` uses libdatadog's shared origin-discovery logic:

- Read the container ID from `/proc/self/cgroup`.
- Prefer `ci-<container-id>` for the entity ID. Otherwise use `in-<cgroup-inode>`, selecting
  the v1 `memory` controller or v2 unified hierarchy under `/sys/fs/cgroup`. Suppress the
  inode fallback when `/proc/self/ns/cgroup` identifies the host cgroup namespace.
- Read nonempty `DD_EXTERNAL_ENV` through the shared environment API. `DD_ENTITY_ID` is not
  used as a local entity-ID override.

Origin discovery is uncached and best effort: I/O failures omit the affected IDs, while
`DD_EXTERNAL_ENV` remains independently available. `origin.headers()` borrows the available
`datadog-container-id`, `datadog-entity-id`, and `datadog-external-env` values. These headers
are separate from the legacy tracer metadata payload. The pure `parse_container_id` helper
can parse already-read cgroup contents on any platform.

Runtime UUID, tracer language/version, resolved service configuration, and process tags
remain caller-supplied metadata fields. `metadata.to_msgpack()` encodes the legacy payload
without `std`, and the type also implements `serde::Serialize`. Memfd storage, OTel
conversion, and process-context publishing require `std`.
