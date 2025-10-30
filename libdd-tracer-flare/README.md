# libdd-tracer-flare

Tracer flare functionality for diagnostic data collection.

## Overview

`libdd-tracer-flare` provides utilities for collecting diagnostic information from tracer instances to help debug configuration and connectivity issues.

## Features

- **Diagnostic Collection**: Gather tracer configuration and state
- **Flare Generation**: Create diagnostic bundles for support
- **Configuration Snapshot**: Capture current tracer settings
- **Remote Config Integration**: Include remote configuration state
- **Privacy-Safe**: Automatically obfuscate sensitive information

## What is a Tracer Flare?

A tracer flare is a diagnostic bundle that contains:
- Tracer configuration
- Recent logs and errors
- Connection status
- Remote configuration state
- System information
- Agent connectivity details

This information helps Datadog support diagnose tracer issues.

## Features Flags

- `default`: Standard flare functionality
- `listener`: Enable flare listener for remote triggering

## Example Usage

```rust
use libdd_tracer_flare;

// Generate a flare
// let flare = tracer_flare::generate();
// Send to support or save locally
```

