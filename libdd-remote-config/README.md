# libdd-remote-config

Remote configuration client for Datadog products.

## Overview

`libdd-remote-config` provides a client for receiving and applying configuration updates from Datadog's remote configuration service without restarting applications.

## Features

- **Real-time Configuration**: Receive config updates in real-time
- **Polling**: Periodic polling for configuration changes
- **Configuration Targets**: Target specific services, environments, or versions
- **Multiple Products**: Support for APM, ASM, and other products
- **Version Control**: Track configuration versions
- **Validation**: Validate configurations before applying
- **Rollback**: Handle configuration rollbacks
- **Caching**: Cache configurations locally

## Configuration Types

- Tracer configuration
- Security rules (ASM)
- Sampling rates
- Feature flags
- Library configuration
- Custom application config

## Example Usage

```rust
use libdd_remote_config;

// Create client
// let client = RemoteConfigClient::new(config)?;

// Start polling
// client.start_polling().await?;

// Receive configuration updates
// let updates = client.poll().await?;
// apply_config(updates);
```

## Features Flags

- `test`: Enable test utilities

