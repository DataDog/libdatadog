# libdd-library-config

Library configuration management for Datadog products.

## Overview

`libdd-library-config` provides utilities for managing library configuration from environment variables, files, and remote sources.

## Features

- **Environment Variables**: Read configuration from env vars
- **Configuration Files**: Parse JSON and YAML config files
- **Validation**: Validate configuration values
- **Defaults**: Provide sensible defaults
- **Type Safety**: Strongly-typed configuration structs
- **Remote Config**: Integration with remote configuration

## Configuration Sources

Priority order (highest to lowest):
1. Environment variables
2. Configuration file
3. Remote configuration
4. Defaults

## Example Usage

```rust
use libdd_library_config;

// Load configuration
// let config = LibraryConfig::from_env()?;

// Or from file
// let config = LibraryConfig::from_file("config.json")?;

// Access configuration
// let service_name = config.service_name();
```

