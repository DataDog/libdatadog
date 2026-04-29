# libdd-tracer-flare

Rust library for collecting and transmitting Datadog tracer diagnostic flares via remote configuration.

## Overview

This library collects diagnostic information from Datadog tracers, packages it into a zip archive, and sends it to the Datadog agent. It optionally supports remote configuration to automatically trigger flare collection and control log levels.

## Features

- `listener` (default) - Enables remote configuration support

## License

Apache-2.0
